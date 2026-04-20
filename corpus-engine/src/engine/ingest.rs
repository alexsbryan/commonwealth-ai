//! Ingestion pipeline — acquire, extract, chunk, embed, index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;

use crate::acquirers::bulk_download::BulkDownloader;
use crate::acquirers::huggingface::HuggingFaceDatasetAcquirer;
use crate::acquirers::local_file::LocalFileAcquirer;
use crate::chunkers::{self, Chunker};
use crate::error::{Error, Result};
use crate::extractors::{self, Extractor};
use crate::index::{CorpusIndex, InsertChunk};
use crate::progress::{IngestProgress, ProgressCallback, SourceFileManifest, SourceFileStatus};
use crate::recipe::{AcquirerConfig, ChunkerConfig, ExtractorConfig, Recipe};
use crate::types::{CorpusSpec, IngestResult};

use super::{blake3_hex, normalize_content, CorpusEngine, EMBED_BATCH_SIZE, INDEX_FLUSH_SIZE};

impl CorpusEngine {
    /// Ingest a corpus from source. Downloads, parses, chunks,
    /// embeds, and writes a complete index.
    ///
    /// Failure modes are surfaced cleanly:
    ///
    /// 1. **Pre-flight check.** Before touching disk, the engine asks
    ///    the configured `EmbedFn` to embed a tiny smoke string. If
    ///    that fails (most commonly because no embedding model is
    ///    configured), the error is returned immediately and no
    ///    index directory is created. This prevents the "ghost
    ///    install" state where a half-built directory makes the UI
    ///    think a corpus is installed.
    ///
    /// 2. **Cleanup on failure.** If any step *after* the pre-flight
    ///    fails (download error, parquet schema mismatch, embed
    ///    overflow mid-batch, …), the partial index directory is
    ///    deleted before the error propagates so the corpus appears
    ///    as "not installed" in the UI on the next refresh.
    pub async fn ingest(
        &self,
        corpus: &CorpusSpec,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        let mut recipe = self.resolve_recipe(corpus).await?;

        // Ensure parent directory exists so `_downloads` and the
        // per-corpus index dir can be created underneath.
        std::fs::create_dir_all(&self.index_dir)?;

        // ── Pre-flight: validate the embed function works ─────────
        //
        // We do this before creating the index directory so a missing
        // or broken embedder fails fast with no on-disk side effects.
        // The smoke string is short and the result is discarded —
        // we only care that the call returns Ok and produces a vector
        // of the expected dimensionality.
        let probe = (self.embed)("probe").await.map_err(|e| {
            Error::Embed(format!(
                "Embedding function is not available: {e}. \
                 Configure an embedding model before installing corpora."
            ))
        })?;
        if probe.is_empty() {
            return Err(Error::Embed(
                "Embedding function returned an empty vector. \
                 The configured embed model may be misloaded."
                    .to_string(),
            ));
        }
        // Auto-adapt: use the model's actual output dimensionality.
        // embedding_dimensions = 0 means auto-detect (the default when
        // the recipe omits the field). Only log if the recipe explicitly
        // specified a different value.
        if recipe.index.embedding_dimensions == 0 {
            recipe.index.embedding_dimensions = probe.len();
        } else if probe.len() != recipe.index.embedding_dimensions {
            tracing::info!(
                "Embedding model returns {} dimensions; recipe specified {}. \
                 Using actual model dimensions.",
                probe.len(),
                recipe.index.embedding_dimensions,
            );
            recipe.index.embedding_dimensions = probe.len();
        }

        // ── Choose output path ─────────────────────────────────────
        //
        // Unified primitive: all new ingests write to the per-node
        // partition directory (`<corpus>-partition-<self>/`). The
        // canonical `<corpus>/` directory is materialised only by
        // `finalise_solo_ingest` (single-shard rename) or by
        // `ShardManager::coordinate_merge` (peers participated).
        //
        // Compatibility shim: if the canonical directory already
        // exists with committed data AND no partition-of-self exists,
        // we stay on the legacy in-place path — the user has partial
        // work from a pre-unification install that would otherwise
        // be invisible under the new flow. They can complete the
        // legacy ingest or `remove_corpus_everything` and restart
        // under the new flow. New installs and fresh resumes from
        // peers always use the partition path.
        let corpus_id = recipe.corpus.id.clone();
        let canonical = self.index_dir.join(&corpus_id);
        let self_partition = self.partition_path(&corpus_id);
        let legacy_resume = canonical.exists()
            && CorpusIndex::has_committed_data(&canonical)
            && !self_partition.exists();
        let index_path = if legacy_resume {
            tracing::info!(
                corpus_id,
                path = %canonical.display(),
                "ingest: legacy canonical with committed data — resuming in place (no partition split)"
            );
            canonical.clone()
        } else {
            self_partition.clone()
        };

        // For multi-shard JSONL corpora, restrict this ingest to the
        // shards that have NOT already been recorded as processed
        // anywhere on disk (canonical + partition-of-self + peer
        // partitions from a prior run). Missing or single-shard
        // sources keep `shard_indices = None`, preserving the
        // legacy extractor behaviour that reads everything.
        if !legacy_resume {
            let shard_count = self.jsonl_source_shard_count(&corpus_id).unwrap_or(1);
            if shard_count > 1 {
                let processed: std::collections::HashSet<usize> = self
                    .corpus_processed_shards(&corpus_id)
                    .into_iter()
                    .collect();
                let remaining: Vec<usize> = (0..shard_count)
                    .filter(|i| !processed.contains(i))
                    .collect();
                if remaining.is_empty() {
                    tracing::info!(
                        corpus_id,
                        shard_count,
                        "ingest: all shards already processed — finaliser will promote"
                    );
                }
                apply_jsonl_shard_override(&mut recipe, Some(remaining));
            }
        }

        // ── Run the actual pipeline with cleanup-on-failure ───────
        // Solo `ingest()` never runs under a work-queue lease — unit_id
        // stamping only happens for `ingest_with_overrides` callers that
        // explicitly thread a UnitId in.
        let result = self
            .ingest_inner(&recipe, &index_path, &progress, None)
            .await;

        // On successful completion of a new-flow ingest, attempt to
        // promote the partition to canonical. If peer partitions are
        // already present (collaborative run), the finaliser defers
        // to `ShardManager::coordinate_merge`; we log and return the
        // partition IngestResult unchanged.
        let result = match result {
            Ok(r) if !legacy_resume => match self.finalise_solo_ingest(&corpus_id) {
                Ok(true) => {
                    tracing::info!(
                        corpus_id,
                        "ingest: promoted partition-of-self to canonical (solo run)"
                    );
                    Ok(r)
                }
                Ok(false) => {
                    tracing::info!(
                        corpus_id,
                        "ingest: left partition on disk (peer partitions present or canonical already exists)"
                    );
                    Ok(r)
                }
                Err(e) => {
                    tracing::warn!(
                        corpus_id,
                        error = %e,
                        "ingest: finalise_solo_ingest failed — partition-of-self left in place"
                    );
                    Ok(r)
                }
            },
            other => other,
        };

        match result {
            Ok(r) => Ok(r),
            Err(Error::Cancelled(corpus_id)) => {
                // User-initiated cancel: the Desktop "Cancel" handler
                // (via POST /internal/corpus/cancel) is responsible for
                // calling `remove_corpus_everything` once the task exits.
                // We must NOT wipe here because that would race the
                // caller's own wipe and could swallow an in-flight
                // recreation (e.g. a second install fired immediately
                // after Cancel before the handler's wipe landed).
                tracing::info!(
                    corpus = %corpus_id,
                    "ingest cancelled — caller owns cleanup"
                );
                Err(Error::Cancelled(corpus_id))
            }
            Err(e) => {
                if index_path.exists() {
                    if CorpusIndex::has_committed_data(&index_path) {
                        // Committed chunks exist — preserve the partial index so
                        // the user can resume without re-embedding everything.
                        tracing::info!(
                            "Corpus '{}' install failed ({e}), but committed chunks exist — preserving for resume",
                            recipe.corpus.id,
                        );
                        eprintln!(
                            "[{}] Install failed ({e}). Committed chunks are preserved — re-install to resume.",
                            recipe.corpus.id,
                        );
                    } else {
                        // No chunks committed — fresh install failed early. Safe to wipe.
                        if let Err(rm) = std::fs::remove_dir_all(&index_path) {
                            tracing::warn!(
                                "Failed to clean up partial index at {}: {rm}",
                                index_path.display()
                            );
                        }
                    }
                }
                Err(e)
            }
        }
    }

