//! CorpusIndex — wraps a LanceDB table for a per-corpus index
//! with IVF-PQ vector search and Tantivy full-text search.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{
    Array, Float32Array, Int64Array, RecordBatch, StringArray,
    FixedSizeListArray,
    types::Float32Type,
};
use arrow_schema::SchemaRef;
use futures::TryStreamExt;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};

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

/// A single corpus index backed by LanceDB.
/// Uses IVF-PQ for vector search and Tantivy for full-text search.
pub struct CorpusIndex {
    db: lancedb::Connection,
    table: lancedb::Table,
    corpus_id: String,
    corpus_name: String,
    embedding_model: String,
    embedding_dimensions: usize,
    mesh_sharing: bool,
    license: String,
    created_at: u64,
    is_shard: bool,
    chunk_range: Option<ChunkRange>,
}

/// Build the Arrow schema for a corpus index table.
pub(crate) fn corpus_schema(embedding_dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
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
    ]))
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
    #[serde(default)]
    is_shard: bool,
    #[serde(default)]
    chunk_range_start: Option<u64>,
    #[serde(default)]
    chunk_range_end: Option<u64>,
}

fn meta_path(index_dir: &Path) -> std::path::PathBuf {
    index_dir.join("_corpus_meta.json")
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
    // ── Construction ───────────────────────────────────────

    /// Create a new LanceDB index at the given directory.
    pub async fn create(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        license: &str,
    ) -> Result<Self> {
        std::fs::create_dir_all(path)?;

        let db = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let schema = corpus_schema(embedding_dim);
        let table = db
            .create_empty_table(CHUNKS_TABLE, schema)
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let now = now_unix();
        let meta = IndexMeta {
            corpus_id: corpus_id.to_string(),
            corpus_name: corpus_name.to_string(),
            embedding_model: embedding_model.to_string(),
            embedding_dimensions: embedding_dim,
            mesh_sharing,
            license: license.to_string(),
            created_at: now,
            last_updated: now,
            is_shard: false,
            chunk_range_start: None,
            chunk_range_end: None,
        };
        write_meta(path, &meta)?;

        Ok(Self {
            db,
            table,
            corpus_id: corpus_id.to_string(),
            corpus_name: corpus_name.to_string(),
            embedding_model: embedding_model.to_string(),
            embedding_dimensions: embedding_dim,
            mesh_sharing,
            license: license.to_string(),
            created_at: now,
            is_shard: false,
            chunk_range: None,
        })
    }

    /// Open an existing LanceDB index.
    pub async fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::IndexNotFound(path.display().to_string()));
        }

        let meta = read_meta(path)?;

        let db = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let table = db
            .open_table(CHUNKS_TABLE)
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        let chunk_range = if meta.is_shard {
            match (meta.chunk_range_start, meta.chunk_range_end) {
                (Some(s), Some(e)) => Some(ChunkRange::new(s, e)),
                _ => None,
            }
        } else {
            None
        };

        Ok(Self {
            db,
            table,
            corpus_id: meta.corpus_id,
            corpus_name: meta.corpus_name,
            embedding_model: meta.embedding_model,
            embedding_dimensions: meta.embedding_dimensions,
            mesh_sharing: meta.mesh_sharing,
            license: meta.license,
            created_at: meta.created_at,
            is_shard: meta.is_shard,
            chunk_range,
        })
    }

    // ── Mutation ───────────────────────────────────────────

    /// Insert a batch of chunks (with pre-computed embeddings).
    pub async fn insert_batch(&self, chunks: &[(InsertChunk, Vec<f32>)]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let base_id = self.chunk_count().await?;

        let ids: Vec<i64> = (0..chunks.len())
            .map(|i| (base_id + i as u64 + 1) as i64)
            .collect();
        let contents: Vec<&str> = chunks.iter().map(|(c, _)| c.content.as_str()).collect();
        let titles: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.title.as_deref())
            .collect();
        let urls: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.url.as_deref())
            .collect();
        let metadatas: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.metadata.as_deref())
            .collect();

        // Build the embedding FixedSizeList array.
        let dim = self.embedding_dimensions as i32;
        let embedding_array = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            chunks.iter().map(|(_, e)| {
                Some(e.iter().map(|&v| Some(v)))
            }),
            dim,
        );

        let schema = corpus_schema(self.embedding_dimensions);
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(contents)),
                Arc::new(StringArray::from(titles)),
                Arc::new(StringArray::from(urls)),
                Arc::new(embedding_array),
                Arc::new(StringArray::from(metadatas)),
            ],
        )
        .map_err(|e| Error::Serialization(format!("record batch: {e}")))?;

        self.table
            .add(vec![batch])
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        // Update last_updated in metadata.
        let index_dir = Path::new(self.db.uri());
        if let Ok(mut meta) = read_meta(index_dir) {
            meta.last_updated = now_unix();
            let _ = write_meta(index_dir, &meta);
        }

        Ok(())
    }

    /// Build vector + FTS indexes for efficient search.
    /// Should be called after all data is inserted.
    pub async fn build_indexes(&self) -> Result<()> {
        let count = self.chunk_count().await?;
        if count == 0 {
            return Ok(());
        }

        // Build IVF-PQ vector index.
        // Only build if we have enough data (LanceDB needs >= 256 rows for IVF).
        if count >= 256 {
            self.table
                .create_index(
                    &["embedding"],
                    lancedb::index::Index::Auto,
                )
                .execute()
                .await
                .map_err(|e| Error::Database(format!("vector index: {e}")))?;
        }

        // Build Tantivy FTS indexes on content and title separately
        // (composite multi-column FTS is not supported in LanceDB).
        self.table
            .create_index(
                &["content"],
                lancedb::index::Index::FTS(
                    lancedb::index::scalar::FtsIndexBuilder::default(),
                ),
            )
            .execute()
            .await
            .map_err(|e| Error::Database(format!("FTS content index: {e}")))?;

        self.table
            .create_index(
                &["title"],
                lancedb::index::Index::FTS(
                    lancedb::index::scalar::FtsIndexBuilder::default(),
                ),
            )
            .execute()
            .await
            .map_err(|e| Error::Database(format!("FTS title index: {e}")))?;

        Ok(())
    }

    // ── Search ─────────────────────────────────────────────

    /// Hybrid search combining vector similarity and FTS keyword matching.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        let do_vector = !query_embedding.is_empty();
        let sanitized = sanitize_fts_query(query_text);
        let do_fts = !sanitized.is_empty();

        if !do_vector && !do_fts {
            return Ok(Vec::new());
        }

        let results = if do_vector && do_fts {
            // Hybrid: vector + FTS combined via reranking.
            self.table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .full_text_search(
                    FullTextSearchQuery::new(sanitized),
                )
                .limit(limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("hybrid search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        } else if do_vector {
            // Vector-only search.
            self.table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .limit(limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("vector search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        } else {
            // FTS-only search.
            self.table
                .query()
                .full_text_search(
                    FullTextSearchQuery::new(sanitized),
                )
                .limit(limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("FTS search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        };

        // Convert Arrow RecordBatches to ScoredChunks.
        let mut scored = Vec::new();
        for batch in &results {
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let urls = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadata_col = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let distance_col = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

            let num_rows = batch.num_rows();
            for i in 0..num_rows {
                let content = contents
                    .map(|c| c.value(i).to_string())
                    .unwrap_or_default();
                let title = titles.and_then(|t| {
                    if t.is_null(i) { None } else { Some(t.value(i).to_string()) }
                });
                let url = urls.and_then(|u| {
                    if u.is_null(i) { None } else { Some(u.value(i).to_string()) }
                });
                let metadata: HashMap<String, String> = metadata_col
                    .and_then(|m| {
                        if m.is_null(i) {
                            None
                        } else {
                            serde_json::from_str(m.value(i)).ok()
                        }
                    })
                    .unwrap_or_default();

                // Convert distance to score (lower distance = higher score).
                let score = distance_col
                    .map(|d| {
                        let dist = d.value(i);
                        1.0 / (1.0 + dist)
                    })
                    .unwrap_or(1.0); // FTS-only results get score 1.0

                scored.push(ScoredChunk {
                    content,
                    title,
                    url,
                    corpus_id: self.corpus_id.clone(),
                    score,
                    metadata,
                });
            }
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
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
        idx.build_indexes().await.unwrap();

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
            idx.build_indexes().await.unwrap();
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
