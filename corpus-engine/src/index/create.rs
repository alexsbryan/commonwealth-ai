//! Index creation, resumption, and index-building logic.

use std::path::Path;

use super::*;

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

// ─── CorpusIndex creation & index-building methods ────────

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
}
