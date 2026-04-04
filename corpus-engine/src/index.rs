//! CorpusIndex — wraps a single SQLite connection to a per-corpus database
//! with sqlite-vec for vector search and FTS5 for keyword search.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{ffi::sqlite3_auto_extension, params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::types::{ChunkRange, IndexInfo, ScoredChunk};

// ─── Helper types ──────────────────────────────────────────

/// A chunk to be inserted into the index.
pub struct InsertChunk {
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub metadata: Option<String>, // JSON string
}

// ─── CorpusIndex ───────────────────────────────────────────

/// A single corpus index backed by SQLite + FTS5 + sqlite-vec.
pub struct CorpusIndex {
    db: Connection,
    has_vec: bool,
}

impl CorpusIndex {
    // ── Construction ───────────────────────────────────────

    /// Create a new index database at `path`.
    pub fn create(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        license: &str,
    ) -> Result<Self> {
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;

        let has_vec = try_load_vec(&db);

        // Core tables.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS corpus_meta (
                key   TEXT PRIMARY KEY,
                value TEXT
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id        INTEGER PRIMARY KEY,
                content   TEXT NOT NULL,
                title     TEXT,
                url       TEXT,
                embedding BLOB NOT NULL,
                metadata  TEXT,
                CONSTRAINT content_not_empty CHECK(length(content) > 0)
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                content, title, content=chunks, content_rowid=id
            );",
        )?;

        // sqlite-vec virtual table.
        if has_vec {
            db.execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_vec USING vec0(embedding float[{embedding_dim}]);"
            ))?;
        }

        // Populate corpus_meta.
        let now = now_unix();
        let dim_str = embedding_dim.to_string();
        let now_str = now.to_string();
        let meta_pairs: &[(&str, &str)] = &[
            ("corpus_id", corpus_id),
            ("corpus_name", corpus_name),
            ("embedding_model", embedding_model),
            ("embedding_dimensions", &dim_str),
            ("created_at", &now_str),
            ("last_updated", &now_str),
            ("mesh_sharing", if mesh_sharing { "true" } else { "false" }),
            ("license", license),
        ];

        {
            let mut stmt =
                db.prepare("INSERT INTO corpus_meta(key, value) VALUES (?, ?)")?;
            for (k, v) in meta_pairs {
                stmt.execute(params![k, v])?;
            }
        }

        Ok(Self { db, has_vec })
    }

    /// Open an existing index database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::IndexNotFound(path.display().to_string()));
        }
        let db = Connection::open(path)?;
        let has_vec = try_load_vec(&db);

        // Verify corpus_meta exists by reading a row.
        db.query_row(
            "SELECT value FROM corpus_meta WHERE key = 'corpus_id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| {
            Error::IndexNotFound(format!(
                "corpus_meta table missing or empty in {}",
                path.display()
            ))
        })?;

        Ok(Self { db, has_vec })
    }

    // ── Mutation ───────────────────────────────────────────

    /// Insert a batch of chunks (with pre-computed embeddings) into the index.
    pub fn insert_batch(&self, chunks: &[(InsertChunk, Vec<f32>)]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;

        {
            let mut ins_chunk = tx.prepare(
                "INSERT INTO chunks(content, title, url, embedding, metadata)
                 VALUES (?, ?, ?, ?, ?)",
            )?;
            let mut ins_fts = tx.prepare(
                "INSERT INTO chunks_fts(rowid, content, title) VALUES (?, ?, ?)",
            )?;
            let mut ins_vec = if self.has_vec {
                Some(tx.prepare(
                    "INSERT INTO chunks_vec(rowid, embedding) VALUES (?, ?)",
                )?)
            } else {
                None
            };

            for (chunk, embedding) in chunks {
                let blob = embedding_to_blob(embedding);

                let rowid = ins_chunk.insert(params![
                    chunk.content,
                    chunk.title,
                    chunk.url,
                    blob,
                    chunk.metadata,
                ])?;

                ins_fts.execute(params![rowid, chunk.content, chunk.title])?;

                if let Some(ref mut stmt) = ins_vec {
                    stmt.execute(params![rowid, blob])?;
                }
            }
        }

        // Update last_updated.
        tx.execute(
            "UPDATE corpus_meta SET value = ? WHERE key = 'last_updated'",
            params![now_unix().to_string()],
        )?;

        tx.commit()?;
        Ok(())
    }

    // ── Search ─────────────────────────────────────────────

    /// Hybrid search combining vector similarity and FTS5 keyword matching.
    ///
    /// Weights: 0.7 vector + 0.3 keyword.
    pub fn search(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        let corpus_id = self.meta_value("corpus_id")?;

        let do_vector = !query_embedding.is_empty();
        let sanitized = sanitize_fts_query(query_text);
        let do_fts = !sanitized.is_empty();

        // Gather candidate scores: rowid -> (vec_score, fts_score)
        let mut scores: HashMap<i64, (f32, f32)> = HashMap::new();

        // ── Vector search ──────────────────────────────────
        if do_vector {
            let vec_results = if self.has_vec {
                self.vec_search(query_embedding, limit)?
            } else {
                self.brute_force_search(query_embedding, limit)?
            };

            // Normalise distances into [0, 1] similarity scores.
            // sqlite-vec returns L2 distance; smaller is better.
            // Brute-force already returns cosine similarity; higher is better.
            if self.has_vec {
                // L2 distances — convert to a similarity-like score.
                let max_dist = vec_results
                    .iter()
                    .map(|&(_, d)| d)
                    .fold(f32::NEG_INFINITY, f32::max)
                    .max(1e-9);
                for (rowid, dist) in &vec_results {
                    let sim = 1.0 - dist / (max_dist + 1e-9);
                    scores.entry(*rowid).or_insert((0.0, 0.0)).0 = sim;
                }
            } else {
                for (rowid, sim) in &vec_results {
                    scores.entry(*rowid).or_insert((0.0, 0.0)).0 = *sim;
                }
            }
        }

        // ── FTS5 search ────────────────────────────────────
        if do_fts {
            let fts_results = self.fts_search(&sanitized, limit)?;

            // BM25 ranks are negative (more negative = better).
            let min_rank = fts_results
                .iter()
                .map(|&(_, r)| r)
                .fold(f32::INFINITY, f32::min)
                .min(-1e-9);
            for (rowid, rank) in &fts_results {
                // Map rank to [0, 1] where 1 = best match.
                let sim = rank / min_rank; // both negative → positive fraction
                scores.entry(*rowid).or_insert((0.0, 0.0)).1 = sim;
            }
        }

        // ── Combine and sort ───────────────────────────────
        let (w_vec, w_fts) = if do_vector && do_fts {
            (0.7_f32, 0.3_f32)
        } else if do_vector {
            (1.0, 0.0)
        } else {
            (0.0, 1.0)
        };

        let mut ranked: Vec<(i64, f32)> = scores
            .into_iter()
            .map(|(id, (vs, fs))| (id, w_vec * vs + w_fts * fs))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(limit);

        // ── Hydrate results ────────────────────────────────
        let mut out = Vec::with_capacity(ranked.len());
        let mut stmt = self.db.prepare(
            "SELECT content, title, url, metadata FROM chunks WHERE id = ?",
        )?;

        for (rowid, score) in &ranked {
            let row = stmt
                .query_row(params![rowid], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })
                .optional()?;

            if let Some((content, title, url, metadata_json)) = row {
                let metadata: HashMap<String, String> = metadata_json
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default();

                out.push(ScoredChunk {
                    content,
                    title,
                    url,
                    corpus_id: corpus_id.clone(),
                    score: *score,
                    metadata,
                });
            }
        }

        Ok(out)
    }

    // ── Info ───────────────────────────────────────────────

    /// Return metadata about this index.
    pub fn info(&self) -> Result<IndexInfo> {
        let corpus_id = self.meta_value("corpus_id")?;
        let corpus_name = self.meta_value("corpus_name")?;
        let embedding_model = self.meta_value("embedding_model")?;
        let embedding_dimensions: usize = self
            .meta_value("embedding_dimensions")?
            .parse()
            .map_err(|e| Error::Serialization(format!("bad embedding_dimensions: {e}")))?;
        let created_at: u64 = self
            .meta_value("created_at")?
            .parse()
            .map_err(|e| Error::Serialization(format!("bad created_at: {e}")))?;
        let last_updated: u64 = self
            .meta_value("last_updated")?
            .parse()
            .map_err(|e| Error::Serialization(format!("bad last_updated: {e}")))?;
        let mesh_sharing = self.meta_value("mesh_sharing")? == "true";

        let is_shard = self
            .meta_value_opt("is_shard")?
            .map(|v| v == "true")
            .unwrap_or(false);

        let chunk_range = if is_shard {
            let start: u64 = self
                .meta_value("chunk_range_start")?
                .parse()
                .map_err(|e| Error::Serialization(format!("bad chunk_range_start: {e}")))?;
            let end: u64 = self
                .meta_value("chunk_range_end")?
                .parse()
                .map_err(|e| Error::Serialization(format!("bad chunk_range_end: {e}")))?;
            Some(ChunkRange::new(start, end))
        } else {
            None
        };

        let chunk_count = self.chunk_count()?;

        // File size: use the database path from PRAGMA.
        let db_path: String = self
            .db
            .query_row("PRAGMA database_list", [], |r| r.get(2))?;
        let index_size_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

        Ok(IndexInfo {
            corpus_id,
            corpus_name,
            path: db_path.into(),
            chunk_count,
            index_size_bytes,
            created_at,
            last_updated,
            embedding_model,
            embedding_dimensions,
            mesh_sharing,
            is_shard,
            chunk_range,
        })
    }

    /// Return the number of chunks in the index.
    pub fn chunk_count(&self) -> Result<u64> {
        let count: i64 =
            self.db
                .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        Ok(count as u64)
    }

    // ── Access for sharding module ────────────────────────

    /// Borrow the underlying database connection.
    /// Used by the sharding module for direct SQL operations.
    pub fn connection(&self) -> &Connection {
        &self.db
    }

    /// Set or update a corpus_meta key-value pair.
    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO corpus_meta(key, value) VALUES (?, ?)",
            params![key, value],
        )?;
        Ok(())
    }

    // ── Private helpers ────────────────────────────────────

    fn meta_value(&self, key: &str) -> Result<String> {
        self.db
            .query_row(
                "SELECT value FROM corpus_meta WHERE key = ?",
                params![key],
                |r| r.get(0),
            )
            .map_err(|e| {
                Error::IndexNotFound(format!("missing corpus_meta key '{key}': {e}"))
            })
    }

    fn meta_value_opt(&self, key: &str) -> Result<Option<String>> {
        self.db
            .query_row(
                "SELECT value FROM corpus_meta WHERE key = ?",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Vector search via sqlite-vec. Returns (rowid, L2 distance).
    fn vec_search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let blob = embedding_to_blob(query_embedding);
        let mut stmt = self.db.prepare(
            "SELECT rowid, distance FROM chunks_vec WHERE embedding MATCH ? ORDER BY distance LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![blob, limit as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f32>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Brute-force cosine similarity when sqlite-vec is unavailable.
    fn brute_force_search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let mut stmt =
            self.db.prepare("SELECT id, embedding FROM chunks")?;
        let mut scored: Vec<(i64, f32)> = stmt
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                let blob: Vec<u8> = r.get(1)?;
                Ok((id, blob))
            })?
            .filter_map(|r| r.ok())
            .map(|(id, blob)| {
                let emb = blob_to_embedding(&blob);
                let sim = cosine_similarity(query_embedding, &emb);
                (id, sim)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// FTS5 keyword search. Returns (rowid, bm25 rank).
    fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(i64, f32)>> {
        let mut stmt = self.db.prepare(
            "SELECT rowid, rank FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY rank LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![query, limit as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f32>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

// ─── Free helpers ──────────────────────────────────────────

/// Register the sqlite-vec extension and verify it works on `conn`.
/// Returns true on success, false if the extension cannot be loaded.
fn try_load_vec(conn: &Connection) -> bool {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    }
    // Verify it actually works by calling vec_version().
    conn.query_row("SELECT vec_version()", [], |_| Ok(()))
        .is_ok()
}

/// Convert an f32 embedding slice to little-endian bytes.
pub fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        buf.extend_from_slice(&val.to_le_bytes());
    }
    buf
}

/// Convert a blob of little-endian f32 bytes back to a Vec<f32>.
pub fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

/// Current unix timestamp in seconds.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Sanitize text for FTS5 queries — strip characters that would cause syntax
/// errors and collapse whitespace. Returns an empty string if nothing useful
/// remains.
fn sanitize_fts_query(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    // Collapse whitespace and trim.
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build a simple 4-d embedding pointing in a given direction.
    fn make_embedding(direction: &[f32; 4]) -> Vec<f32> {
        direction.to_vec()
    }

    fn create_test_index(dir: &Path) -> CorpusIndex {
        let db_path = dir.join("test.db");
        CorpusIndex::create(
            &db_path,
            "test-corpus",
            "Test Corpus",
            "test-model",
            4,
            false,
            "MIT",
        )
        .expect("create index")
    }

    fn sample_chunks() -> Vec<(InsertChunk, Vec<f32>)> {
        vec![
            (
                InsertChunk {
                    content: "Rust is a systems programming language".into(),
                    title: Some("Rust Language".into()),
                    url: Some("https://rust-lang.org".into()),
                    metadata: Some(r#"{"source":"docs"}"#.into()),
                },
                make_embedding(&[1.0, 0.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Python is great for machine learning".into(),
                    title: Some("Python ML".into()),
                    url: None,
                    metadata: None,
                },
                make_embedding(&[0.0, 1.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "SQLite is an embedded database engine".into(),
                    title: Some("SQLite".into()),
                    url: Some("https://sqlite.org".into()),
                    metadata: Some(r#"{"source":"wiki"}"#.into()),
                },
                make_embedding(&[0.0, 0.0, 1.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Rust and systems programming go hand in hand".into(),
                    title: Some("Systems Programming".into()),
                    url: None,
                    metadata: None,
                },
                make_embedding(&[0.9, 0.1, 0.0, 0.0]),
            ),
        ]
    }

    #[test]
    fn create_insert_and_count() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path());

        assert_eq!(idx.chunk_count().unwrap(), 0);

        idx.insert_batch(&sample_chunks()).unwrap();

        assert_eq!(idx.chunk_count().unwrap(), 4);
    }

    #[test]
    fn search_fts_only() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path());
        idx.insert_batch(&sample_chunks()).unwrap();

        // Search with empty embedding → FTS only.
        let results = idx.search(&[], "Rust programming", 10).unwrap();
        assert!(!results.is_empty(), "FTS search should return results");
        // The top result should mention Rust.
        assert!(
            results[0].content.contains("Rust"),
            "top FTS result should mention Rust"
        );
        assert_eq!(results[0].corpus_id, "test-corpus");
    }

    #[test]
    fn search_vector_only() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path());
        idx.insert_batch(&sample_chunks()).unwrap();

        // Query embedding close to [1, 0, 0, 0] → should find Rust chunks.
        let query = make_embedding(&[0.95, 0.05, 0.0, 0.0]);
        let results = idx.search(&query, "", 10).unwrap();
        assert!(!results.is_empty(), "vector search should return results");
        // Top result should be the Rust chunk (embedding [1,0,0,0] or [0.9,0.1,0,0]).
        assert!(
            results[0].content.contains("Rust"),
            "top vector result should be about Rust, got: {}",
            results[0].content
        );
    }

    #[test]
    fn search_hybrid() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path());
        idx.insert_batch(&sample_chunks()).unwrap();

        // Both embedding and text point toward Rust.
        let query_emb = make_embedding(&[0.9, 0.1, 0.0, 0.0]);
        let results = idx.search(&query_emb, "Rust", 10).unwrap();
        assert!(!results.is_empty(), "hybrid search should return results");
        assert!(
            results[0].content.contains("Rust"),
            "top hybrid result should be about Rust"
        );
    }

    #[test]
    fn info_returns_metadata() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path());
        idx.insert_batch(&sample_chunks()).unwrap();

        let info = idx.info().unwrap();
        assert_eq!(info.corpus_id, "test-corpus");
        assert_eq!(info.corpus_name, "Test Corpus");
        assert_eq!(info.embedding_model, "test-model");
        assert_eq!(info.embedding_dimensions, 4);
        assert_eq!(info.chunk_count, 4);
        assert!(!info.mesh_sharing);
        assert!(!info.is_shard);
        assert!(info.chunk_range.is_none());
        assert!(info.index_size_bytes > 0);
        assert!(info.created_at > 0);
        assert!(info.last_updated >= info.created_at);
    }

    #[test]
    fn open_existing_and_search() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("reopen.db");

        // Create and populate.
        {
            let idx = CorpusIndex::create(
                &db_path,
                "reopen-corpus",
                "Reopen Test",
                "test-model",
                4,
                true,
                "Apache-2.0",
            )
            .unwrap();
            idx.insert_batch(&sample_chunks()).unwrap();
        }

        // Re-open and verify.
        let idx = CorpusIndex::open(&db_path).unwrap();
        assert_eq!(idx.chunk_count().unwrap(), 4);

        let results = idx.search(&[], "SQLite database", 5).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("SQLite"));
        assert_eq!(results[0].corpus_id, "reopen-corpus");
    }

    #[test]
    fn embedding_round_trip() {
        let original = vec![1.0_f32, -2.5, 3.14, 0.0, f32::MAX, f32::MIN];
        let blob = embedding_to_blob(&original);
        let restored = blob_to_embedding(&blob);
        assert_eq!(original, restored);
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn sanitize_fts_strips_special_chars() {
        assert_eq!(sanitize_fts_query("hello world"), "hello world");
        assert_eq!(sanitize_fts_query("hello-world"), "hello world");
        assert_eq!(sanitize_fts_query("(NOT) OR *"), "NOT OR");
        assert_eq!(sanitize_fts_query("  "), "");
        assert_eq!(sanitize_fts_query(""), "");
    }

    #[test]
    fn open_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let result = CorpusIndex::open(&dir.path().join("nope.db"));
        assert!(result.is_err());
    }
}