    /// The actual ingest pipeline. Pulled into its own function so the
    /// public `ingest()` can wrap it with cleanup-on-failure logic.
    ///
    /// `unit_id` — when this run is executing a leased work-queue unit,
    /// the caller threads the `UnitId` through so every chunk written to
    /// LanceDB is stamped with it. `None` for legacy static-partition
    /// ingests and local Desktop-driven installs.
    async fn ingest_inner(
        &self,
        recipe: &Recipe,
        index_path: &Path,
        progress: &Option<ProgressCallback>,
        unit_id: Option<u32>,
    ) -> Result<IngestResult> {
        let start = Instant::now();

        // Step 1: Acquire source data.
        let download_dir = self.index_dir.join("_downloads");
        let source_path = self
            .acquire_source(recipe, &download_dir, progress)
            .await?;

        // Step 2: Extract documents.
        let extractor = self.make_extractor(&recipe.extract);
        let doc_iter = extractor.extract(&source_path)?;

        // Step 3: Chunk, embed, and index.
        let chunker = self.make_chunker(&recipe.chunk);

        // Open or resume a partial index (supports resuming after process kill).
        let (index, resume_iter_pos) = CorpusIndex::create_or_resume_with_sharing(
            index_path,
            &recipe.corpus.id,
            &recipe.corpus.name,
            // Use the engine's actual embedding model name (derived from the
            // configured file path), not the recipe's hardcoded default string.
            &self.expected_embedding_model,
            recipe.index.embedding_dimensions,
            recipe.corpus.mesh_sharing,
            recipe.corpus.query_sharing,
            &recipe.corpus.license,
        )
        .await?;

        // Initialise counters. On resume these start from where we left off.
        let mut total_chunks = index.chunk_count().await.unwrap_or(0);
        let mut docs_processed = 0u64; // successful docs in THIS run
        let mut docs_skipped = 0u64;   // docs skipped due to extraction errors this run
        let mut iter_pos = 0u64;       // absolute position in the source iterator

        // ── Source-file manifest tracking ─────────────────────────────────
        //
        // When the extractor sets `source_file` on each `ExtractedDoc` (e.g.
        // the HuggingFace parquet extractor), we track file boundaries and
        // write `_source_manifest.json` after each tier-2 flush.
        //
        // `file_boundary_iter_pos`: maps filename → iter_pos of the last doc
        // from that file. We populate this when `source_file` transitions from
        // file A to file B (i.e. file A's last doc was the previous doc).
        //
        // After `update_committed_iter_pos(iter_pos)` at each flush, any file
        // whose `boundary <= iter_pos` is now fully committed to LanceDB.
        let mut source_manifest: Option<SourceFileManifest> = SourceFileManifest::load(index_path)
            .unwrap_or(None);
        let mut file_boundary_iter_pos: HashMap<String, u64> = HashMap::new();
        let mut prev_source_file: Option<String> = None;
        // Per-file chunk counters: filename → chunks pushed to pending_chunks.
        let mut chunks_per_file: HashMap<String, u64> = HashMap::new();
        // Per-file chunk counters for chunks already flushed (committed to LanceDB).
        let mut flushed_chunks_per_file: HashMap<String, u64> = HashMap::new();
        // Track which shard indices have already been recorded in
        // `processed_shards` this run so we don't rewrite the meta on
        // every flush after the boundary passes.
        let mut recorded_shards: std::collections::HashSet<usize> = index
            .processed_shards()
            .unwrap_or_default()
            .into_iter()
            .collect();

        // Per-run embed batch size — can be tuned per machine via env var
        // without a rebuild. Lower values reduce Metal GPU pressure at the
        // cost of slightly more Rust-to-GPU round trips.
        let embed_batch_size: usize = std::env::var("SOVEREIGN_EMBED_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(EMBED_BATCH_SIZE);

        // Two-tier buffering:
        //  1. pending_chunks/texts: accumulate until embed_batch_size, then embed
        //  2. index_buffer: accumulate embedded chunks until INDEX_FLUSH_SIZE, then write
        // This decouples embedding frequency from LanceDB insert frequency,
        // drastically reducing fragment count and compaction stalls.
        let mut pending_chunks: Vec<InsertChunk> = Vec::new();
        let mut pending_texts: Vec<String> = Vec::new();
        let mut index_buffer: Vec<(InsertChunk, Vec<f32>)> = Vec::new();
        let mut embed_timer = Instant::now();

        let use_batch_embed = self.batch_embed.is_some();
        // Always log the pipeline config so resume runs also confirm which
        // embed_batch_size is active — important when per-machine tuning
        // via SOVEREIGN_EMBED_BATCH_SIZE is in play and the operator needs
        // to verify their env var reached the launchd-managed daemon.
        let resuming = resume_iter_pos > 0;
        tracing::info!(
            corpus = %recipe.corpus.id,
            embed_batch = embed_batch_size,
            index_flush = INDEX_FLUSH_SIZE,
            batch_embed = use_batch_embed,
            resuming,
            resume_iter_pos,
            "Starting embed+index pipeline"
        );
        eprintln!(
            "[{}] {} embed+index pipeline (embed_batch={}, index_flush={}, batch_embed={}){}",
            recipe.corpus.id,
            if resuming { "Resuming" } else { "Starting" },
            embed_batch_size,
            INDEX_FLUSH_SIZE,
            use_batch_embed,
            if resuming { format!(" from iter {resume_iter_pos}") } else { String::new() },
        );

        // Register (or look up) the cancellation flag for this corpus.
        // Both the Desktop-originated install path and the peer
        // ingest_partition HTTP handler share the same registry, so a
        // cancel fired from Desktop stops whichever task is actually
        // running on this node. The flag is polled at every doc,
        // embed-batch, and tier-2 flush boundary.
        let cancel_flag = self.cancel_registry.register(&recipe.corpus.id);
        // RAII guard that unregisters on any exit path (success, cancel,
        // error, panic-unwind). Crucial so a subsequent ingest for the
        // same corpus gets a fresh flag rather than an already-tripped
        // stale one.
        struct CancelGuard<'a> {
            registry: &'a crate::engine::CancellationRegistry,
            corpus_id: &'a str,
        }
        impl Drop for CancelGuard<'_> {
            fn drop(&mut self) {
                self.registry.unregister(self.corpus_id);
            }
        }
        let _cancel_guard = CancelGuard {
            registry: &self.cancel_registry,
            corpus_id: &recipe.corpus.id,
        };

