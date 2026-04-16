//! Working notes store — persistent, searchable scratchpad for agents.
//!
//! Notes survive across sessions and are retrieved by full-text search,
//! symbol name, file path, or kind filter. Unlike test/lint stores (which
//! are overwritten on every run), notes are only deleted explicitly via
//! [`NoteStore::delete_note`].
//!
//! ## Schema
//!
//! - **`notes`** — one row per note with JSON arrays for `symbols` and `files`.
//! - **`notes_fts`** — FTS5 virtual table backed by `notes`, kept in sync by
//!   three triggers (after insert, before delete, after update).
//!
//! ## Threading
//!
//! `NoteStore` wraps a synchronous `rusqlite::Connection` in a
//! `tokio::sync::Mutex`. All operations are microsecond-fast; no
//! `spawn_blocking` is needed.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::error::{Error, Result};

// ─── Types ────────────────────────────────────────────────────────────────────

/// A single note row returned from [`NoteStore::read_notes`].
#[derive(Debug, Clone)]
pub struct NoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    /// RFC 3339 timestamp string.
    pub created_at: String,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite + FTS5 store for agent working notes.
pub struct NoteStore {
    conn: Arc<Mutex<Connection>>,
}

impl NoteStore {
    /// Open or create the database at `db_path`. Schema migrations are
    /// idempotent — safe to call on an existing database.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let conn = Connection::open(db_path).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "NoteStore::open {}: {e}",
                db_path.display()
            )))
        })?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::Io(std::io::Error::other(format!("NoteStore schema: {e}"))))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Persist a new note. Returns the generated note ID.
    ///
    /// `kind` must be one of `"decision"`, `"attempt"`, `"invariant"`, `"todo"`.
    /// `symbols` and `files` are associated symbol names / file paths for
    /// filtered retrieval.
    pub async fn write_note(
        &self,
        kind: &str,
        content: &str,
        symbols: Vec<String>,
        files: Vec<String>,
        session_id: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = unix_now();
        let symbols_json = serde_json::to_string(&symbols).unwrap_or_else(|_| "[]".to_string());
        let files_json = serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string());

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, kind, content, symbols, files, session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, kind, content, symbols_json, files_json, session_id, now],
        )
        .map_err(sqlite_err)?;

        Ok(id)
    }

    /// Query notes. Filters compose with AND.
    ///
    /// - `query` — FTS5 full-text search, ordered by BM25 relevance.
    ///   When `None`, results are ordered by recency (newest first).
    /// - `symbols` — retain notes that mention any of these symbol names.
    /// - `files` — retain notes that mention any of these file paths.
    /// - `kinds` — retain notes whose `kind` is in this list.
    /// - `limit` — maximum number of results (capped at 100 internally).
    pub async fn read_notes(
        &self,
        query: Option<&str>,
        symbols: &[String],
        files: &[String],
        kinds: &[String],
        limit: usize,
    ) -> Result<Vec<NoteRow>> {
        let cap = limit.min(100);
        // Over-fetch when FTS is active to leave room for post-filtering.
        let fetch_limit = if query.is_some() { cap * 10 } else { cap };

        let rows: Vec<NoteRow> = {
            let conn = self.conn.lock().await;
            if let Some(q) = query.filter(|s| !s.is_empty()) {
                // FTS5 path — BM25 relevance order.
                let sql = format!(
                    "WITH ranked AS (
                        SELECT rowid, bm25(notes_fts) AS rank
                        FROM notes_fts
                        WHERE notes_fts MATCH ?
                        LIMIT {fetch_limit}
                    )
                    SELECT n.id, n.kind, n.content, n.symbols, n.files, n.session_id, n.created_at
                    FROM notes n
                    JOIN ranked r ON r.rowid = n.rowid
                    ORDER BY r.rank"
                );
                let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
                let mapped = stmt.query_map(params![q], map_note_row).map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            } else {
                // Recency path.
                let mut stmt = conn
                    .prepare(
                        "SELECT id, kind, content, symbols, files, session_id, created_at
                         FROM notes
                         ORDER BY created_at DESC
                         LIMIT ?",
                    )
                    .map_err(sqlite_err)?;
                let mapped = stmt
                    .query_map(params![fetch_limit as i64], map_note_row)
                    .map_err(sqlite_err)?;
                let mut out = Vec::new();
                for row in mapped {
                    out.push(row.map_err(sqlite_err)?);
                }
                out
            }
        };

        // Post-filter: kinds, symbols, files (conn lock released above).
        let mut out: Vec<NoteRow> = rows
            .into_iter()
            .filter(|n| kinds.is_empty() || kinds.iter().any(|k| k == &n.kind))
            .filter(|n| symbols.is_empty() || symbols.iter().any(|s| n.symbols.contains(s)))
            .filter(|n| files.is_empty() || files.iter().any(|f| n.files.contains(f)))
            .collect();

        out.truncate(cap);
        Ok(out)
    }

    /// Delete a note by ID. Returns `true` if a row was removed, `false` if
    /// the ID was not found.
    pub async fn delete_note(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute("DELETE FROM notes WHERE id = ?", params![id])
            .map_err(sqlite_err)?;
        Ok(affected > 0)
    }

    /// Return the most recent open `todo` notes (for the startup summary).
    pub async fn open_todos(&self, limit: usize) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, content, symbols, files, session_id, created_at
                 FROM notes
                 WHERE kind = 'todo'
                 ORDER BY created_at DESC
                 LIMIT ?",
            )
            .map_err(sqlite_err)?;
        let mapped = stmt
            .query_map(params![limit as i64], map_note_row)
            .map_err(sqlite_err)?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row.map_err(sqlite_err)?);
        }
        Ok(out)
    }
}

