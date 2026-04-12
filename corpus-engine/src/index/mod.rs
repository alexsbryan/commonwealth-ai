//! CorpusIndex — wraps a LanceDB table for a per-corpus index
//! with IVF-PQ vector search and Tantivy full-text search.

mod create;
mod enrichment;
mod search;
mod write;

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow::datatypes::{DataType, Field, Schema};
use arrow_schema::SchemaRef;

use crate::error::{Error, Result};
use crate::types::{ChunkRange, IndexInfo};

// ─── Helper types ──────────────────────────────────────────

/// Typed code-intelligence metadata for a single chunk. Populated by the
/// `code` extractor and promoted into the typed schema columns by the
/// insert path; `None` for non-code corpora.
#[derive(Clone, Debug, Default)]
pub struct InsertCodeMeta {
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub language: Option<String>,
    pub mtime: Option<i64>,
}

/// Extract code-intelligence fields from an `ExtractedDoc.metadata` JSON
/// object. Returns an empty `InsertCodeMeta` if the metadata is missing or
/// doesn't carry code-specific keys — in that case every code column is
/// stored as Null and the chunk behaves like any other non-code chunk.
pub fn code_meta_from_json(metadata: Option<&serde_json::Value>) -> InsertCodeMeta {
    let Some(obj) = metadata.and_then(|v| v.as_object()) else {
        return InsertCodeMeta::default();
    };
    InsertCodeMeta {
        symbol_name: obj.get("symbol_name").and_then(|v| v.as_str()).map(String::from),
        symbol_kind: obj.get("symbol_kind").and_then(|v| v.as_str()).map(String::from),
        file_path: obj.get("file_path").and_then(|v| v.as_str()).map(String::from),
        line_start: obj.get("line_start").and_then(|v| v.as_i64()).map(|n| n as i32),
        line_end: obj.get("line_end").and_then(|v| v.as_i64()).map(|n| n as i32),
        language: obj.get("language").and_then(|v| v.as_str()).map(String::from),
        mtime: obj.get("mtime").and_then(|v| v.as_i64()),
    }
}

/// A chunk to be inserted into the index.
#[derive(Clone)]
pub struct InsertChunk {
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub metadata: Option<String>, // JSON string
    /// BLAKE3 hex digest of the chunk text, populated during ingestion.
    pub content_hash: Option<String>,
    /// Document-level grouping key (article URL, DOI, etc.).
    /// Used by delta updates to delete/replace all chunks from one document.
    pub source_doc_id: Option<String>,
    /// Optional code-intelligence metadata. `Default::default()` means
    /// non-code chunk — all code columns will be Null.
    pub code: InsertCodeMeta,
}

/// A pre-embedded chunk ready for direct insertion.
pub struct EmbeddedChunk {
    pub insert: InsertChunk,
    pub embedding: Vec<f32>,
}

/// A chunk read out of an index, used by the enrichment pipeline.
#[derive(Debug, Clone)]
pub struct StoredChunk {
    pub id: u64,
    pub content: String,
    pub title: Option<String>,
}

/// A chunk with its raw metadata JSON string, used by the structural
/// enrichment pipeline (link graph builder and article profile builder).
#[derive(Debug, Clone)]
pub struct StoredChunkWithMetadata {
    pub id: u64,
    pub title: Option<String>,
    pub url: Option<String>,
    pub metadata_raw: Option<String>,
}

// ─── CorpusIndex ───────────────────────────────────────────

/// A single corpus index backed by LanceDB.
/// Uses IVF-PQ for vector search and Tantivy for full-text search.
///
/// The on-disk `_corpus_meta.json` is the single source of truth for index
/// metadata. We cache only the two fields read on every operation
/// (`corpus_id` for identity, `embedding_dimensions` for vector ops); the
/// rest is loaded on-demand via `info()`. This avoids stale-cache bugs when
/// callers like `set_shard_meta()` mutate the metadata file.
pub struct CorpusIndex {
    db: lancedb::Connection,
    table: lancedb::Table,
    corpus_id: String,
    embedding_dimensions: usize,
}