        for doc_result in doc_iter {
            iter_pos += 1;

            // Skip documents that were already committed in a previous run.
            if iter_pos <= resume_iter_pos {
                continue;
            }

            // Cooperative cancellation — polled once per document (cheap
            // atomic load). Exits between documents so the current flush
            // boundary is respected and `committed_iter_pos` stays
            // consistent with what's durably written.
            if cancel_flag.is_cancelled() {
                tracing::info!(
                    corpus = %recipe.corpus.id,
                    iter_pos,
                    total_chunks,
                    "ingest cancelled by user request — stopping cleanly"
                );
                return Err(Error::Cancelled(recipe.corpus.id.clone()));
            }

            let doc = match doc_result {
                Ok(d) => d,
                Err(e) => {
                    docs_skipped += 1;
                    tracing::warn!(
                        corpus = %recipe.corpus.id,
                        iter_pos,
                        docs_skipped,
                        error = %e,
                        "skipping document due to extraction error"
                    );
                    continue;
                }
            };

            // ── File-boundary detection ────────────────────────────────
            // When `source_file` transitions from A → B, file A's last doc
            // was the previous document (iter_pos - 1). Record that boundary
            // so we can mark A as Complete after the next tier-2 flush.
            if let Some(ref sf) = doc.source_file {
                let file_changed = prev_source_file.as_deref() != Some(sf.as_str());
                if file_changed {
                    if let Some(ref old_sf) = prev_source_file.take() {
                        // iter_pos already incremented at top of loop.
                        file_boundary_iter_pos.insert(old_sf.clone(), iter_pos - 1);
                    }
                    // Transition InProgress state in manifest if present.
                    if let Some(ref mut manifest) = source_manifest {
                        if let Some(record) = manifest.files.iter_mut().find(|r| &r.filename == sf) {
                            if matches!(record.status, SourceFileStatus::Pending) {
                                record.status = SourceFileStatus::InProgress {
                                    started_at: Utc::now(),
                                };
                                manifest.updated_at = Utc::now();
                                let _ = manifest.save(index_path);
                            }
                        }
                    }
                    prev_source_file = Some(sf.clone());
                }
            }

            docs_processed += 1;

            let cleaned_content = normalize_content(&doc.content);
            let text_chunks = chunker.chunk(&cleaned_content);

            for tc in text_chunks {
                let content = if let Some(ref title) = doc.title {
                    if !tc.content.starts_with(title.as_str()) {
                        format!("{title}\n\n{}", tc.content)
                    } else {
                        tc.content
                    }
                } else {
                    tc.content
                };

                let content_hash = blake3_hex(&content);
                // Promote code-intelligence metadata from the extractor's
                // metadata JSON into typed columns. Non-code extractors
                // leave the JSON untouched and `code_meta_from_json`
                // returns all-None → stored as Null columns.
                let code = crate::index::code_meta_from_json(doc.metadata.as_ref());
                pending_texts.push(content.clone());
                pending_chunks.push(InsertChunk {
                    content,
                    title: doc.title.clone(),
                    url: doc.url.clone(),
                    metadata: doc.metadata.as_ref().map(|m| m.to_string()),
                    content_hash: Some(content_hash),
                    source_doc_id: doc.url.clone()
                        .or_else(|| Some(doc.source_id.clone())),
                    source_file: doc.source_file.clone(),
                    code,
                    unit_id,
                });
                // Track chunk count per source file for manifest reporting.
                if let Some(ref sf) = doc.source_file {
                    *chunks_per_file.entry(sf.clone()).or_insert(0) += 1;
                }

                // Tier 1: embed when we have enough pending chunks.
                if pending_chunks.len() >= embed_batch_size {
                    let embed_start = Instant::now();
                    let embed_count = pending_texts.len();
                    let embeddings = if let Some(ref batch_embed) = self.batch_embed {
                        (batch_embed)(&pending_texts).await?
                    } else {
                        let mut embs = Vec::with_capacity(pending_texts.len());
                        for text in &pending_texts {
                            embs.push((self.embed)(text).await?);
                        }
                        embs
                    };
                    let embed_ms = embed_start.elapsed().as_millis();
                    let embed_rate = embed_count as f64 / (embed_ms as f64 / 1000.0).max(0.001);

                    tracing::debug!(
                        chunks = embed_count,
                        embed_ms,
                        rate = format!("{embed_rate:.1}/s"),
                        "Embed batch"
                    );

                    for (chunk, embedding) in pending_chunks.drain(..).zip(embeddings) {
                        index_buffer.push((chunk, embedding));
                    }
                    pending_texts.clear();

                    // Report progress after each embed batch.
                    let elapsed = start.elapsed();
                    let embed_secs = embed_timer.elapsed().as_secs_f32().max(0.001);
                    let chunks_per_sec = embed_count as f32 / embed_secs;
                    eprintln!(
                        "[{}] {} embedded ({} buffered) | {} docs | {chunks_per_sec:.1} chunks/s | {}m{}s",
                        recipe.corpus.id,
                        total_chunks + index_buffer.len() as u64,
                        index_buffer.len(),
                        resume_iter_pos + docs_processed,
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60,
                    );
                    embed_timer = Instant::now();

                    if let Some(ref cb) = progress {
                        cb(IngestProgress::Embedding {
                            chunks_embedded: total_chunks + index_buffer.len() as u64,
                            total: 0,
                            docs_processed: resume_iter_pos + docs_processed,
                            chunks_per_sec,
                        });
                    }
                }

                // Tier 2: flush to index when buffer is large enough.
                if index_buffer.len() >= INDEX_FLUSH_SIZE {
                    let flush_count = index_buffer.len();
                    let insert_start = Instant::now();
                    index.insert_batch(&index_buffer).await?;
                    let insert_ms = insert_start.elapsed().as_millis();
                    let _ = index.update_committed_iter_pos(iter_pos);
                    total_chunks += flush_count as u64;

                    // Tally chunks per file AFTER successful insert, then clear.
                    for (chunk, _) in &index_buffer {
                        if let Some(ref sf) = chunk.source_file {
                            *flushed_chunks_per_file.entry(sf.clone()).or_insert(0) += 1;
                        }
                    }
                    index_buffer.clear();

                    // Mark any files whose last doc has now been committed.
                    mark_complete_files(
                        iter_pos,
                        &file_boundary_iter_pos,
                        &flushed_chunks_per_file,
                        source_manifest.as_mut(),
                        index_path,
                    );
                    mark_complete_shards(
                        iter_pos,
                        &file_boundary_iter_pos,
                        &mut recorded_shards,
                        &index,
                    );

                    if insert_ms > 5000 {
                        tracing::warn!(
                            insert_ms,
                            flush_count,
                            total_chunks,
                            "Index flush stall — likely LanceDB compaction"
                        );
                    }
                    eprintln!(
                        "[{}] Flushed {} chunks to index ({insert_ms}ms) — {total_chunks} total committed",
                        recipe.corpus.id, flush_count,
                    );
                }
            }
        }

