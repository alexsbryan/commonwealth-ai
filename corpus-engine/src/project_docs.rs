//! Project documentation store — indexes `*.md` files for FTS5 retrieval.
//!
//! Used by the `project_context` MCP tool to answer questions like "what
//! are the conventions for error handling?" or "what does this architecture
//! diagram mean?" by searching indexed project docs.
//!
//! ## Design
//!
//! SQLite FTS5 only — no LanceDB, no vectors. The MCP server uses a
//! zero-vector embed function (`vec![0.0; 768]`) which makes cosine
//! similarity meaningless. BM25 keyword ranking is the right retrieval
//! here and it requires no embedding model.
//!
//! ## Chunking
//!
//! Markdown files are split at `#`, `##`, `###` headings. Each section
//! becomes one chunk stored as `source = "filename §heading"`. Files with
//! no headings are split into fixed 1 000-char chunks with 100-char overlap.
//!
//! ## Indexing lifecycle
//!
//! - Initial indexing is triggered by [`ProjectContextTool`] on first
//!   startup (async, does not block the server).
//! - Subsequent updates come from [`ProjectIndexWatcher`] on file changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Types ────────────────────────────────────────────────────────────────────

/// A single doc chunk returned by [`ProjectDocsStore::search`].
#[derive(Debug, Clone)]
pub struct DocResult {
    /// Human-readable source label: `"ARCHITECTURE.md §4.1"` or `"README.md"`.
    pub source: String,
    /// Repo-relative file path, e.g. `"ARCHITECTURE.md"`.
    pub file_path: String,
    /// The chunk content.
    pub content: String,
    /// Normalised relevance in [0, 1]. Higher is better.
    pub relevance: f32,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite + FTS5 store for project documentation chunks.
pub struct ProjectDocsStore {
    conn: Arc<Mutex<Connection>>,
}

impl ProjectDocsStore {
    /// Open or create the database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "ProjectDocsStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Io(std::io::Error::other(format!("ProjectDocsStore schema: {e}"))))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Index a single file. Deletes existing chunks for that file, then
    /// inserts fresh ones. Returns the number of chunks inserted.
    pub async fn index_file(&self, path: &Path, repo_root: &Path) -> Result<usize> {
        let text = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Ok(0), // Skip unreadable files silently.
        };

        let rel_path = path
            .strip_prefix(repo_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| rel_path.clone());

        let chunks = split_markdown_into_chunks(&filename, &text);
        if chunks.is_empty() {
            return Ok(0);
        }

        let now = unix_now();
        let conn = self.conn.lock().await;

        // Delete old chunks for this file.
        conn.execute("DELETE FROM docs WHERE file_path = ?", params![rel_path])
            .map_err(sqlite_err)?;

        for (source, content) in &chunks {
            conn.execute(
                "INSERT INTO docs (source, file_path, content, indexed_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![source, rel_path, content, now],
            )
            .map_err(sqlite_err)?;
        }

        Ok(chunks.len())
    }

    /// Remove all chunks for a file (called when the file is deleted).
    pub async fn delete_file(&self, file_path: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM docs WHERE file_path = ?", params![file_path])
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// Full-text search over indexed documentation. Returns up to `limit`
    /// results ordered by BM25 relevance (best first).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<DocResult>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }

        let conn = self.conn.lock().await;
        let sql = "
            WITH ranked AS (
                SELECT rowid, bm25(docs_fts) AS rank
                FROM docs_fts
                WHERE docs_fts MATCH ?
                LIMIT ?
            )
            SELECT d.source, d.file_path, d.content, r.rank
            FROM docs d
            JOIN ranked r ON r.rowid = d.id
            ORDER BY r.rank";

        let mut stmt = conn.prepare(sql).map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![query, limit as i64], |row| {
                let bm25: f64 = row.get(3)?;
                Ok(DocResult {
                    source: row.get(0)?,
                    file_path: row.get(1)?,
                    content: row.get(2)?,
                    // BM25 is negative; normalise to [0,1].
                    relevance: normalise_bm25(bm25),
                })
            })
            .map_err(sqlite_err)?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }

    /// Returns true if no documents have been indexed yet.
    pub async fn is_empty(&self) -> Result<bool> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0))
            .map_err(sqlite_err)?;
        Ok(count == 0)
    }

    /// Number of distinct files currently indexed.
    pub async fn file_count(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT file_path) FROM docs",
                [],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?;
        Ok(count as usize)
    }
}