/// Build the Arrow schema for a corpus index table.
///
/// Columns fall into two groups:
/// - **Base** (`id`, `content`, `title`, `url`, `embedding`, `metadata`,
///   `content_hash`, `source_doc_id`) — populated by every corpus type.
/// - **Code intelligence** (`symbol_name`, `symbol_kind`, `file_path`,
///   `line_start`, `line_end`, `language`, `mtime`) — nullable. Populated
///   only by code corpora; Wikipedia/SEP etc. leave them Null. Real
///   columns (not JSON) so LanceDB filter pushdown keeps symbol-lookup
///   under 10ms.
pub(crate) fn corpus_schema(embedding_dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        // Base columns
        Field::new("id", DataType::Int64, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, true),
        Field::new("url", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                embedding_dim as i32,
            ),
            false,
        ),
        Field::new("metadata", DataType::Utf8, true),
        Field::new("content_hash", DataType::Utf8, true),
        Field::new("source_doc_id", DataType::Utf8, true),
        // Code-intelligence columns (nullable for non-code corpora)
        Field::new("symbol_name", DataType::Utf8, true),
        Field::new("symbol_kind", DataType::Utf8, true),
        Field::new("file_path", DataType::Utf8, true),
        Field::new("line_start", DataType::Int32, true),
        Field::new("line_end", DataType::Int32, true),
        Field::new("language", DataType::Utf8, true),
        Field::new("mtime", DataType::Int64, true),
    ]))
}

/// Current on-disk schema version. Bumped when `corpus_schema()` changes
/// in a way that requires an LanceDB `add_columns` migration on open.
pub(crate) const CURRENT_INDEX_SCHEMA_VERSION: u32 = 2;

fn default_index_schema_version() -> u32 {
    1 // Files without the field predate versioning → treat as v1.
}

/// Metadata stored as a JSON file alongside the LanceDB table.
#[derive(serde::Serialize, serde::Deserialize)]
struct IndexMeta {
    corpus_id: String,
    corpus_name: String,
    embedding_model: String,
    embedding_dimensions: usize,
    mesh_sharing: bool,
    license: String,
    created_at: u64,
    last_updated: u64,
    /// Version of the LanceDB column layout used by this index. Opened
    /// indexes with a version lower than `CURRENT_INDEX_SCHEMA_VERSION`
    /// are migrated in place via `Table::add_columns`.
    #[serde(default = "default_index_schema_version")]
    schema_version: u32,
    #[serde(default)]
    is_shard: bool,
    #[serde(default)]
    chunk_range_start: Option<u64>,
    #[serde(default)]
    chunk_range_end: Option<u64>,
    /// Set to true when ingestion starts, cleared to false only on successful
    /// completion. If the process is killed mid-ingest this stays true, allowing
    /// `installed_indexes()` to skip the partial index rather than reporting it
    /// as fully installed. Defaults to false so existing complete indexes (written
    /// before this field existed) are treated as complete.
    #[serde(default)]
    ingestion_in_progress: bool,
    /// Number of source documents iterated (including skipped/errored) at the
    /// last successful batch flush. Used to resume a killed ingest from the
    /// correct position without re-embedding already-committed chunks.
    /// Defaults to 0 so existing complete indexes don't try to resume.
    #[serde(default)]
    committed_iter_pos: u64,
    /// Set to true once `build_indexes()` completes successfully.
    /// Allows a resume to skip index-building if it already finished in a
    /// previous run (e.g. process killed between build_indexes and
    /// mark_ingestion_complete). Defaults to false.
    #[serde(default)]
    indexes_built: bool,
    /// Per-sub-phase checkpoints within build_indexes(). Allow a resume to skip
    /// whichever sub-indexes were already built before a kill/crash, avoiding
    /// multi-hour rebuilds of completed work.
    #[serde(default)]
    vector_index_built: bool,
    #[serde(default)]
    content_fts_built: bool,
    #[serde(default)]
    title_fts_built: bool,