        // Flush remaining pending chunks through embedding.
        if !pending_chunks.is_empty() {
            let embeddings = if let Some(ref batch_embed) = self.batch_embed {
                (batch_embed)(&pending_texts).await?
            } else {
                let mut embs = Vec::with_capacity(pending_texts.len());
                for text in &pending_texts {
                    embs.push((self.embed)(text).await?);
                }
                embs
            };
            for (chunk, embedding) in pending_chunks.drain(..).zip(embeddings) {
                index_buffer.push((chunk, embedding));
            }
        }

        // Flush remaining index buffer.
        if !index_buffer.is_empty() {
            let flush_count = index_buffer.len();
            total_chunks += flush_count as u64;
            index.insert_batch(&index_buffer).await?;
            let _ = index.update_committed_iter_pos(iter_pos);

            // Tally AFTER successful insert.
            for (chunk, _) in &index_buffer {
                if let Some(ref sf) = chunk.source_file {
                    *flushed_chunks_per_file.entry(sf.clone()).or_insert(0) += 1;
                }
            }
            if docs_skipped > 0 {
                tracing::warn!(
                    corpus = %recipe.corpus.id,
                    docs_skipped,
                    docs_processed,
                    "ingestion complete with extraction errors — source file may be corrupted or partially downloaded"
                );
            }
            eprintln!(
                "[{}] Final flush — {flush_count} chunks — {total_chunks} total committed from {} docs ({docs_skipped} skipped)",
                recipe.corpus.id,
                resume_iter_pos + docs_processed,
            );

            // The last file in the stream was never "closed" by seeing a
            // subsequent file — record its boundary now.
            if let Some(ref last_sf) = prev_source_file {
                file_boundary_iter_pos.insert(last_sf.clone(), iter_pos);
            }
            mark_complete_files(
                iter_pos,
                &file_boundary_iter_pos,
                &flushed_chunks_per_file,
                source_manifest.as_mut(),
                index_path,
            );
            mark_complete_shards(
                iter_pos,
                &file_boundary_iter_pos,
                &mut recorded_shards,
                &index,
            );
        } else if let Some(ref last_sf) = prev_source_file {
            // No final flush needed (buffer empty) but we still need to close
            // the last file if there was one (can happen on resume when all
            // remaining docs fit in the initial embed pass).
            file_boundary_iter_pos.insert(last_sf.clone(), iter_pos);
            mark_complete_files(
                iter_pos,
                &file_boundary_iter_pos,
                &flushed_chunks_per_file,
                source_manifest.as_mut(),
                index_path,
            );
            mark_complete_shards(
                iter_pos,
                &file_boundary_iter_pos,
                &mut recorded_shards,
                &index,
            );
        }

