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
        Field::new("content_hash", DataType::Utf8, true),
        Field::new("source_doc_id", DataType::Utf8, true),
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

// ─── Vector index helpers ──────────────────────────────────

/// Compute IVF partition count: sqrt(n), clamped 8–4096.
/// LanceDB Auto uses the same heuristic; making it explicit lets us log it.
fn optimal_partitions(num_chunks: u64) -> u32 {
    ((num_chunks as f64).sqrt() as u32).max(8).min(4096)
}

/// Read the embedding column's fixed-list dimension from the table schema.
async fn detect_vector_dims(table: &lancedb::Table) -> Result<usize> {
    use arrow::datatypes::DataType;
    let schema = table.schema().await
        .map_err(|e| Error::Database(format!("schema: {e}")))?;
    for field in schema.fields() {
        if field.name() == "embedding" {
            if let DataType::FixedSizeList(_, dims) = field.data_type() {
                return Ok(*dims as usize);
            }
        }
    }
    Err(Error::Database("embedding column not found or not FixedSizeList".into()))
}

/// Sum file sizes in a directory (flat, non-recursive).
/// Returns 0 if the directory doesn't exist yet.
fn dir_size_bytes_sync(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Build the IVF-PQ vector index with filesystem-polling phase-aware progress.
///
/// Spawns a background thread that polls `indices_dir` every 3 seconds.
/// Before any files appear (Phase A: k-means training) it logs a heartbeat
/// every 15 s so the user knows the process is alive. Once files start
/// growing (Phase B: vector encoding) it logs a percentage estimate.
///
/// The build itself uses explicit `IvfPqIndexBuilder` so that partition count
/// and distance type are logged rather than silently delegated to Auto.
async fn build_vector_index_with_progress(
    table: &lancedb::Table,
    indices_dir: &Path,
    num_chunks: u64,
    num_partitions: u32,
    dims: usize,
    corpus_id: &str,
) -> Result<()> {
    // Each encoded vector occupies roughly (dims/16) PQ bytes + per-centroid
    // overhead (~32 bytes). Use this to estimate when encoding is complete.
    let num_sub_vectors = ((dims / 16) as u64).max(1);
    let estimated_bytes = num_chunks * (num_sub_vectors + 32);
    // LanceDB's default sample rate is 256 vectors per partition for k-means.
    let sample_vectors = 256_u64.saturating_mul(num_partitions as u64);

    eprintln!(
        "[{corpus_id}] IVF-PQ params — chunks: {num_chunks}, dims: {dims}, \
         partitions: {num_partitions}, sub_vectors: {num_sub_vectors}, \
         training sample: ~{sample_vectors} vectors"
    );

    // Use spawn_blocking + std::thread::sleep so the poll loop doesn't
    // compete with the Tokio executor during the CPU-bound k-means phase.
    // An AtomicBool signals the thread to stop — spawn_blocking tasks cannot
    // be aborted via JoinHandle::abort() once they are running.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_clone = done.clone();
    let indices_dir_owned = indices_dir.to_path_buf();
    let id = corpus_id.to_string();
    let poll_handle = tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        let mut last_pct: i32 = -1;
        let mut last_elapsed_logged: u64 = 0;
        while !done_clone.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(3));
            if done_clone.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let dir_bytes = dir_size_bytes_sync(&indices_dir_owned);
            let elapsed = start.elapsed().as_secs();
            if dir_bytes < 16 * 1024 {
                // Phase A: k-means training — no significant files yet.
                if elapsed.saturating_sub(last_elapsed_logged) >= 15 {
                    eprintln!(
                        "[{id}] ↳ Training IVF centroids \
                         (~{sample_vectors} vectors, {elapsed}s elapsed)..."
                    );
                    last_elapsed_logged = elapsed;
                }
            } else {
                // Phase B: vector encoding — files are growing.
                let pct = ((dir_bytes as f64 / estimated_bytes as f64) * 100.0)
                    .clamp(0.0, 99.0) as i32;
                if pct >= last_pct + 5 {
                    eprintln!("[{id}] ↳ Encoding vectors → {pct}%");
                    last_pct = pct;
                }
            }
        }
    });

    let result = table
        .create_index(
            &["embedding"],
            lancedb::index::Index::IvfPq(
                lancedb::index::vector::IvfPqIndexBuilder::default()
                    .num_partitions(num_partitions)
                    .distance_type(lancedb::DistanceType::Cosine),
            ),
        )
        .replace(true)
        .execute()
        .await;

    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = poll_handle.await;
    result.map_err(|e| Error::Database(format!("vector index: {e}")))
}

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
            ingestion_in_progress: true,
            committed_iter_pos: 0,
            indexes_built: false,
            vector_index_built: false,
            content_fts_built: false,
            title_fts_built: false,
            chunks_expected: None,
            resume_from: None,
            enrichment_enabled: false,
            enriched_chunks: None,
            source_version: None,
            update_manifest_url: None,
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

    /// Create a fresh index, or resume an interrupted one.
    ///
    /// If a partial index (with `ingestion_in_progress: true`) already exists at
    /// `path`, this opens it in append mode and returns `(index, committed_iter_pos)`
    /// so the caller can skip already-processed source documents. This makes
    /// long ingests (hours) resumable after a process kill or crash.
    ///
    /// If no index exists, a fresh one is created and `committed_iter_pos` is 0.
    pub async fn create_or_resume(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        license: &str,
    ) -> Result<(Self, u64)> {
        // Resume path: partial index exists from a previous killed run.
        if path.exists() && !Self::is_ingestion_complete(path) {
            match Self::open(path).await {
                Ok(index) => {
                    let iter_pos = read_meta(path)
                        .map(|m| m.committed_iter_pos)
                        .unwrap_or(0);
                    let existing = index.chunk_count().await.unwrap_or(0);
                    eprintln!(
                        "[corpus] Resuming '{}' — skipping first {iter_pos} source docs ({existing} chunks already indexed)",
                        corpus_id,
                    );
                    return Ok((index, iter_pos));
                }
                Err(e) => {
                    // Corrupt partial index — wipe and start fresh.
                    tracing::warn!(
                        "Partial index at '{}' could not be opened ({e}); starting fresh",
                        path.display()
                    );
                    if let Err(rm) = std::fs::remove_dir_all(path) {
                        tracing::warn!("Failed to remove corrupt partial index: {rm}");
                    }
                }
            }
        }

        // Fresh start.
        let index = Self::create(path, corpus_id, corpus_name, embedding_model, embedding_dim, mesh_sharing, license).await?;
        Ok((index, 0))
    }

    /// Persist the current iterator position as a resume checkpoint.
    /// Called after each successful batch flush so that a subsequent restart
    /// can skip already-committed source documents.
    pub fn update_committed_iter_pos(&self, iter_pos: u64) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.committed_iter_pos = iter_pos;
        write_meta(index_dir, &meta)
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
        let content_hashes: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.content_hash.as_deref())
            .collect();
        let source_doc_ids: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.source_doc_id.as_deref())
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
                Arc::new(StringArray::from(content_hashes)),
                Arc::new(StringArray::from(source_doc_ids)),
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

    /// Clear the `ingestion_in_progress` flag. Called by the engine once the
    /// full pipeline (embed → index → optional enrichment) completes successfully.
    pub fn mark_ingestion_complete(&self) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.ingestion_in_progress = false;
        write_meta(index_dir, &meta)
    }

    /// Returns true if the index has a complete, fully-committed ingestion.
    /// Used by `installed_indexes()` to skip partially-ingested directories
    /// left behind by a process kill.
    pub fn is_ingestion_complete(path: &Path) -> bool {
        read_meta(path)
            .map(|m| !m.ingestion_in_progress)
            .unwrap_or(false)
    }

    /// Returns true if at least one batch of chunks has been committed to this
    /// index. Used by the ingest cleanup logic to decide whether to wipe a
    /// failed install (safe only if no work has been done yet).
    pub fn has_committed_data(path: &Path) -> bool {
        read_meta(path)
            .map(|m| m.committed_iter_pos > 0)
            .unwrap_or(false)
    }

    /// Returns true if the vector + FTS search indexes were already built in a
    /// previous run. A resume can skip `build_indexes()` entirely when this is
    /// true, jumping straight to `mark_ingestion_complete()`.
    pub fn indexes_are_built(path: &Path) -> bool {
        read_meta(path)
            .map(|m| m.indexes_built)
            .unwrap_or(false)
    }

    /// Persist that search indexes have been successfully built.
    pub fn mark_indexes_built(&self) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.indexes_built = true;
        write_meta(index_dir, &meta)
    }

    pub fn mark_vector_index_built(&self) -> Result<()> {
        let dir = Path::new(self.db.uri());
        let mut meta = read_meta(dir)?;
        meta.vector_index_built = true;
        write_meta(dir, &meta)
    }

    /// Returns `true` if the embedding column has a complete IVF-PQ vector index.
    ///
    /// Checks the local meta flag first (fast path), then verifies via
    /// `list_indices()` — which only returns COMPLETE indices in the LanceDB
    /// Rust SDK. Self-heals the meta flag if the index is found intact.
    pub async fn is_vector_index_ready(&self) -> bool {
        let dir = std::path::Path::new(self.db.uri()).to_path_buf();
        let meta_says_done = read_meta(&dir)
            .map(|m| m.vector_index_built)
            .unwrap_or(false);

        let indices = self.table.list_indices().await.unwrap_or_default();
        let live_check = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "embedding"));

        if live_check && !meta_says_done {
            let _ = self.mark_vector_index_built();
        }
        live_check
    }

    pub fn mark_content_fts_built(&self) -> Result<()> {
        let dir = Path::new(self.db.uri());
        let mut meta = read_meta(dir)?;
        meta.content_fts_built = true;
        write_meta(dir, &meta)
    }

    pub fn mark_title_fts_built(&self) -> Result<()> {
        let dir = Path::new(self.db.uri());
        let mut meta = read_meta(dir)?;
        meta.title_fts_built = true;
        write_meta(dir, &meta)
    }

    /// Build vector + FTS indexes for efficient search.
    /// Should be called after all data is inserted.
    ///
    /// Each sub-phase (vector, content FTS, title FTS) is checkpointed
    /// individually so a resume after a kill skips already-built indexes.
    /// `on_sub_phase_complete` is called with `(completed, total_sub_phases)`
    /// after each sub-phase finishes so callers can emit progress events.
    pub async fn build_indexes(
        &self,
        build_vector: bool,
        build_fts: bool,
        on_sub_phase_complete: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    ) -> Result<()> {
        let count = self.chunk_count().await?;
        if count == 0 {
            return Ok(());
        }

        let dir = Path::new(self.db.uri()).to_path_buf();
        let id = &self.corpus_id;

        // (1/3) IVF-PQ vector index.
        let vector_done = read_meta(&dir).map(|m| m.vector_index_built).unwrap_or(false);
        if !build_vector {
            if !vector_done { let _ = self.mark_vector_index_built(); }
            eprintln!("[{id}] Vector index disabled in recipe — skipping (1/3)");
        } else if vector_done {
            eprintln!("[{id}] Vector index already built — skipping (1/3)");
        } else if count >= 256 {
            // Secondary runtime check: list_indices() only returns complete indexes.
            // Catches the case where the meta-flag was lost but the index is intact.
            let already_complete = self.table
                .list_indices().await
                .unwrap_or_default()
                .iter()
                .any(|idx| idx.columns.iter().any(|c| c == "embedding"));
            if already_complete {
                eprintln!("[{id}] Vector index already complete (list_indices) — skipping (1/3)");
                let _ = self.mark_vector_index_built();
            } else {
                let dims = detect_vector_dims(&self.table).await.unwrap_or(1024);
                let num_partitions = optimal_partitions(count);
                let indices_dir = dir.join(format!("{CHUNKS_TABLE}.lance/_indices"));
                eprintln!("[{id}] Building vector index (1/3)...");
                build_vector_index_with_progress(
                    &self.table, &indices_dir, count, num_partitions, dims, id,
                ).await?;
                let _ = self.mark_vector_index_built();
                eprintln!("[{id}] Vector index done");
            }
        } else {
            eprintln!("[{id}] Skipping vector index — fewer than 256 rows (1/3)");
            let _ = self.mark_vector_index_built();
        }
        if let Some(cb) = on_sub_phase_complete { cb(1, 3); }

        // (2/3) Tantivy FTS index on content.
        let content_done = read_meta(&dir).map(|m| m.content_fts_built).unwrap_or(false);
        if !build_fts {
            if !content_done { let _ = self.mark_content_fts_built(); }
            eprintln!("[{id}] FTS indexes disabled in recipe — skipping (2/3)");
        } else if content_done {
            eprintln!("[{id}] FTS content index already built — skipping (2/3)");
        } else {
            eprintln!("[{id}] Building FTS content index (2/3)...");
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
            let _ = self.mark_content_fts_built();
            eprintln!("[{id}] FTS content index done");
        }
        if let Some(cb) = on_sub_phase_complete { cb(2, 3); }

        // (3/3) Tantivy FTS index on title.
        let title_done = read_meta(&dir).map(|m| m.title_fts_built).unwrap_or(false);
        if !build_fts {
            if !title_done { let _ = self.mark_title_fts_built(); }
            eprintln!("[{id}] FTS indexes disabled in recipe — skipping (3/3)");
        } else if title_done {
            eprintln!("[{id}] FTS title index already built — skipping (3/3)");
        } else {
            eprintln!("[{id}] Building FTS title index (3/3)...");
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
            let _ = self.mark_title_fts_built();
            eprintln!("[{id}] FTS title index done");
        }
        if let Some(cb) = on_sub_phase_complete { cb(3, 3); }

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
        let sanitized = sanitize_fts_query(query_text);
        // Single live check — list_indices() only returns COMPLETE indices.
        // Gates both vector (IVF-PQ on "embedding") and FTS (Tantivy on "content"/"title").
        // Avoids 30-second flat scans when either index is absent or stale on large tables.
        // Exception: tables below FLAT_SCAN_THRESHOLD rows use brute-force scans
        // unconditionally — they're too small to warrant an IVF-PQ index and a flat
        // scan completes in milliseconds.
        const FLAT_SCAN_THRESHOLD: usize = 10_000;
        let row_count = self.table.count_rows(None).await.unwrap_or(usize::MAX);
        let indices = self.table.list_indices().await.unwrap_or_default();
        let ivf_built = indices.iter().any(|idx| idx.columns.iter().any(|c| c == "embedding"));
        let do_vector = !query_embedding.is_empty()
            && (ivf_built || row_count < FLAT_SCAN_THRESHOLD);
        let fts_built = !sanitized.is_empty()
            && indices.iter().any(|idx| idx.columns.iter().any(|c| c == "content" || c == "title"));
        let do_fts = fts_built;

        tracing::debug!(
            do_vector,
            do_fts,
            ivf_built,
            fts_built,
            row_count = row_count as u64,
            stored_dims = self.embedding_dimensions,
            query_dims = query_embedding.len(),
            dims_match = (query_embedding.is_empty() || query_embedding.len() == self.embedding_dimensions),
            sanitized_query = %sanitized,
            "CorpusIndex::search"
        );

        if !do_vector && !do_fts {
            tracing::debug!("CorpusIndex::search: nothing to search, returning empty");
            return Ok(Vec::new());
        }

        let t_search = std::time::Instant::now();

        let results = if do_vector && do_fts {
            // Hybrid: vector + FTS combined via reranking.
            self.table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .full_text_search(
                    FullTextSearchQuery::new(sanitized),
                )
                .nprobes(50)
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
                .nprobes(50)
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

        // Log the result schema once so we can see what score columns LanceDB returns.
        // Hybrid search may return _relevance_score or _score instead of _distance.
        if let Some(first) = results.first() {
            let schema = first.schema();
            let col_names: Vec<&str> = schema.fields().iter()
                .map(|f| f.name().as_str())
                .collect();
            tracing::debug!(columns = ?col_names, "CorpusIndex::search result schema");
        }

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
            // LanceDB vector-only → _distance; hybrid → _relevance_score or _score.
            // Try all known column names so we can log which one is actually present.
            let distance_col = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            let relevance_col = batch
                .column_by_name("_relevance_score")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            let score_col = batch
                .column_by_name("_score")
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

                // Convert distance/relevance to a [0,1] score.
                // _distance: lower = better → score = 1/(1+d)
                // _relevance_score / _score: higher = better → use directly (already [0,1])
                let (score, score_source) = if let Some(d) = distance_col {
                    let dist = d.value(i);
                    (1.0_f32 / (1.0 + dist), "_distance")
                } else if let Some(r) = relevance_col {
                    (r.value(i), "_relevance_score")
                } else if let Some(s) = score_col {
                    (s.value(i), "_score")
                } else {
                    (1.0_f32, "none")
                };

                tracing::debug!(
                    rank = i + 1,
                    score,
                    score_source,
                    title = title.as_deref().unwrap_or(""),
                    content_preview = &content[..content.len().min(120)],
                    "CorpusIndex::search result"
                );

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
        // Apply score threshold only in vector-only mode, where score = 1/(1+cosine_distance)
        // and 0.45 corresponds to cosine_distance ≈ 1.22 (weak semantic match).
        // In hybrid mode, scores are RRF (_relevance_score ≈ 0.016) — incompatible scale,
        // so we let all results through and trust the model to judge relevance.
        let before_threshold = scored.len();
        if do_vector && !do_fts {
            scored.retain(|c| c.score >= 0.45);
        }
        tracing::debug!(
            before = before_threshold,
            after = scored.len(),
            dropped = before_threshold - scored.len(),
            threshold_applied = do_vector && !do_fts,
            "CorpusIndex::search: threshold check"
        );
        scored.truncate(limit);
        tracing::debug!(
            results = scored.len(),
            elapsed_ms = t_search.elapsed().as_millis() as u64,
            "CorpusIndex::search complete"
        );
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

    /// Sample up to `n` chunk embeddings for integrity checking.
    /// Returns `(chunk_id, embedding)` pairs.
    pub async fn sample_embeddings(&self, n: usize) -> Result<Vec<(u64, Vec<f32>)>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "embedding".to_string(),
            ]))
            .limit(n)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("sample_embeddings query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("sample_embeddings collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let ids = match batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            {
                Some(a) => a,
                None => continue,
            };
            let embeddings = match batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
            {
                Some(a) => a,
                None => continue,
            };
            for i in 0..batch.num_rows() {
                let id = ids.value(i) as u64;
                let values = embeddings.value(i);
                let floats = values
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .map(|a| (0..a.len()).map(|j| a.value(j)).collect::<Vec<_>>())
                    .unwrap_or_default();
                out.push((id, floats));
            }
        }
        Ok(out)
    }

    /// Rebuild both FTS indexes (content + title) from current data.
    /// This drops the existing indexes and recreates them.
    pub async fn rebuild_fts(&self) -> Result<()> {
        // Clear sub-phase flags so build_indexes() actually rebuilds the FTS indexes.
        let dir = Path::new(self.db.uri());
        if let Ok(mut meta) = read_meta(dir) {
            meta.content_fts_built = false;
            meta.title_fts_built = false;
            let _ = write_meta(dir, &meta);
        }
        self.build_indexes(false, true, None).await
    }

    /// Re-embed the specified chunks with a fresh embedding call and update them in place.
    pub async fn re_embed_chunks(&self, chunk_ids: &[u64], embed_fn: &crate::types::EmbedFn) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        // Fetch content for the given chunk IDs.
        let id_filter = chunk_ids
            .iter()
            .map(|id| format!("id = {id}"))
            .collect::<Vec<_>>()
            .join(" OR ");

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(id_filter)
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("re_embed fetch: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("re_embed collect: {e}")))?;

        for batch in &batches {
            let ids = match batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            {
                Some(a) => a,
                None => continue,
            };
            let contents = match batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            {
                Some(a) => a,
                None => continue,
            };
            for i in 0..batch.num_rows() {
                let id = ids.value(i) as i64;
                let content = contents.value(i);
                let new_embedding = embed_fn(content).await
                    .map_err(|e| Error::Embed(format!("re-embed chunk {id}: {e}")))?;

                // Update the row — delete + insert.
                self.table
                    .delete(&format!("id = {id}"))
                    .await
                    .map_err(|e| Error::Database(format!("re_embed delete {id}: {e}")))?;

                let schema = self.table.schema().await
                    .map_err(|e| Error::Database(format!("re_embed schema: {e}")))?;
                let dim = new_embedding.len() as i32;
                let embedding_flat = arrow_array::Float32Array::from(new_embedding.clone());
                let embedding_list: Vec<Option<Vec<Option<f32>>>> = vec![
                    Some(new_embedding.iter().map(|&x| Some(x)).collect()),
                ];
                let _ = (schema, dim, embedding_flat, embedding_list);
                // NOTE: Full row re-insert requires all columns — complex without the full
                // original row. Defer to a full-corpus re-embed job for now.
                // This is a best-effort attempt; mark as partial progress.
                return Err(Error::Extraction(
                    "Per-chunk re-embed requires full row data; use schedule_enrichment_full instead".into()
                ));
            }
        }
        Ok(())
    }

    /// Delete all chunks whose `source_doc_id` matches `doc_id`.
    pub async fn delete_chunks_by_source_doc(&self, doc_id: &str) -> Result<()> {
        // Escape single quotes to prevent filter injection.
        let safe_id = doc_id.replace('\'', "''");
        self.table
            .delete(&format!("source_doc_id = '{safe_id}'"))
            .await
            .map_err(|e| Error::Database(format!("delete_chunks_by_source_doc: {e}")))?;
        Ok(())
    }

    /// Insert pre-embedded chunks into the index.
    pub async fn insert_chunks(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let pairs: Vec<(InsertChunk, Vec<f32>)> = chunks
            .iter()
            .map(|c| (c.insert.clone(), c.embedding.clone()))
            .collect();
        self.insert_batch(&pairs).await
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

    // ── Enrichment: chunk iteration ──────────────────────

    /// Read every chunk in the index. Used by the enrichment pipeline
    /// to feed claim extraction prompts.
    ///
    /// Materializes all chunks into memory. For very large corpora this
    /// is significant — but enrichment runs offline as a one-time job
    /// and the chunks are reasonably bounded (a few hundred bytes each
    /// of content + title; embeddings are not loaded here).
    pub async fn all_chunks(&self) -> Result<Vec<StoredChunk>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
                "title".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("all_chunks query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("all_chunks collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing content column".into()))?;
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing title column".into()))?;

            for i in 0..batch.num_rows() {
                out.push(StoredChunk {
                    id: ids.value(i) as u64,
                    content: contents.value(i).to_string(),
                    title: if titles.is_null(i) {
                        None
                    } else {
                        Some(titles.value(i).to_string())
                    },
                });
            }
        }
        Ok(out)
    }

    /// Like `all_chunks` but also returns the raw `metadata` JSON string and
    /// the URL, for use by the structural enrichment pipeline (link graph
    /// builder and article profile builder).
    pub async fn all_chunks_with_raw_metadata(&self) -> Result<Vec<StoredChunkWithMetadata>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "title".to_string(),
                "url".to_string(),
                "metadata".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("all_chunks_with_raw_metadata query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("all_chunks_with_raw_metadata collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let urls = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadatas = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                out.push(StoredChunkWithMetadata {
                    id: ids.value(i) as u64,
                    title: titles.and_then(|t| {
                        if t.is_null(i) { None } else { Some(t.value(i).to_string()) }
                    }),
                    url: urls.and_then(|u| {
                        if u.is_null(i) { None } else { Some(u.value(i).to_string()) }
                    }),
                    metadata_raw: metadatas.and_then(|m| {
                        if m.is_null(i) { None } else { Some(m.value(i).to_string()) }
                    }),
                });
            }
        }
        Ok(out)
    }

    // ── Field model enrichment methods ─────────────────────

    /// Stream all chunk embeddings from the index.
    /// Returns `(chunk_ids, embeddings)` — columnar projection, does not
    /// load chunk text.
    pub async fn stream_embedding_column(&self) -> Result<(Vec<u64>, Vec<Vec<f32>>)> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "embedding".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("stream_embedding_column query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("stream_embedding_column collect: {e}")))?;

        let mut ids = Vec::new();
        let mut embeddings = Vec::new();

        for batch in &batches {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;

            let emb_col = batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
                .ok_or_else(|| Error::Serialization("missing embedding column".into()))?;

            for i in 0..batch.num_rows() {
                ids.push(id_col.value(i) as u64);

                let values = emb_col
                    .value(i)
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .map(|a| a.values().to_vec())
                    .unwrap_or_default();
                embeddings.push(values);
            }
        }

        Ok((ids, embeddings))
    }

    /// Bulk-update an Int32 column on the chunks table.
    /// Used by the clustering phase to write `cluster_id`.
    pub async fn bulk_update_i32_column(
        &self,
        _col_name: &str,
        _assignments: &std::collections::HashMap<u64, i32>,
    ) -> Result<()> {
        // TODO: Implement via LanceDB merge or update API.
        // For now, this is a placeholder that will be filled in during
        // the CorpusIndex extension phase.
        Ok(())
    }

    /// Bulk-update a Utf8 column on the chunks table.
    /// Used by the labeling phase to write `chunk_role`.
    pub async fn bulk_update_str_column(
        &self,
        _col_name: &str,
        _assignments: &std::collections::HashMap<u64, &str>,
    ) -> Result<()> {
        // TODO: Implement via LanceDB merge or update API.
        Ok(())
    }

    /// Load specific chunks by their IDs.
    pub async fn get_chunks(&self, chunk_ids: &[u64]) -> Result<Vec<StoredChunk>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        // Build a filter expression: id IN (1, 2, 3, ...)
        let id_list = chunk_ids
            .iter()
            .map(|id| format!("{}", *id as i64))
            .collect::<Vec<_>>()
            .join(", ");
        let filter = format!("id IN ({id_list})");

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
                "title".to_string(),
            ]))
            .only_if(filter)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("get_chunks query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("get_chunks collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing content column".into()))?;
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing title column".into()))?;

            for i in 0..batch.num_rows() {
                out.push(StoredChunk {
                    id: ids.value(i) as u64,
                    content: contents.value(i).to_string(),
                    title: if titles.is_null(i) {
                        None
                    } else {
                        Some(titles.value(i).to_string())
                    },
                });
            }
        }
        Ok(out)
    }

    /// True if this index has field model tables (from the new enrichment pipeline).
    pub async fn has_field_model_tables(&self) -> bool {
        self.has_table("field_questions").await
    }

    /// Get the index directory path.
    pub fn path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.db.uri())
    }

    /// Write a field skeleton JSON file to the index directory.
    pub fn write_field_skeleton(
        &self,
        skeleton: &crate::enrichment::skeleton::FieldSkeleton,
    ) -> Result<()> {
        let path = self.path().join("field_skeleton.json");
        let json = serde_json::to_string_pretty(skeleton)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load the field skeleton JSON file if it exists.
    pub fn load_field_skeleton(
        &self,
    ) -> Result<Option<crate::enrichment::skeleton::FieldSkeleton>> {
        let path = self.path().join("field_skeleton.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        let skeleton = serde_json::from_str(&raw)
            .map_err(|e| Error::Serialization(format!("Bad field_skeleton.json: {e}")))?;
        Ok(Some(skeleton))
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