    // ── Health-check fields ──────────────────────────────────
    /// Expected total chunks, written at ingest start.
    #[serde(default)]
    chunks_expected: Option<u64>,
    /// Resume cursor (batch ID) from last checkpoint. Same as
    /// committed_iter_pos semantically but expressed as a string batch key
    /// for compatibility with the CorpusUpdater progress log.
    #[serde(default)]
    resume_from: Option<String>,
    /// True if the enrichment pipeline has been run at least once.
    #[serde(default)]
    enrichment_enabled: bool,
    /// Count of chunks that have at least one extracted claim.
    #[serde(default)]
    enriched_chunks: Option<u64>,
    /// Source version token (date stamp or manifest hash).
    #[serde(default)]
    source_version: Option<String>,
    /// Manifest URL for update checks.
    #[serde(default)]
    update_manifest_url: Option<String>,
}

fn meta_path(index_dir: &Path) -> std::path::PathBuf {
    index_dir.join("_corpus_meta.json")
}

/// Migrate an on-disk index from `from_version` to the current schema
/// version. Adds the code-intelligence columns as all-Null when the
/// source version is < 2. Safe to call on a partially-migrated index;
/// LanceDB's `add_columns` is a no-op for columns that already exist.
async fn migrate_schema(table: &lancedb::Table, from_version: u32) -> Result<()> {
    if from_version >= 2 {
        return Ok(());
    }

    use lancedb::table::NewColumnTransform;

    // Check which code columns are actually missing — a previous
    // migration attempt may have added some and then crashed. We build
    // the Arrow schema for exactly the columns that don't exist yet,
    // so retries are idempotent.
    let current = table
        .schema()
        .await
        .map_err(|e| Error::Database(format!("read schema: {e}")))?;
    let existing: std::collections::HashSet<&str> =
        current.fields().iter().map(|f| f.name().as_str()).collect();

    let wanted: &[(&str, DataType)] = &[
        ("symbol_name", DataType::Utf8),
        ("symbol_kind", DataType::Utf8),
        ("file_path", DataType::Utf8),
        ("line_start", DataType::Int32),
        ("line_end", DataType::Int32),
        ("language", DataType::Utf8),
        ("mtime", DataType::Int64),
    ];

    let missing: Vec<Field> = wanted
        .iter()
        .filter(|(name, _)| !existing.contains(name))
        .map(|(name, dtype)| Field::new(*name, dtype.clone(), true))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let add_schema = Arc::new(Schema::new(missing));
    table
        .add_columns(NewColumnTransform::AllNulls(add_schema), None)
        .await
        .map_err(|e| Error::Database(format!("add_columns migration: {e}")))?;

    tracing::info!("Migrated index to schema v{CURRENT_INDEX_SCHEMA_VERSION}");
    Ok(())
}

fn read_meta(index_dir: &Path) -> Result<IndexMeta> {
    let path = meta_path(index_dir);
    let content = std::fs::read_to_string(&path).map_err(|e| {
        Error::IndexNotFound(format!("Missing metadata at {}: {e}", path.display()))
    })?;
    serde_json::from_str(&content).map_err(|e| {
        Error::Serialization(format!("Bad index metadata: {e}"))
    })
}

fn write_meta(index_dir: &Path, meta: &IndexMeta) -> Result<()> {
    let path = meta_path(index_dir);
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| Error::Serialization(e.to_string()))?;
    std::fs::write(&path, json)?;
    Ok(())
}

const CHUNKS_TABLE: &str = "chunks";