        // A pipeline that produced zero chunks is almost always a bug
        // (wrong column name, empty parquet, all docs filtered out).
        // On resume: if we skipped everything (all docs were committed), total_chunks
        // is from the existing table and we proceed to build indexes normally.
        if total_chunks == 0 {
            return Err(Error::Extraction(format!(
                "Ingest produced zero chunks for corpus '{}'. \
                 The source may be empty, the extractor may be \
                 misconfigured, or every document may have been filtered.",
                recipe.corpus.id,
            )));
        }

        // Build search indexes (IVF-PQ + FTS).
        // Skip if already completed in a previous run — this is the common case
        // when a process was killed after build_indexes() but before
        // mark_ingestion_complete(). We detect it via the `indexes_built` flag
        // so we don't waste minutes rebuilding what's already there.
        if CorpusIndex::indexes_are_built(index_path) {
            eprintln!(
                "[{}] Search indexes already built — skipping to completion",
                recipe.corpus.id,
            );
        } else {
            let build_vector = recipe.index.vector;
            let build_fts = recipe.index.fts;
            let dims = recipe.index.embedding_dimensions;
            // Estimate IVF-PQ partition count: LanceDB Auto ≈ sqrt(N), capped 2–512.
            let est_partitions = (total_chunks as f64).sqrt().round() as u64;
            let est_partitions = est_partitions.max(2).min(512);
            eprintln!(
                "[{id}] Index build starting — model: {model} ({dims}d), \
                 chunks: {total_chunks}, \
                 vector: {build_vector} (IVF-PQ auto ≈ {est_partitions} partitions), \
                 fts: {build_fts}",
                id = recipe.corpus.id,
                model = self.expected_embedding_model,
            );
            if let Some(ref cb) = progress {
                cb(IngestProgress::Indexing {
                    chunks_indexed: 0,
                    total: total_chunks,
                });
            }
            let sub_phase_cb: Option<Box<dyn Fn(u64, u64) + Send + Sync>> =
                progress.as_ref().map(|cb| -> Box<dyn Fn(u64, u64) + Send + Sync> {
                    Box::new(move |done, total_phases| {
                        cb(IngestProgress::Indexing {
                            chunks_indexed: total_chunks * done / total_phases,
                            total: total_chunks,
                        });
                    })
                });
            index.build_indexes(build_vector, build_fts, sub_phase_cb.as_deref()).await?;
            // Checkpoint: if killed after this point, resume can skip rebuild.
            let _ = index.mark_indexes_built();
        }

        // Optional enrichment phase: field model enrichment.
        if let Some(enrichment_config) = recipe.enrichment.as_ref() {
            if enrichment_config.enabled {
                match self.inference.as_ref() {
                    Some(inference) => {
                        let field_engine =
                            crate::enrichment::field_engine::FieldModelEngine::from_recipe(
                                &recipe,
                                self.embed.clone(),
                                inference.clone(),
                            )?;
                        let id = recipe.corpus.id.clone();
                        let progress_fn = move |p: crate::enrichment::clustering::EnrichmentProgress| {
                            use crate::enrichment::clustering::EnrichmentProgress as EP;
                            match &p {
                                EP::Phase { phase, name, note } => {
                                    if note.is_empty() {
                                        eprintln!("[{id}] Phase {phase}: {name}");
                                    } else {
                                        eprintln!("[{id}] Phase {phase}: {name} ({note})");
                                    }
                                }
                                EP::PhaseSkipped { phase, name } =>
                                    eprintln!("[{id}] Phase {phase}: {name} — skipped (checkpoint)"),
                                EP::Resuming { from_phase } =>
                                    eprintln!("[{id}] Resuming enrichment from {from_phase}"),
                                EP::ClusteringStarted { total_chunks } =>
                                    eprintln!("[{id}] Clustering {total_chunks} chunks..."),
                                EP::ClusteringStep { step, detail } =>
                                    eprintln!("[{id}] ↳ {step}: {detail}"),
                                EP::ClusteringComplete { cluster_count, noise_chunks } =>
                                    eprintln!("[{id}] Clustering complete: {cluster_count} clusters, {noise_chunks} noise"),
                                EP::Phase1Progress { batches_done, batches_total } =>
                                    eprintln!("[{id}] Skeleton extraction: {batches_done}/{batches_total} batches"),
                                EP::Phase2bComplete { labeled_count } =>
                                    eprintln!("[{id}] Cluster labeling complete: {labeled_count} clusters labeled"),
                            }
                        };
                        field_engine.enrich(&index, &progress_fn).await?;
                    }
                    None => {
                        tracing::warn!(
                            "Recipe '{}' requests enrichment but no InferenceFn was provided to CorpusEngine — skipping",
                            recipe.corpus.id,
                        );
                    }
                }
            }
        }

