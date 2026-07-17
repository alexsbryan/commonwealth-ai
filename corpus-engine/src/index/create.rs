// SPDX-License-Identifier: AGPL-3.0-or-later
//! Index creation, resumption, and index-building logic.

use std::path::Path;

use super::*;

// ─── Vector index helpers ──────────────────────────────────

/// Compute IVF partition count: sqrt(n), clamped 8–4096.
/// LanceDB Auto uses the same heuristic; making it explicit lets us log it.
fn optimal_partitions(num_chunks: u64) -> u32 {
    ((num_chunks as f64).sqrt() as u32).clamp(8, 4096)
}

/// Read the embedding column's fixed-list dimension from the table schema.
async fn detect_vector_dims(table: &lancedb::Table) -> Result<usize> {
    use arrow::datatypes::DataType;
    let schema = table
        .schema()
        .await
        .map_err(|e| Error::Database(format!("schema: {e}")))?;
    for field in schema.fields() {
        if field.name() == "embedding" {
            if let DataType::FixedSizeList(_, dims) = field.data_type() {
                return Ok(*dims as usize);
            }
        }
    }
    Err(Error::Database(
        "embedding column not found or not FixedSizeList".into(),
    ))
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
                let pct =
                    ((dir_bytes as f64 / estimated_bytes as f64) * 100.0).clamp(0.0, 99.0) as i32;
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
    ///
    /// Back-compat wrapper: passes `query_sharing = None`, which
    /// resolves at open-time to whatever `mesh_sharing` is —
    /// preserving pre-split behavior. New callers who know they want
    /// a different value should use `create_with_sharing`.
    pub async fn create(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        license: &str,
    ) -> Result<Self> {
        Self::create_with_sharing(
            path,
            corpus_id,
            corpus_name,
            embedding_model,
            embedding_dim,
            mesh_sharing,
            None,
            license,
        )
        .await
    }

    /// Create a new LanceDB index with explicit `query_sharing`.
    /// Used by the ingest pipeline, which reads the recipe's value.
    pub async fn create_with_sharing(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        query_sharing: Option<bool>,
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
            query_sharing,
            // Stamped post-create from the recipe via `set_dedup_by_source`
            // (mirrors `set_display`); `None` here = baseline at creation.
            dedup_by_source: None,
            // Stamped post-create via `set_personal_scope` (same shape).
            personal_scope: None,
            // Stamped post-create from `[corpus] grantable` via `set_grantable`.
            grantable: None,
            license: license.to_string(),
            created_at: now,
            last_updated: now,
            schema_version: super::CURRENT_INDEX_SCHEMA_VERSION,
            source_path: None,
            is_shard: false,
            chunk_range_start: None,
            chunk_range_end: None,
            ingestion_in_progress: true,
            committed_iter_pos: 0,
            indexes_built: false,
            vector_index_built: false,
            content_fts_built: false,
            title_fts_built: false,
            chunks_deduped: false,
            // Fresh index: first inserted chunk gets id 1. Bumped by the
            // batch size on every insert; never reused (see the field doc).
            next_chunk_id: Some(1),
            chunks_expected: None,
            resume_from: None,
            enrichment_enabled: false,
            enriched_chunks: None,
            source_version: None,
            update_manifest_url: None,
            processed_shards: Vec::new(),
            total_shards: None,
            committed_shard_set: None,
            scope: None,
            filter_override: None,
            provenance: super::CorpusProvenance::default(),
            kind: None,
            parent_corpus_id: None,
            // Stamped at canonical-write time by
            // `compute_and_stamp_fingerprint()` after ingestion is
            // complete. Leaving None during ingestion is correct —
            // the chunk set is still mutating.
            canonical_fingerprint: None,
            // Set lazily via `set_mutable_merge` once the recipe's
            // policy (or the first input shard's, in the merge path)
            // is known. None preserves classic content-hash dedupe.
            mutable_merge: None,
            // Set lazily by `sovereign corpus stream-axes` once the
            // recipe is resolved post-ingest. Move 5 Stage 2 wires
            // this; v1 leaves the create path untouched.
            stream: None,
            // Stamped by `set_display` from the ingest path right
            // after create when the recipe carries a `[display]` block.
            display: None,
            // Move 6 P5.b/c — opt-in by the user via
            // `set_atlas_incremental_enabled(true)` after migrating
            // the corpus's atoms.json to content-hash ids.
            atlas_incremental_enabled: None,
        };
        write_meta(path, &meta)?;

        Ok(Self {
            db,
            table,
            corpus_id: corpus_id.to_string(),
            embedding_dimensions: embedding_dim,
            gate_cache: Default::default(),
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
        Self::create_or_resume_with_sharing(
            path,
            corpus_id,
            corpus_name,
            embedding_model,
            embedding_dim,
            mesh_sharing,
            None,
            license,
        )
        .await
    }

    /// Like `create_or_resume`, but takes an explicit `query_sharing`
    /// flag — the ingest pipeline passes the recipe's value through
    /// so SEP (and future cite-but-don't-redistribute corpora) are
    /// queryable from peers without being replicable.
    pub async fn create_or_resume_with_sharing(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        query_sharing: Option<bool>,
        license: &str,
    ) -> Result<(Self, u64)> {
        // Resume path: an index exists at this location.
        //
        // We deliberately accept BOTH "ingest in progress" (the
        // historical resume case — a previous run was killed) AND
        // "ingest complete" (a successfully-finished corpus the
        // operator has chosen to re-trigger). The latter is the
        // drift-recovery and expand-corpus path: the chunks table
        // already exists, the iter_pos may be stale, and the in-loop
        // drift detection + skipset machinery handle the rest. Trying
        // to "fresh-create" here would hit `Table 'chunks' already
        // exists` from LanceDB and tank the recovery — so once we
        // know the directory is openable, we open it and let the
        // ingest loop decide what to do with the existing state.
        if path.exists() {
            match Self::open(path).await {
                Ok(index) => {
                    let iter_pos = read_meta(path).map(|m| m.committed_iter_pos).unwrap_or(0);
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

        // Fresh start. Pass the resolved query_sharing through so a
        // freshly-written `_corpus_meta.json` records the recipe's
        // intent explicitly — no more guessing from mesh_sharing at
        // open-time for this index.
        let index = Self::create_with_sharing(
            path,
            corpus_id,
            corpus_name,
            embedding_model,
            embedding_dim,
            mesh_sharing,
            query_sharing,
            license,
        )
        .await?;
        Ok((index, 0))
    }

    /// Open or create an index for a unit-scoped ingest (pull-queue worker).
    ///
    /// Unlike `create_or_resume_with_sharing`, this:
    ///   - Does NOT read `committed_iter_pos` — unit-scoped source
    ///     iterators are already bounded by the caller's `file_indices`
    ///     / `article_range` overrides, so a global resume cursor would
    ///     skip most of the unit's work.
    ///   - Does NOT fail when `_corpus_meta.json` reports
    ///     `ingestion_in_progress: false`. Multiple units land in the
    ///     same per-node partition dir; only the merge leader flips
    ///     the flag, so later units must tolerate a "complete"-looking
    ///     meta and keep appending.
    ///
    /// If the partition dir exists with a valid chunks table, this opens
    /// it in append mode. Otherwise a fresh index is created with
    /// `ingestion_in_progress: true`.
    pub async fn create_or_open_for_unit(
        path: &Path,
        corpus_id: &str,
        corpus_name: &str,
        embedding_model: &str,
        embedding_dim: usize,
        mesh_sharing: bool,
        query_sharing: Option<bool>,
        license: &str,
    ) -> Result<Self> {
        if path.exists() && read_meta(path).is_ok() {
            match Self::open(path).await {
                Ok(index) => {
                    eprintln!(
                        "[corpus] Opening unit-scoped partition '{}' at {} (append)",
                        corpus_id,
                        path.display(),
                    );
                    return Ok(index);
                }
                Err(e) => {
                    // Corrupt partition — wipe and start fresh. Losing a
                    // partial partition is safe: the merge leader dedupes
                    // chunks by (chunk_id, unit_id) across all peers.
                    tracing::warn!(
                        "Unit-scoped partition at '{}' could not be opened ({e}); starting fresh",
                        path.display()
                    );
                    if let Err(rm) = std::fs::remove_dir_all(path) {
                        tracing::warn!("Failed to remove corrupt partition: {rm}");
                    }
                }
            }
        }

        Self::create_with_sharing(
            path,
            corpus_id,
            corpus_name,
            embedding_model,
            embedding_dim,
            mesh_sharing,
            query_sharing,
            license,
        )
        .await
    }

    /// Persist the current iterator position as a resume checkpoint.
    /// Called after each successful batch flush so that a subsequent restart
    /// can skip already-committed source documents.
    ///
    /// Prefer [`Self::update_committed_iter_pos_with_shards`] from any
    /// caller that knows the assigned shard set — without it, a
    /// resume cannot detect the coordinate-space drift that occurs
    /// when `processed_shards` mutates between runs (see the
    /// `committed_shard_set` field on `IndexMeta` for the full
    /// rationale). This signature is kept for the `expand_corpus`
    /// reset path and any caller that is genuinely shard-agnostic.
    pub fn update_committed_iter_pos(&self, iter_pos: u64) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.committed_iter_pos = iter_pos;
        write_meta(index_dir, &meta)
    }

    /// Same as [`Self::update_committed_iter_pos`] but additionally
    /// records the assigned-shard set the iterator was constructed
    /// with at the start of this run. The saved set is what later
    /// resumes compare against to detect coordinate-space drift.
    /// Pass `None` for non-sharded ingests (the canonical default
    /// signal: the iter_pos space is implicit).
    pub fn update_committed_iter_pos_with_shards(
        &self,
        iter_pos: u64,
        shard_set: Option<&[usize]>,
    ) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.committed_iter_pos = iter_pos;
        meta.committed_shard_set = shard_set.map(|s| s.to_vec());
        write_meta(index_dir, &meta)
    }

    /// Read the shard set associated with the saved `committed_iter_pos`.
    /// Returns `None` for legacy indexes that pre-date the field.
    pub fn committed_shard_set(&self) -> Result<Option<Vec<usize>>> {
        let index_dir = Path::new(self.db.uri());
        let meta = read_meta(index_dir)?;
        Ok(meta.committed_shard_set)
    }

    /// Mark a zip shard as fully committed. Idempotent — adding a
    /// shard index that is already recorded is a no-op. The resulting
    /// list is stored sorted to keep JSON diffs deterministic.
    ///
    /// Called by the ingest pipeline at each shard boundary so the
    /// collaborative-ingestion coordinator can compute the set of
    /// still-outstanding shards when planning a partition.
    pub fn record_processed_shard(&self, shard_index: usize) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        if !meta.processed_shards.contains(&shard_index) {
            meta.processed_shards.push(shard_index);
            meta.processed_shards.sort_unstable();
        }
        write_meta(index_dir, &meta)
    }

    /// Read the set of fully-committed zip shard indices for this index.
    pub fn processed_shards(&self) -> Result<Vec<usize>> {
        let index_dir = Path::new(self.db.uri());
        let meta = read_meta(index_dir)?;
        Ok(meta.processed_shards)
    }

    // ─── Scope / filter metadata ──────────────────────────────
    //
    // The scope block records which filter pipeline produced this
    // index — set at ingest time so the UI can offer "Expand to full
    // <corpus>" when a relaxed scope makes sense.

    /// Read the active scope, if any.
    pub fn read_scope(&self) -> Result<Option<super::ScopeMeta>> {
        let index_dir = Path::new(self.db.uri());
        let meta = read_meta(index_dir)?;
        Ok(meta.scope)
    }

    /// Write or replace the scope block.
    pub fn write_scope(&self, scope: Option<super::ScopeMeta>) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.scope = scope;
        write_meta(index_dir, &meta)
    }

    /// Read the in-flight filter override, if any.
    pub fn read_filter_override(&self) -> Result<Option<super::FilterOverride>> {
        let index_dir = Path::new(self.db.uri());
        let meta = read_meta(index_dir)?;
        Ok(meta.filter_override)
    }

    /// Write or clear the in-flight filter override. Set by
    /// `expand_corpus` so a restart resumes the expansion with the
    /// relaxed scope; cleared once expansion completes.
    pub fn write_filter_override(&self, ovr: Option<super::FilterOverride>) -> Result<()> {
        let index_dir = Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.filter_override = ovr;
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
        read_meta(path).map(|m| m.indexes_built).unwrap_or(false)
    }

    /// Reset the meta flags so a subsequent `ingest()` treats this
    /// index as actively in-progress and rebuilds search indexes.
    ///
    /// Concretely: clears `indexes_built`, `vector_index_built`,
    /// `content_fts_built`, `title_fts_built`, and flips
    /// `ingestion_in_progress` back to `true`. Leaves committed data
    /// (chunks, processed_shards, committed_iter_pos) untouched, so
    /// resume will skip already-processed work.
    ///
    /// Two callers today:
    /// - Drift recovery: post-embed build phase needs to rebuild
    ///   IVF-PQ + FTS over the union of existing + newly-embedded
    ///   chunks. Without this reset, `indexes_are_built()` would
    ///   short-circuit the build and leave recovery chunks
    ///   unsearchable.
    /// - `corpus repair` CLI: a partition that completed with
    ///   missing shards (e.g. resume-cursor-rewind bug) is
    ///   marked done. Repair flips state back to in-progress so
    ///   auto-resume / install picks it up. The embed-side dedup
    ///   gate makes this safe — already-embedded content_hashes
    ///   are skipped on the next pass.
    pub fn reset_for_resume(&self) -> Result<()> {
        let index_dir = std::path::Path::new(self.db.uri());
        let mut meta = read_meta(index_dir)?;
        meta.indexes_built = false;
        meta.vector_index_built = false;
        meta.content_fts_built = false;
        meta.title_fts_built = false;
        meta.ingestion_in_progress = true;
        write_meta(index_dir, &meta)
    }

    /// Legacy alias for `reset_for_resume`. Drift-recovery callers
    /// use this name; the body is identical.
    pub fn reset_for_drift_recovery(&self) -> Result<()> {
        self.reset_for_resume()
    }

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

    /// Stamp the total number of source shards the extractor will
    /// process for this corpus. Idempotent: writing the same value
    /// is a no-op; writing a different value replaces (the
    /// extractor knows the real count once it inspects the source
    /// archive, so we trust the caller here).
    ///
    /// Called once at extract start by sharded extractors. Diag
    /// reads it via `total_shards()` to compute missing-shard
    /// coverage that doesn't undercount trailing-missing shards.
    pub fn set_total_shards(&self, total: usize) -> Result<()> {
        let dir = Path::new(self.db.uri());
        let mut meta = read_meta(dir)?;
        meta.total_shards = Some(total);
        write_meta(dir, &meta)
    }

    /// Read the total-shards field. `None` for non-sharded corpora
    /// and for legacy indexes that pre-date the field; in that case
    /// callers should fall back to the `max(processed_shards)+1`
    /// heuristic with an explicit "trailing shards may be missing"
    /// caveat.
    pub fn total_shards(&self) -> Option<usize> {
        let dir = Path::new(self.db.uri());
        read_meta(dir).ok().and_then(|m| m.total_shards)
    }

    /// Persist that the pre-build dedupe pass has run for this
    /// index. Read by `build_indexes()` to avoid a second
    /// (no-op) full table scan on resume.
    pub fn mark_chunks_deduped(&self) -> Result<()> {
        let dir = Path::new(self.db.uri());
        let mut meta = read_meta(dir)?;
        meta.chunks_deduped = true;
        write_meta(dir, &meta)
    }

    /// Whether the dedupe pass has been recorded as complete for
    /// this index. False for legacy indexes that pre-date the
    /// `chunks_deduped` field; those will dedupe on their next
    /// `build_indexes()` call (no-op if already clean).
    pub fn is_chunks_deduped(&self) -> bool {
        let dir = Path::new(self.db.uri());
        read_meta(dir).map(|m| m.chunks_deduped).unwrap_or(false)
    }

    /// Build a BTree scalar index on `title` so `only_if("title = …")`
    /// predicates (`fetch_chunks_by_title` — the dominant-source and
    /// structural-expansion fetch path) use an index seek instead of a
    /// filtered full scan (measured ~450-500 ms per call on the 1.9M-row
    /// wikipedia table, 2026-07-17). Idempotent: LanceDB replaces an
    /// existing index on the same column.
    pub async fn build_title_scalar_index(&self) -> Result<()> {
        self.table
            .create_index(
                &["title"],
                lancedb::index::Index::BTree(
                    lancedb::index::scalar::BTreeIndexBuilder::default(),
                ),
            )
            .execute()
            .await
            .map_err(|e| Error::Database(format!("BTree title index: {e}")))?;
        // Index set changed — drop the cached search gate (same
        // contract as build_indexes).
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = None;
        }
        Ok(())
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
        // Index existence is about to change — drop the cached search
        // gate so post-build searches see the new IVF/FTS immediately
        // (a stale "no IVF" on a >10k-row corpus silently skips the
        // vector leg; see the gate_cache field doc).
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = None;
        }

        let dir = Path::new(self.db.uri()).to_path_buf();
        let id = &self.corpus_id;

        // (0/3) Pre-build dedupe pass. The resume-cursor-rewind
        // bug seen in the wild leaves up to ~65% duplicate-content
        // rows; vector training over duplicates wastes time AND
        // poisons retrieval (top-k saturates with near-identical
        // chunks). Running dedupe BEFORE training fixes both.
        //
        // Idempotent: a clean index produces a no-op DedupeReport
        // and the cost is one bounded table scan. We still flag
        // the run as complete via `chunks_deduped` so a resume
        // (e.g. process killed between dedupe and vector build)
        // doesn't re-scan.
        if !self.is_chunks_deduped() {
            eprintln!("[{id}] Pre-build dedupe pass (0/3)...");
            match self.dedupe_by_content_hash().await {
                Ok(report) => {
                    if report.changed() {
                        let pct = report.dup_fraction() * 100.0;
                        eprintln!(
                            "[{id}] Dedupe collapsed {} duplicate rows ({pct:.2}% \
                             of hashed) — {} unique chunks remain",
                            report.duplicates_deleted, report.rows_after,
                        );
                    } else {
                        eprintln!(
                            "[{id}] Dedupe: no duplicates found ({} rows)",
                            report.rows_after
                        );
                    }
                    if report.hashless_rows_preserved > 0 {
                        eprintln!(
                            "[{id}] Dedupe preserved {} legacy hashless rows \
                             (no content_hash to compare)",
                            report.hashless_rows_preserved
                        );
                    }
                    let _ = self.mark_chunks_deduped();
                }
                Err(e) => {
                    // Don't abort the build — dedupe is a polish
                    // pass, not load-bearing. Log + continue.
                    eprintln!("[{id}] Warning: dedupe scan failed ({e}); proceeding with build");
                }
            }
        } else {
            eprintln!("[{id}] Dedupe already recorded — skipping (0/3)");
        }

        // (1/3) IVF-PQ vector index.
        let vector_done = read_meta(&dir)
            .map(|m| m.vector_index_built)
            .unwrap_or(false);
        if !build_vector {
            if !vector_done {
                let _ = self.mark_vector_index_built();
            }
            eprintln!("[{id}] Vector index disabled in recipe — skipping (1/3)");
        } else if vector_done {
            eprintln!("[{id}] Vector index already built — skipping (1/3)");
        } else if count >= 256 {
            // Secondary runtime check: list_indices() only returns complete indexes.
            // Catches the case where the meta-flag was lost but the index is intact.
            let already_complete = self
                .table
                .list_indices()
                .await
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
                    &self.table,
                    &indices_dir,
                    count,
                    num_partitions,
                    dims,
                    id,
                )
                .await?;
                let _ = self.mark_vector_index_built();
                eprintln!("[{id}] Vector index done");
            }
        } else {
            eprintln!("[{id}] Skipping vector index — fewer than 256 rows (1/3)");
            let _ = self.mark_vector_index_built();
        }
        if let Some(cb) = on_sub_phase_complete {
            cb(1, 3);
        }

        // (2/3) Tantivy FTS index on content.
        let content_done = read_meta(&dir)
            .map(|m| m.content_fts_built)
            .unwrap_or(false);
        if !build_fts {
            // Do NOT mark FTS as built when skipping — that would corrupt
            // metadata and prevent a future build_indexes(true, true) from
            // actually building the index.
            eprintln!("[{id}] FTS indexes not requested — skipping (2/3)");
        } else if content_done {
            eprintln!("[{id}] FTS content index already built — skipping (2/3)");
        } else {
            eprintln!("[{id}] Building FTS content index (2/3)...");
            self.table
                .create_index(
                    &["content"],
                    lancedb::index::Index::FTS(lancedb::index::scalar::FtsIndexBuilder::default()),
                )
                .execute()
                .await
                .map_err(|e| Error::Database(format!("FTS content index: {e}")))?;
            let _ = self.mark_content_fts_built();
            eprintln!("[{id}] FTS content index done");
        }
        if let Some(cb) = on_sub_phase_complete {
            cb(2, 3);
        }

        // (3/3) Tantivy FTS index on title.
        let title_done = read_meta(&dir).map(|m| m.title_fts_built).unwrap_or(false);
        if !build_fts {
            eprintln!("[{id}] FTS indexes not requested — skipping (3/3)");
        } else if title_done {
            eprintln!("[{id}] FTS title index already built — skipping (3/3)");
        } else {
            eprintln!("[{id}] Building FTS title index (3/3)...");
            self.table
                .create_index(
                    &["title"],
                    lancedb::index::Index::FTS(lancedb::index::scalar::FtsIndexBuilder::default()),
                )
                .execute()
                .await
                .map_err(|e| Error::Database(format!("FTS title index: {e}")))?;
            let _ = self.mark_title_fts_built();
            eprintln!("[{id}] FTS title index done");
        }
        if let Some(cb) = on_sub_phase_complete {
            cb(3, 3);
        }

        Ok(())
    }
}