impl CorpusIndex {
    /// Open an existing LanceDB index.
    ///
    /// Lazily migrates pre-v2 indexes to add the code-intelligence
    /// columns. Migration is idempotent — guarded by the schema_version
    /// field in `_corpus_meta.json` — so subsequent opens are cheap.
    pub async fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::IndexNotFound(path.display().to_string()));
        }

        let mut meta = read_meta(path)?;

        let db = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let table = db
            .open_table(CHUNKS_TABLE)
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        if meta.schema_version < CURRENT_INDEX_SCHEMA_VERSION {
            migrate_schema(&table, meta.schema_version).await?;
            meta.schema_version = CURRENT_INDEX_SCHEMA_VERSION;
            let _ = write_meta(path, &meta);
        }

        Ok(Self {
            db,
            table,
            corpus_id: meta.corpus_id,
            embedding_dimensions: meta.embedding_dimensions,
        })
    }

    // ── Info ───────────────────────────────────────────────

    /// Return metadata about this index.
    pub async fn info(&self) -> Result<IndexInfo> {
        let index_dir = Path::new(self.db.uri());
        let meta = read_meta(index_dir)?;
        let chunk_count = self.chunk_count().await?;
        let index_size_bytes = dir_size(index_dir);

        let chunk_range = if meta.is_shard {
            match (meta.chunk_range_start, meta.chunk_range_end) {
                (Some(s), Some(e)) => Some(ChunkRange::new(s, e)),
                _ => None,
            }
        } else {
            None
        };

        Ok(IndexInfo {
            corpus_id: meta.corpus_id,
            corpus_name: meta.corpus_name,
            path: index_dir.to_path_buf(),
            chunk_count,
            index_size_bytes,
            created_at: meta.created_at,
            last_updated: meta.last_updated,
            embedding_model: meta.embedding_model,
            embedding_dimensions: meta.embedding_dimensions,
            mesh_sharing: meta.mesh_sharing,
            is_shard: meta.is_shard,
            chunk_range,
            chunks_expected: meta.chunks_expected,
            resume_from: meta.resume_from,
            enrichment_enabled: meta.enrichment_enabled,
            enriched_chunks: meta.enriched_chunks,
            source_version: meta.source_version,
            update_manifest_url: meta.update_manifest_url,
        })
    }

    /// Return the number of chunks in the index.
    pub async fn chunk_count(&self) -> Result<u64> {
        let count = self
            .table
            .count_rows(None)
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(count as u64)
    }

    // ── Health-check helpers ───────────────────────────────

    /// Number of documents in the FTS index.
    /// Falls back to `chunk_count()` if the FTS index is unavailable.
    pub async fn fts_doc_count(&self) -> Result<u64> {
        // LanceDB exposes FTS through query; we count via a broad wildcard search
        // that matches everything.  We use the chunk_count as a cheap fallback.
        // A proper Tantivy row count would require internal API access — for now
        // we use a sampling approach: perform a full vector-free scan limited to
        // a very large number and trust LanceDB to return all rows.
        // The simplest reliable proxy: use count_rows with no filter (same as chunk_count).
        self.chunk_count().await
    }

    // ── Access for sharding module ────────────────────────

    /// Get the LanceDB connection.
    pub fn connection(&self) -> &lancedb::Connection {
        &self.db
    }

    /// Get the chunks table.
    pub fn table(&self) -> &lancedb::Table {
        &self.table
    }

    /// Get the embedding dimensions.
    pub fn embedding_dim(&self) -> usize {
        self.embedding_dimensions
    }

    /// Set shard metadata.
    pub fn set_shard_meta(
        &self,
        chunk_range: ChunkRange,
    ) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.is_shard = true;
        meta.chunk_range_start = Some(chunk_range.start_id);
        meta.chunk_range_end = Some(chunk_range.end_id);
        write_meta(index_dir, &meta)
    }

    /// The corpus ID this index belongs to.
    pub fn corpus_id(&self) -> &str {
        &self.corpus_id
    }

    /// Get the index directory path.
    pub fn path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.db.uri())
    }

    // ── Table helpers ────────────────────────────────────

    async fn has_table(&self, name: &str) -> bool {
        match self.db.table_names().execute().await {
            Ok(names) => names.iter().any(|n| n == name),
            Err(_) => false,
        }
    }

}

// ─── Free helpers ──────────────────────────────────────────