        let duration_secs = start.elapsed().as_secs();
        let info = index.info().await?;

        // Mark the index as fully committed so it survives a restart as "Indexed"
        // rather than being treated as a partial/incomplete ingest.
        if let Err(e) = index.mark_ingestion_complete() {
            tracing::warn!("Failed to mark ingestion complete for '{}': {e}", recipe.corpus.id);
        }

        // For code corpora sourced from a local directory, record the
        // absolute source path so the watcher can find the root without
        // re-parsing the recipe. `reindex_file` and `sovereign code watch`
        // both rely on this.
        if matches!(recipe.extract, crate::recipe::ExtractorConfig::Code { .. }) {
            if let crate::recipe::AcquirerConfig::LocalFile { path } = &recipe.acquire {
                // Expand `~` the same way LocalFileAcquirer does —
                // via $HOME so we don't take a `dirs` dep.
                let resolved = if let Some(rest) = path.strip_prefix("~/") {
                    std::env::var("HOME")
                        .map(|h| PathBuf::from(h).join(rest))
                        .unwrap_or_else(|_| PathBuf::from(path))
                } else {
                    PathBuf::from(path)
                };
                let abs = resolved.canonicalize().unwrap_or(resolved);
                if let Err(e) = index.set_source_path(&abs) {
                    tracing::warn!("Failed to set source_path for '{}': {e}", recipe.corpus.id);
                }
            }
        }

        eprintln!(
            "[{}] Ingestion complete — {total_chunks} chunks in {}m{}s",
            recipe.corpus.id,
            duration_secs / 60,
            duration_secs % 60,
        );

        if let Some(ref cb) = progress {
            cb(IngestProgress::Complete {
                total_chunks,
                duration_secs,
            });
        }