// ─── Schema ───────────────────────────────────────────────────────────────────

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS notes (
    id         TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL CHECK(kind IN ('decision','attempt','invariant','todo')),
    content    TEXT    NOT NULL,
    symbols    TEXT    NOT NULL DEFAULT '[]',
    files      TEXT    NOT NULL DEFAULT '[]',
    session_id TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_kind ON notes(kind);
CREATE INDEX IF NOT EXISTS idx_notes_created ON notes(created_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    content, kind,
    content='notes',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS notes_fts_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_ad BEFORE DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
END;

CREATE TRIGGER IF NOT EXISTS notes_fts_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, content, kind)
    VALUES ('delete', old.rowid, old.content, old.kind);
    INSERT INTO notes_fts(rowid, content, kind) VALUES (new.rowid, new.content, new.kind);
END;
";

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Io(std::io::Error::other(format!("NoteStore sqlite: {e}")))
}

fn map_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRow> {
    let symbols_json: String = row.get(3)?;
    let files_json: String = row.get(4)?;
    let created_at_secs: i64 = row.get(6)?;

    let symbols: Vec<String> =
        serde_json::from_str(&symbols_json).unwrap_or_default();
    let files: Vec<String> =
        serde_json::from_str(&files_json).unwrap_or_default();

    // Convert unix timestamp to RFC 3339.
    let created_at = chrono::DateTime::<chrono::Utc>::from_timestamp(created_at_secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| created_at_secs.to_string());

    Ok(NoteRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        symbols,
        files,
        session_id: row.get(5)?,
        created_at,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_store() -> NoteStore {
        let dir = tempfile::tempdir().unwrap();
        NoteStore::open(&dir.path().join("notes.db")).unwrap()
    }

    #[tokio::test]
    async fn write_note_roundtrip() {
        let store = make_store().await;
        let id = store
            .write_note(
                "decision",
                "Use BFS for blast radius",
                vec!["blast_radius".into()],
                vec!["src/lib.rs".into()],
                "test-session",
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let notes = store
            .read_notes(None, &[], &[], &[], 10)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "decision");
        assert_eq!(notes[0].content, "Use BFS for blast radius");
        assert_eq!(notes[0].symbols, vec!["blast_radius"]);
    }

    #[tokio::test]
    async fn read_notes_fts_search() {
        let store = make_store().await;
        store
            .write_note("decision", "Use BFS for blast radius traversal", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "Unrelated note about caching", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(Some("blast radius"), &[], &[], &[], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("blast radius"));
    }

    #[tokio::test]
    async fn read_notes_symbol_filter() {
        let store = make_store().await;
        store
            .write_note("attempt", "tried foo", vec!["foo_fn".into()], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("attempt", "tried bar", vec!["bar_fn".into()], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &["foo_fn".to_string()], &[], &[], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("foo"));
    }

    #[tokio::test]
    async fn read_notes_kind_filter() {
        let store = make_store().await;
        store
            .write_note("decision", "keep this", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("todo", "do this later", vec![], vec![], "s1")
            .await
            .unwrap();

        let results = store
            .read_notes(None, &[], &[], &["todo".to_string()], 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "todo");
    }

    #[tokio::test]
    async fn delete_note_removes() {
        let store = make_store().await;
        let id = store
            .write_note("invariant", "never call this twice", vec![], vec![], "s1")
            .await
            .unwrap();

        let deleted = store.delete_note(&id).await.unwrap();
        assert!(deleted);

        let notes = store.read_notes(None, &[], &[], &[], 10).await.unwrap();
        assert!(notes.is_empty());

        // Deleting again returns false.
        let deleted_again = store.delete_note(&id).await.unwrap();
        assert!(!deleted_again);
    }

    #[tokio::test]
    async fn open_todos_returns_only_todos() {
        let store = make_store().await;
        store
            .write_note("todo", "fix the thing", vec![], vec![], "s1")
            .await
            .unwrap();
        store
            .write_note("decision", "keep it", vec![], vec![], "s1")
            .await
            .unwrap();

        let todos = store.open_todos(10).await.unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "fix the thing");
    }
}