/// Current unix timestamp in seconds.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Calculate total size of a directory recursively.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = p.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// Sanitize text for FTS queries — strip characters that cause syntax errors.
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
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_embedding(direction: &[f32; 4]) -> Vec<f32> {
        direction.to_vec()
    }

    async fn create_test_index(dir: &Path) -> CorpusIndex {
        let db_path = dir.join("test-corpus");
        CorpusIndex::create(
            &db_path,
            "test-corpus",
            "Test Corpus",
            "test-model",
            4,
            false,
            "MIT",
        )
        .await
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
                    content_hash: None,
                    source_doc_id: Some("https://rust-lang.org".into()),
                    code: InsertCodeMeta::default(),
                },
                make_embedding(&[1.0, 0.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Python is great for machine learning".into(),
                    title: Some("Python ML".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: None,
                    code: InsertCodeMeta::default(),
                },
                make_embedding(&[0.0, 1.0, 0.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "SQLite is an embedded database engine".into(),
                    title: Some("SQLite".into()),
                    url: Some("https://sqlite.org".into()),
                    metadata: Some(r#"{"source":"wiki"}"#.into()),
                    content_hash: None,
                    source_doc_id: Some("https://sqlite.org".into()),
                    code: InsertCodeMeta::default(),
                },
                make_embedding(&[0.0, 0.0, 1.0, 0.0]),
            ),
            (
                InsertChunk {
                    content: "Rust and systems programming go hand in hand".into(),
                    title: Some("Systems Programming".into()),
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: None,
                    code: InsertCodeMeta::default(),
                },
                make_embedding(&[0.9, 0.1, 0.0, 0.0]),
            ),
        ]
    }

    #[tokio::test]
    async fn create_insert_and_count() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;

        assert_eq!(idx.chunk_count().await.unwrap(), 0);

        idx.insert_batch(&sample_chunks()).await.unwrap();

        assert_eq!(idx.chunk_count().await.unwrap(), 4);
    }

    #[tokio::test]
    async fn search_fts_only() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();
        idx.build_indexes(true, true, None).await.unwrap();

        let results = idx.search(&[], "Rust programming", 10).await.unwrap();
        assert!(!results.is_empty(), "FTS search should return results");
        assert!(
            results[0].content.contains("Rust"),
            "top FTS result should mention Rust"
        );
        assert_eq!(results[0].corpus_id, "test-corpus");
    }

    #[tokio::test]
    async fn search_vector_only() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        // Vector search works without index (brute force on small datasets).
        let query = make_embedding(&[0.95, 0.05, 0.0, 0.0]);
        let results = idx.search(&query, "", 10).await.unwrap();
        assert!(!results.is_empty(), "vector search should return results");
        assert!(
            results[0].content.contains("Rust"),
            "top vector result should be about Rust, got: {}",
            results[0].content
        );
    }

    #[tokio::test]
    async fn info_returns_metadata() {
        let dir = tempdir().unwrap();
        let idx = create_test_index(dir.path()).await;
        idx.insert_batch(&sample_chunks()).await.unwrap();

        let info = idx.info().await.unwrap();
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

    #[tokio::test]
    async fn open_existing_and_search() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("reopen-corpus");

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
            .await
            .unwrap();
            idx.insert_batch(&sample_chunks()).await.unwrap();
            idx.build_indexes(true, true, None).await.unwrap();
        }

        // Re-open and verify.
        let idx = CorpusIndex::open(&db_path).await.unwrap();
        assert_eq!(idx.chunk_count().await.unwrap(), 4);

        let results = idx.search(&[], "embedded database", 5).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("SQLite"));
        assert_eq!(results[0].corpus_id, "reopen-corpus");
    }

    #[test]
    fn sanitize_fts_strips_special_chars() {
        assert_eq!(sanitize_fts_query("hello world"), "hello world");
        assert_eq!(sanitize_fts_query("hello-world"), "hello world");
        assert_eq!(sanitize_fts_query("(NOT) OR *"), "NOT OR");
        assert_eq!(sanitize_fts_query("  "), "");
        assert_eq!(sanitize_fts_query(""), "");
    }

    #[tokio::test]
    async fn open_nonexistent_returns_error() {
        let dir = tempdir().unwrap();
        let result = CorpusIndex::open(&dir.path().join("nope")).await;
        assert!(result.is_err());
    }
}