        Ok(IngestResult {
            corpus_id: recipe.corpus.id.clone(),
            chunks_created: total_chunks,
            index_size_bytes: info.index_size_bytes,
            duration_secs,
            docs_skipped,
        })
    }

    // ── Private helpers ────────────────────────────────

    async fn resolve_recipe(&self, corpus: &CorpusSpec) -> Result<Recipe> {
        match corpus {
            CorpusSpec::Builtin(id) => self.registry.fetch_recipe(id).await,
            CorpusSpec::RecipePath(path) => Recipe::from_file(path),
        }
    }

    pub(crate) async fn acquire_source(
        &self,
        recipe: &Recipe,
        download_dir: &Path,
        progress: &Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        match &recipe.acquire {
            AcquirerConfig::BulkDownload { url, resume } => {
                let downloader = BulkDownloader::new(url, *resume);
                downloader
                    .download(download_dir, &recipe.corpus.id, progress)
                    .await
            }
            AcquirerConfig::LocalFile { path } => {
                let acq = LocalFileAcquirer::new(path);
                acq.acquire()
            }
            AcquirerConfig::HuggingFaceDataset { repo, subset, file_indices } => {
                let mut acq = HuggingFaceDatasetAcquirer::new(repo, subset.as_deref());
                if let Some(indices) = file_indices {
                    acq.file_indices = Some(indices.clone());
                }
                acq.download(download_dir, &recipe.corpus.id, progress).await
            }
            AcquirerConfig::WebCrawl { .. } => {
                Err(Error::Recipe("Web crawl acquirer not yet implemented".into()))
            }
            AcquirerConfig::ApiPaginated { .. } => {
                Err(Error::Recipe("API paginated acquirer not yet implemented".into()))
            }
        }
    }

    pub(crate) fn make_extractor(&self, config: &ExtractorConfig) -> Box<dyn Extractor> {
        match config {
            ExtractorConfig::MediawikiXml {
                namespace_filter,
                skip_redirects,
                decompress,
            } => Box::new(extractors::xml::MediawikiExtractor {
                namespace_filter: namespace_filter.clone(),
                skip_redirects: *skip_redirects,
                decompress: decompress.clone(),
            }),
            ExtractorConfig::StackExchangeXml { min_score } => {
                Box::new(extractors::xml::StackExchangeExtractor {
                    min_score: *min_score,
                })
            }
            ExtractorConfig::Jsonl {
                content_field,
                title_field,
                filter,
                decompress,
            } => Box::new(extractors::json::JsonlExtractor {
                content_field: content_field.clone(),
                title_field: title_field.clone(),
                filter: filter.clone(),
                decompress: decompress.clone(),
            }),
            ExtractorConfig::Html {
                content_selector,
                title_selector,
            } => Box::new(extractors::html::HtmlExtractor {
                content_selector: content_selector.clone(),
                title_selector: title_selector.clone(),
                label: String::new(),
            }),
            ExtractorConfig::Csv {
                content_column,
                title_column,
                delimiter,
            } => Box::new(extractors::csv::CsvExtractor {
                content_column: content_column.clone(),
                title_column: title_column.clone(),
                delimiter: delimiter.map(|c| c as u8),
            }),
            ExtractorConfig::Parquet {
                content_column,
                label_column,
                url_column,
                content_transform,
            } => Box::new(extractors::parquet::ParquetExtractor {
                content_column: content_column.clone(),
                label_column: label_column.clone(),
                url_column: url_column.clone(),
                content_transform: content_transform.clone(),
            }),
            ExtractorConfig::Plaintext {
                title_pattern,
                strip_boilerplate,
            } => Box::new(extractors::plaintext::PlaintextExtractor {
                title_pattern: title_pattern.clone(),
                strip_boilerplate: strip_boilerplate.clone(),
            }),
            ExtractorConfig::WikipediaStructured {
                title_column,
                url_column,
                controversy_patterns,
                factual_patterns,
                ..
            } => Box::new(
                extractors::wikipedia_structured::WikipediaStructuredExtractor {
                    title_column: title_column.clone(),
                    url_column: url_column.clone(),
                    controversy_patterns: controversy_patterns.clone(),
                    factual_patterns: factual_patterns.clone(),
                },
            ),
            ExtractorConfig::WikipediaJsonl {
                controversy_patterns,
                factual_patterns,
                article_range,
                shard_indices,
            } => Box::new(
                extractors::wikipedia_jsonl::WikipediaJsonlExtractor {
                    controversy_patterns: controversy_patterns.clone(),
                    factual_patterns: factual_patterns.clone(),
                    article_range: *article_range,
                    shard_indices: shard_indices.clone(),
                },
            ),
            #[cfg(feature = "treesitter")]
            ExtractorConfig::Code {
                context_lines,
                max_lines_per_chunk,
            } => Box::new(extractors::code::CodeExtractor {
                context_lines: *context_lines,
                max_lines_per_chunk: *max_lines_per_chunk,
            }),
            #[cfg(not(feature = "treesitter"))]
            ExtractorConfig::Code { .. } => {
                // The recipe requested the `code` extractor but this
                // corpus-engine build doesn't include tree-sitter. Fail
                // loudly at recipe-load time, not silently at query time.
                panic!(
                    "corpus-engine was built without the `treesitter` feature — \
                     rebuild with `cargo build --features treesitter` to enable \
                     the `code` extractor"
                );
            }
        }
    }

    pub(crate) fn make_chunker(&self, config: &ChunkerConfig) -> Box<dyn Chunker> {
        match config {
            ChunkerConfig::Paragraph {
                max_chars,
                overlap_chars,
            } => Box::new(chunkers::paragraph::ParagraphChunker {
                max_chars: *max_chars,
                overlap_chars: *overlap_chars,
            }),
            ChunkerConfig::Sentence { max_chars } => {
                Box::new(chunkers::sentence::SentenceChunker {
                    max_chars: *max_chars,
                })
            }
            ChunkerConfig::Fixed {
                max_chars,
                overlap_chars,
            } => Box::new(chunkers::fixed::FixedChunker {
                max_chars: *max_chars,
                overlap_chars: *overlap_chars,
            }),
            ChunkerConfig::Semantic { max_chars } => {
                Box::new(chunkers::semantic::SemanticChunker {
                    max_chars: *max_chars,
                })
            }
            ChunkerConfig::Passthrough => Box::new(chunkers::passthrough::PassthroughChunker),
        }
    }

    /// Ingest a named recipe into a caller-specified output directory, with
    /// optional file-index filtering for collaborative partitioned ingestion.
    ///
    /// Unlike the standard `ingest()`, the output path is provided explicitly
    /// so partition workers can write to `<corpus_id>-partition-<node_id>`
    /// rather than `<corpus_id>`. The merge coordinator collects all partition
    /// directories and calls `merge_partitions()` when they're all complete.
    ///
    /// If `file_indices` is `Some`, the recipe's HuggingFace acquirer is
    /// constrained to download only those shard indices (position in the
    /// sorted full manifest). A `None` value falls through to the recipe's
    /// own `file_indices` field, which allows TOML-based partitioning.
    /// Execute an ingest with caller-provided overrides on the recipe's
    /// extractor/acquirer (selecting a subset of shards / an article range)
    /// and an explicit output directory.
    ///
    /// `unit_id` — when the run is processing a leased unit from a
    /// pull-based [`WorkQueueManager`], the caller threads the UnitId
    /// through so every chunk produced is stamped with it in the LanceDB
    /// `unit_id` column. The merge step uses this to dedupe chunks that
    /// two peers wrote for the same unit after a lease expiry. `None`
    /// for legacy static-partition ingests and local Desktop installs.
    pub async fn ingest_with_overrides(
        &self,
        recipe_id: &str,
        file_indices: Option<Vec<usize>>,
        article_range: Option<(u64, u64)>,
        output_path: &Path,
        progress: Option<ProgressCallback>,
        unit_id: Option<u32>,
    ) -> Result<IngestResult> {
        let mut recipe = self
            .resolve_recipe(&crate::types::CorpusSpec::Builtin(recipe_id.to_string()))
            .await?;

        // Route file_indices to the right consumer based on recipe shape.
        //
        // - HF parquet corpora: indices select which parquet shards the
        //   acquirer downloads.
        // - JSONL ZIP corpora (Wikipedia): indices select which JSONL
        //   entries inside the ZIP the extractor streams. This is the
        //   safe partition key for multi-shard JSONL — article-range
        //   partitioning is unsound across peers with non-identical
        //   extractions (see scheduler::knowledge_assignment docs).
        if let Some(indices) = file_indices {
            match (&mut recipe.acquire, &mut recipe.extract) {
                (
                    AcquirerConfig::HuggingFaceDataset { ref mut file_indices, .. },
                    _,
                ) => {
                    *file_indices = Some(indices);
                }
                (
                    _,
                    ExtractorConfig::WikipediaJsonl { ref mut shard_indices, .. },
                ) => {
                    *shard_indices = Some(indices);
                }
                _ => {
                    tracing::warn!(
                        "ingest_with_overrides received file_indices for a recipe \
                         with neither an HF acquirer nor a WikipediaJsonl extractor \
                         — indices will be ignored"
                    );
                }
            }
        }

        // Override article_range on the Wikipedia JSONL extractor when provided.
        if let (Some(range), ExtractorConfig::WikipediaJsonl { ref mut article_range, .. }) =
            (article_range, &mut recipe.extract)
        {
            *article_range = Some(range);
        }

        // Pre-flight: same embed probe as ingest().
        std::fs::create_dir_all(output_path.parent().unwrap_or(output_path))?;

        let probe = (self.embed)("probe").await.map_err(|e| {
            Error::Embed(format!(
                "Embedding function is not available: {e}. \
                 Configure an embedding model before installing corpora."
            ))
        })?;
        if probe.is_empty() {
            return Err(Error::Embed(
                "Embedding function returned an empty vector.".into(),
            ));
        }
        if recipe.index.embedding_dimensions == 0 {
            recipe.index.embedding_dimensions = probe.len();
        }

        self.ingest_inner(&recipe, output_path, &progress, unit_id)
            .await
    }
}