// ─── Schema ───────────────────────────────────────────────────────────────────

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS docs (
    id         INTEGER PRIMARY KEY,
    source     TEXT    NOT NULL,
    file_path  TEXT    NOT NULL,
    content    TEXT    NOT NULL,
    indexed_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_docs_file ON docs(file_path);

CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
    content, source,
    content='docs',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS docs_fts_ai AFTER INSERT ON docs BEGIN
    INSERT INTO docs_fts(rowid, content, source) VALUES (new.id, new.content, new.source);
END;

CREATE TRIGGER IF NOT EXISTS docs_fts_ad BEFORE DELETE ON docs BEGIN
    INSERT INTO docs_fts(docs_fts, rowid, content, source)
    VALUES ('delete', old.id, old.content, old.source);
END;

CREATE TRIGGER IF NOT EXISTS docs_fts_au AFTER UPDATE ON docs BEGIN
    INSERT INTO docs_fts(docs_fts, rowid, content, source)
    VALUES ('delete', old.id, old.content, old.source);
    INSERT INTO docs_fts(rowid, content, source) VALUES (new.id, new.content, new.source);
END;
";

// ─── Markdown chunking ────────────────────────────────────────────────────────

/// Split a markdown document into `(source_label, content)` chunks.
///
/// Splits at `# `, `## `, `### ` headings. Each section becomes one chunk
/// labelled `"filename §heading"`. Files with no headings are split into
/// fixed 1 000-char chunks with 100-char overlap.
fn split_markdown_into_chunks(filename: &str, text: &str) -> Vec<(String, String)> {
    // Split on heading lines.
    // Each section: (heading_text, line_range_start)
    let mut section_starts: Vec<(String, usize)> = Vec::new();

    let lines: Vec<&str> = text.lines().collect();
    let mut has_headings = false;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("# ") || line.starts_with("## ") || line.starts_with("### ") {
            let heading = line.trim_start_matches('#').trim().to_string();
            section_starts.push((heading, i));
            has_headings = true;
        }
    }

    if has_headings {
        let mut chunks: Vec<(String, String)> = Vec::new();
        for (idx, (heading, start)) in section_starts.iter().enumerate() {
            let end = section_starts
                .get(idx + 1)
                .map(|(_, s)| *s)
                .unwrap_or(lines.len());
            let body = lines[*start..end].join("\n");
            if body.trim().is_empty() {
                continue;
            }
            let source = if heading.is_empty() {
                filename.to_string()
            } else {
                format!("{filename} \u{00a7}{heading}")
            };
            chunks.push((source, body.trim().to_string()));
        }
        return chunks;
    }

    // No headings — fall back to fixed-size chunks.
    fixed_size_chunks(filename, text, 1000, 100)
}

fn fixed_size_chunks(
    filename: &str,
    text: &str,
    chunk_size: usize,
    overlap: usize,
) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    if total == 0 {
        return vec![];
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut part = 1usize;

    while start < total {
        let end = (start + chunk_size).min(total);
        let content: String = chars[start..end].iter().collect();
        let source = format!("{filename} (part {part})");
        chunks.push((source, content.trim().to_string()));
        if end == total {
            break;
        }
        start = end.saturating_sub(overlap);
        part += 1;
    }
    chunks
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Walk `repo_root` recursively and return all `*.md` file paths.
/// Skips `target/`, `.git/`, `node_modules/`, and hidden directories.
pub fn find_markdown_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_for_markdown(repo_root, &mut out);
    out
}

fn walk_for_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Skip hidden dirs and known build/VCS artifacts.
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") {
            continue;
        }

        if path.is_dir() {
            walk_for_markdown(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("ProjectDocsStore sqlite: {e}")))
}

fn normalise_bm25(bm25: f64) -> f32 {
    // BM25 is negative; more negative = better match.
    // Map [-20, 0] → [1, 0] with a clamp.
    let normalised = (-bm25 / 20.0).clamp(0.0, 1.0);
    normalised as f32
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> ProjectDocsStore {
        let dir = tempfile::tempdir().unwrap();
        ProjectDocsStore::open(&dir.path().join("docs.db")).unwrap()
    }

    #[tokio::test]
    async fn project_docs_indexes_md() {
        let store = make_store().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("README.md");
        std::fs::write(
            &path,
            "# Overview\n\nThis is the overview.\n\n## Details\n\nMore detail here.",
        )
        .unwrap();

        let count = store.index_file(&path, dir.path()).await.unwrap();
        assert!(count >= 1);
        assert!(!store.is_empty().await.unwrap());
    }

    #[tokio::test]
    async fn project_docs_fts_search() {
        let store = make_store().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ARCH.md");
        std::fs::write(
            &path,
            "# Architecture\n\nThe system uses BFS for graph traversal.\n\n## Error Handling\n\nAll errors are returned via Result.",
        )
        .unwrap();
        store.index_file(&path, dir.path()).await.unwrap();

        let results = store.search("BFS graph traversal", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("BFS"));
    }

    #[test]
    fn markdown_chunking_by_headings() {
        let text = "# Overview\nIntro text.\n## Details\nDetail text.";
        let chunks = split_markdown_into_chunks("test.md", text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].0.contains("Overview"));
        assert!(chunks[1].0.contains("Details"));
    }

    #[test]
    fn fixed_chunking_no_headings() {
        let text = "x".repeat(2500);
        let chunks = split_markdown_into_chunks("flat.md", &text);
        assert!(chunks.len() >= 2);
    }
}