/// Set the `shard_indices` field on a recipe's `WikipediaJsonl` extractor
/// config. No-op for recipes with any other extractor — the caller has
/// already determined that sharding applies to this corpus (see
/// [`CorpusEngine::jsonl_source_shard_count`]).
///
/// Shared helper for both [`CorpusEngine::ingest`] (solo / legacy path)
/// and [`CorpusEngine::ingest_with_overrides`] (peer / coordinator
/// path) so the two entry points stay in sync about how partition
/// assignments reach the extractor.
pub(crate) fn apply_jsonl_shard_override(
    recipe: &mut Recipe,
    indices: Option<Vec<usize>>,
) {
    if let ExtractorConfig::WikipediaJsonl { ref mut shard_indices, .. } = recipe.extract {
        *shard_indices = indices;
    }
}

// ─── Source-file manifest helpers ────────────────────────────────────────────

/// After a tier-2 flush, check whether any files have had all their docs
/// committed to LanceDB and mark them `Complete` in the manifest.
///
/// A file is complete when `committed_iter_pos >= file_boundary_iter_pos`:
/// since `update_committed_iter_pos(iter_pos)` just ran, all documents up to
/// `iter_pos` are durably written.  If a file's last document was at or before
/// that position, every chunk from that file is now in the index.
fn mark_complete_files(
    committed_iter_pos: u64,
    file_boundary_iter_pos: &HashMap<String, u64>,
    flushed_chunks_per_file: &HashMap<String, u64>,
    manifest: Option<&mut SourceFileManifest>,
    index_path: &Path,
) {
    let Some(manifest) = manifest else { return };
    let mut changed = false;
    for (filename, &boundary) in file_boundary_iter_pos {
        if committed_iter_pos < boundary {
            continue;
        }
        if let Some(record) = manifest.files.iter_mut().find(|r| &r.filename == filename) {
            if !matches!(record.status, SourceFileStatus::Complete { .. }) {
                let chunks_indexed = *flushed_chunks_per_file.get(filename).unwrap_or(&0);
                record.status = SourceFileStatus::Complete {
                    chunks_indexed,
                    completed_at: Utc::now(),
                };
                tracing::info!(
                    filename,
                    chunks_indexed,
                    "Source file fully committed to index"
                );
                changed = true;
            }
        }
    }
    if changed {
        manifest.updated_at = Utc::now();
        if let Err(e) = manifest.save(index_path) {
            tracing::warn!("Failed to persist source manifest: {e}");
        }
    }
}

/// JSONL counterpart of `mark_complete_files`.
///
/// When the Wikipedia JSONL extractor runs in sharded mode it stamps every
/// document with `source_file = Some("shard:<n>")`. This helper parses those
/// tags out of `file_boundary_iter_pos`, and for any shard whose boundary
/// has now been durably committed to LanceDB (`committed_iter_pos >=
/// boundary`), writes the shard index into `_corpus_meta.json`'s
/// `processed_shards` array.
///
/// The coordinator reads `processed_shards` from every partition
/// subdirectory when planning the next collaborative ingest so it knows
/// which shards still need work — the sharded analogue of
/// `remaining_source_files` for HF parquet corpora.
///
/// `recorded` is the in-run memoization of which shards we've already
/// persisted, so a flush that passes the boundary of an already-recorded
/// shard doesn't rewrite the meta file.
fn mark_complete_shards(
    committed_iter_pos: u64,
    file_boundary_iter_pos: &HashMap<String, u64>,
    recorded: &mut std::collections::HashSet<usize>,
    index: &crate::index::CorpusIndex,
) {
    for (tag, &boundary) in file_boundary_iter_pos {
        let Some(shard_index) = crate::extractors::wikipedia_jsonl::parse_shard_source_file(tag)
        else {
            continue;
        };
        if recorded.contains(&shard_index) || committed_iter_pos < boundary {
            continue;
        }
        match index.record_processed_shard(shard_index) {
            Ok(()) => {
                tracing::info!(
                    shard_index,
                    committed_iter_pos,
                    boundary,
                    "JSONL shard fully committed to index"
                );
                recorded.insert(shard_index);
            }
            Err(e) => {
                tracing::warn!(
                    shard_index,
                    error = %e,
                    "failed to persist processed_shards entry — will retry next flush"
                );
            }
        }
    }
}
