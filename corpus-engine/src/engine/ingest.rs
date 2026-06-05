//! Ingestion pipeline — acquire, extract, chunk, embed, index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;

use crate::error::{Error, Result};
use crate::extractors::ExtractedDoc;
use crate::filters::build_filter_pipeline;
use crate::index::{CorpusIndex, InsertChunk};
use crate::progress::{IngestProgress, ProgressCallback, SourceFileManifest, SourceFileStatus};
use crate::recipe::{AcquirerConfig, ExtractorConfig, Recipe};
use crate::types::{CorpusSpec, IngestResult};

use super::ingest_helpers::{
    apply_jsonl_shard_override, mark_complete_files, mark_complete_shards,
};
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
        // ── Pre-flight: refuse on-demand recipes resolved by id. ───
        //
        // On-demand recipes (e.g. `gutenberg-work`) are templates —
        // their TOML carries placeholder `[corpus] id` / acquire URL.
        // The catalog ingest service is responsible for cloning the
        // recipe, patching it, and handing it through
        // `CorpusSpec::Inline`. A direct `CorpusEngine::ingest(
        // CorpusSpec::Builtin("gutenberg-work"))` would otherwise
        // happily blast the placeholder URL into a real corpus dir.
        // We do this BEFORE the embed/disk pre-flights so the guard
        // fires consistently — even on engines without an embedder
        // configured.
        let mut recipe = self.resolve_recipe(corpus).await?;
        if recipe.corpus.on_demand && !matches!(corpus, CorpusSpec::Inline(_)) {
            return Err(Error::InvalidInput(format!(
                "recipe `{}` is marked on_demand=true and must be \
                 ingested via CorpusSpec::Inline with corpus.id, \
                 acquire URL, and parent_corpus_id all overridden. \
                 Use CatalogIngestService instead of calling \
                 ingest() directly.",
                recipe.corpus.id,
            )));
        }

        // Ensure parent directory exists so `_downloads` and the
        // per-corpus index dir can be created underneath.
        std::fs::create_dir_all(&self.index_dir)?;

        // ── Pre-flight: require an explicit embed model name ──────
        //
        // `_corpus_meta.json.embedding_model` is what downstream
        // consumers (shard consistency checks, dim-filter at
        // retrieval time, follow-up reindex decisions) read to
        // decide compatibility. Getting it wrong silently is worse
        // than not ingesting at all — a retrieval layer that trusts
        // a bogus label will serve the wrong corpus or skip a
        // compatible one.
        //
        // The engine has no way to introspect an `EmbedFn` — it's an
        // opaque closure. The caller (which built the EmbedFn) knows
        // the model it wired in; it must hand that name to us via
        // `.with_embedding_model(stem)` before calling `ingest`.
        if self.expected_embedding_model.is_empty() {
            return Err(Error::Embed(
                "embedding model name not configured. Call \
                 `CorpusEngine::with_embedding_model(stem)` before \
                 `ingest()`. The stem should match the filename of \
                 the embedding GGUF (e.g. `qwen-embedding-0.6b` for \
                 `qwen-embedding-0.6b.gguf`) so the label written to \
                 `_corpus_meta.json` matches the model that actually \
                 produced the vectors."
                    .to_string(),
            ));
        }

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

        // ── Prebuilt-snapshot restore: short-circuit to download+extract ──
        //
        // Recipes declaring `[prebuilt]` ship a pre-built .tar.zst snapshot
        // of the index (+ optional atlas). The restorer downloads the
        // archive, verifies its sha256, and extracts it under
        // `~/.sovereign/` — bypassing the acquire/extract/chunk/embed
        // pipeline. Compatibility is decided on the SPACE, not the label:
        // dimensions are a hard floor, an exact model-name match is
        // trusted, and a name-only mismatch is VERIFIED by re-embedding
        // sample chunks (the probe in `try_restore_prebuilt`). `Ok(None)`
        // means "incompatible — rebuild with the local model" (Option B);
        // a hard error (download/sha/extract) aborts.
        if let Some(prebuilt) = recipe.prebuilt.as_ref() {
            let started = Instant::now();
            match self
                .try_restore_prebuilt(&recipe, prebuilt, &progress)
                .await
            {
                Ok(Some(restored)) => {
                    let duration_secs = started.elapsed().as_secs();
                    tracing::info!(
                        corpus_id = %recipe.corpus.id,
                        chunks = restored.chunks_created,
                        duration_secs,
                        "ingest: prebuilt snapshot restored — skipped full pipeline"
                    );
                    return Ok(IngestResult {
                        duration_secs,
                        ..restored
                    });
                }
                Ok(None) => {
                    tracing::warn!(
                        corpus_id = %recipe.corpus.id,
                        local_model = %self.expected_embedding_model,
                        "ingest: prebuilt snapshot not usable with the local embedding model — running full ingest"
                    );
                    // fall through to the full acquire/extract/chunk/embed pipeline
                }
                Err(e) => return Err(e),
            }
        }

        // ── Watcher-driven recipes: short-circuit to empty index ────
        //
        // Recipes declaring `[update] ingest_driver = "watcher"` are
        // populated by a daemon-side watcher (see
        // `corpus_engine::update::newsworthy_watcher`) rather than the
        // acquire/extract/chunk pipeline. Install creates a valid but
        // empty CorpusIndex so peer install paths, mesh auto-resume,
        // and the desktop setup wizard all converge on the same on-
        // disk shape; the watcher writes the first chunks on its first
        // tick. Critically, this means we DO NOT invoke the recipe's
        // `[acquire]` block here — its URL template carries watcher-
        // time placeholders (e.g. `{date_yyyy_month_dd}` for
        // wikipedia-newsworthy) that aren't valid recipe parameters
        // and would otherwise trip the template validator.
        let watcher_driven = recipe
            .update
            .as_ref()
            .is_some_and(|u| u.has_external_driver());
        if watcher_driven {
            let started = Instant::now();
            let corpus_id = recipe.corpus.id.clone();
            let canonical = self.index_dir.join(&corpus_id);
            // Watcher-driven recipes have no chunks at install time — the
            // watcher writes them on its first tick. We still mark
            // ingestion complete so `installed_indexes()` returns the
            // corpus (it filters partials by `ingestion_in_progress`),
            // the desktop chip flips off "Add", and the mesh
            // StorageSnapshot loop advertises this node as a holder of
            // the corpus. Without this flip the index is invisible to
            // every downstream surface despite being semantically
            // installed; chunks=0 is the steady state, not a partial.
            let idx = if !canonical.exists() {
                std::fs::create_dir_all(canonical.parent().unwrap_or(&self.index_dir))?;
                let idx = CorpusIndex::create_with_sharing(
                    &canonical,
                    &recipe.corpus.id,
                    &recipe.corpus.name,
                    &recipe.index.embedding_model,
                    recipe.index.embedding_dimensions,
                    recipe.corpus.mesh_sharing,
                    recipe.corpus.query_sharing,
                    &recipe.corpus.license,
                )
                .await?;
                tracing::info!(
                    corpus_id = %corpus_id,
                    driver = recipe.update.as_ref().and_then(|u| u.ingest_driver.as_deref()).unwrap_or(""),
                    path = %canonical.display(),
                    "ingest: created empty index for watcher-driven recipe"
                );
                idx
            } else {
                tracing::info!(
                    corpus_id = %corpus_id,
                    path = %canonical.display(),
                    "ingest: watcher-driven recipe — canonical index already present, no-op"
                );
                CorpusIndex::open(&canonical).await?
            };
            if !CorpusIndex::is_ingestion_complete(&canonical) {
                idx.mark_ingestion_complete()?;
                tracing::info!(
                    corpus_id = %corpus_id,
                    "ingest: marked watcher-driven index as ingestion-complete \
                     (steady state for this driver kind)"
                );
            }
            return Ok(IngestResult {
                corpus_id,
                chunks_created: 0,
                index_size_bytes: 0,
                duration_secs: started.elapsed().as_secs(),
                docs_skipped: 0,
            });
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
        //
        // Applied for BOTH new-flow ingests AND legacy-canonical
        // resumes. Without this on the legacy path, a re-ingest
        // against a previously-promoted canonical falls through to
        // `WikipediaJsonl`'s flat reader, which dumb-greps the ZIP
        // for `*.jsonl` and pulls in macOS resource forks
        // (`__MACOSX/._*.jsonl`) as if they were data shards. The
        // sharded path uses `canonical_jsonl_shard_entries`, which
        // filters those out — same logic peer collaboration uses.
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

        // ── Run the actual pipeline with cleanup-on-failure ───────
        // Solo `ingest()` never runs under a work-queue lease — unit_id
        // stamping only happens for `ingest_with_overrides` callers that
        // explicitly thread a UnitId in.
        let result = self
            .ingest_inner(&recipe, &index_path, &progress, None, false)
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
                    // Stamp the canonical fingerprint after the
                    // rename so peers can compare. The `Result`
                    // path inside `finalise_solo_ingest` is sync
                    // (filesystem rename); fingerprint compute
                    // requires an async LanceDB scan, so we do it
                    // here on the canonical path. Failures are
                    // logged but non-fatal — the canonical is
                    // valid; mesh sync just degrades to chunk-count
                    // comparisons until a future stamp succeeds.
                    let canonical_path = self.index_dir.join(&corpus_id);
                    match crate::index::CorpusIndex::open(&canonical_path).await {
                        Ok(canonical) => {
                            if let Err(e) = canonical.compute_and_stamp_fingerprint().await {
                                tracing::warn!(
                                    corpus_id,
                                    error = %e,
                                    "ingest: fingerprint stamping failed after solo \
                                     promotion; mesh sync degrades to chunk-count"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                corpus_id,
                                error = %e,
                                "ingest: cannot reopen canonical post-promotion to \
                                 stamp fingerprint"
                            );
                        }
                    }
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
        peer_pulled: bool,
    ) -> Result<IngestResult> {
        // Default to no skipset; the expansion path uses
        // `ingest_inner_with_skipset` instead.
        self.ingest_inner_with_skipset(recipe, index_path, progress, unit_id, None, peer_pulled)
            .await
    }

    /// Variant of [`Self::ingest_inner`] that takes an optional set of
    /// already-indexed `source_doc_id`s to skip. Used by
    /// `expand_corpus` to add only newly-accepted documents to an
    /// existing index without re-embedding the originals.
    ///
    /// `peer_pulled` — `true` when this run was initiated by a remote
    /// coordinator's handoff (the peer-pull / static `ingest_partition`
    /// paths). The flag is stamped onto the partition's
    /// `_corpus_meta.json` as `provenance: PeerPulled` right after
    /// the index handle is created, so a daemon-restart auto-resume
    /// can skip work this node didn't initiate (the coordinator
    /// re-issues the handoff if it still wants the work).
    pub(crate) async fn ingest_inner_with_skipset(
        &self,
        recipe: &Recipe,
        index_path: &Path,
        progress: &Option<ProgressCallback>,
        unit_id: Option<u32>,
        already_indexed: Option<std::sync::Arc<std::collections::HashSet<String>>>,
        peer_pulled: bool,
    ) -> Result<IngestResult> {
        // Per-partition exclusion. See `CorpusEngine::partition_locks`
        // for the full motivation; in short, two concurrent ingests into
        // the same `<corpus>-partition-<node>/` halve effective
        // throughput instead of doubling it (single-threaded embed slot
        // + LanceDB writer mutex), so we reject the second caller
        // outright. The guard is held until this function returns.
        let _partition_guard = self.try_acquire_partition_lock(index_path).ok_or_else(|| {
            Error::Recipe(format!(
                "another ingest is already writing to '{}'. \
                     Refusing to start a second concurrent run on the \
                     same partition — the existing run will continue \
                     and this unit should be re-leased to a different peer.",
                index_path.display()
            ))
        })?;

        let start = Instant::now();

        // Step 1: Acquire source data.
        let download_dir = self.index_dir.join("_downloads");
        let source_path = self.acquire_source(recipe, &download_dir, progress).await?;

        // Step 2: Extract documents.
        let extractor = self.make_extractor(&recipe.extract, &recipe.corpus.id);
        let doc_iter = extractor.extract(&source_path)?;

        // Step 2.5: Apply document-level filters (recipe scope).
        //
        // The pipeline wraps the extractor's lazy iterator so rejected
        // documents never reach chunking/embedding. For Wikipedia Core
        // this is `pageview_rank ≤ 100k OR title ∈ vital_articles`.
        // An empty `[[filter]]` block (the default) yields an inactive
        // pipeline that passes everything through.
        //
        // Errors flow through unchanged so the existing
        // skip-and-keep-going extraction-error logic in the chunk loop
        // is preserved.
        let filter_pipeline = build_filter_pipeline(
            &recipe.filters,
            recipe.filter_mode.mode,
            Some(self.recipes_dir.as_path()),
        )?;
        if filter_pipeline.is_active() {
            tracing::info!(
                corpus = %recipe.corpus.id,
                filter_count = recipe.filters.len(),
                filter_mode = ?recipe.filter_mode.mode,
                signature = %filter_pipeline.signature(),
                "Document filter active"
            );
            for desc in filter_pipeline.descriptions() {
                tracing::info!(corpus = %recipe.corpus.id, "  filter: {desc}");
            }
        }
        let scope_meta = if filter_pipeline.is_active() {
            Some(crate::index::ScopeMeta {
                filter_descriptions: filter_pipeline.descriptions(),
                filter_signature: filter_pipeline.signature().to_string(),
                expandable: true,
            })
        } else {
            None
        };
        // Cache the filter's expected accept-count *before* the
        // pipeline gets moved into the iterator-wrapping closure. This
        // is what the desktop UI uses as the percent denominator for
        // filtered ingests — `docs_processed / expected_filter_docs`
        // is a much more honest signal than shard-scan progress when
        // the filter rejects ~99% of the source ZIP.
        let expected_filter_docs: Option<u64> = filter_pipeline.expected_count().map(|n| n as u64);
        let doc_iter: Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send> =
            if filter_pipeline.is_active() {
                let filter = std::sync::Arc::new(filter_pipeline);
                let filter_clone = filter.clone();
                Box::new(doc_iter.filter(move |r| match r {
                    Ok(doc) => filter_clone.accept(doc),
                    Err(_) => true, // pass extraction errors through; chunk loop handles them
                }))
            } else {
                doc_iter
            };

        // Step 3: Chunk, embed, and index.
        let chunker = self.make_chunker(&recipe.chunk);

        // Unit-scoped runs (pull-queue workers) must NOT resume from the
        // partition-wide `committed_iter_pos`, NOT build search indexes,
        // and NOT finalize the partition — the source iterator is already
        // bounded by the caller's file_indices/article_range, and
        // finalization is the merge leader's job after every unit
        // completes. See todo note `f152dfe7` for the 225-failure cascade
        // that motivated this split.
        let unit_scoped = unit_id.is_some();

        // Capture the assigned shard set this run is iterating, if any.
        // For Wikipedia JSONL ingests, `apply_jsonl_shard_override` (in the
        // outer `ingest()`) sets this to `(0..shard_count) - processed_shards`
        // before we get here. This is the coordinate space `iter_pos` will
        // increment within, so we save it alongside `committed_iter_pos`
        // and compare on resume to detect coordinate-space drift.
        let assigned_shard_set: Option<Vec<usize>> = match &recipe.extract {
            ExtractorConfig::WikipediaJsonl { shard_indices, .. } => shard_indices.clone(),
            _ => None,
        };

        // Open or resume a partial index (supports resuming after process kill).
        let (index, resume_iter_pos) = if unit_scoped {
            let index = CorpusIndex::create_or_open_for_unit(
                index_path,
                &recipe.corpus.id,
                &recipe.corpus.name,
                &self.expected_embedding_model,
                recipe.index.embedding_dimensions,
                recipe.corpus.mesh_sharing,
                recipe.corpus.query_sharing,
                &recipe.corpus.license,
            )
            .await?;
            (index, 0u64)
        } else {
            CorpusIndex::create_or_resume_with_sharing(
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
            .await?
        };

        // Stamp the canonical shard count for sharded extractors.
        // Without this field, `corpus diag` can only infer total
        // shards as `max(processed_shards) + 1`, which silently
        // undercounts when the trailing shards never started — the
        // exact case that masked shard 37 missing in the wild
        // wikipedia ingest. Stamping at extract start (when we know
        // the canonical count from the source archive) makes the
        // diag answer authoritative.
        //
        // Currently scoped to WikipediaJsonl since that's the only
        // multi-shard extractor today; trivial to extend when more
        // arrive.
        if let ExtractorConfig::WikipediaJsonl { .. } = recipe.extract {
            match crate::engine::canonical_jsonl_shard_entries(&source_path) {
                Ok(canonical) => {
                    if let Err(e) = index.set_total_shards(canonical.len()) {
                        tracing::warn!(
                            corpus = %recipe.corpus.id,
                            error = %e,
                            "ingest: failed to stamp total_shards — \
                             diag will fall back to the max+1 heuristic"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %recipe.corpus.id,
                        error = %e,
                        "ingest: could not enumerate canonical shards — \
                         total_shards left unstamped"
                    );
                }
            }
        }

        // Stamp the recipe's `kind` and `parent_corpus_id` onto the
        // freshly-created meta file. Both fields are absent on legacy
        // indexes (and on indexes whose recipe didn't set them);
        // `info()` falls back to source_path-based kind derivation
        // and a None parent for those, so this is purely additive.
        // Logged-but-non-fatal: `kind` only affects search-time
        // partitioning, never correctness of the chunks themselves.
        if let Err(e) = index.set_kind_and_parent(
            Some(recipe.corpus.kind),
            recipe.corpus.parent_corpus_id.as_deref(),
        ) {
            tracing::warn!(
                corpus = %recipe.corpus.id,
                path = %index_path.display(),
                error = %e,
                "ingest_inner: failed to stamp kind / parent_corpus_id — \
                 search-time partitioning will fall back to defaults"
            );
        }

        // Stamp the `[display]` block from the recipe so the Atlas
        // View rail can group corpora that share a category (e.g.
        // both `conversations-anthropic` and `conversation-history`
        // surface under one "Conversations" header) and so the
        // synthesis prompt's chunk-section renamer (see
        // `format_scored_chunks_with_kinds`) reads category-aware
        // labels off `IndexInfo.display` without re-resolving the
        // recipe. Pure UI metadata — non-fatal on write failure.
        if recipe.display.is_some() {
            if let Err(e) = index.set_display(recipe.display.clone()) {
                tracing::warn!(
                    corpus = %recipe.corpus.id,
                    path = %index_path.display(),
                    error = %e,
                    "ingest_inner: failed to stamp [display] block — \
                     Atlas View will fall back to ungrouped layout"
                );
            }
        }

        // Stamp the `[retrieval] dedup_by_source` flag so the runtime can
        // apply per-article source dedup to this corpus without an env var
        // or re-resolving the recipe. Stamped unconditionally (like the
        // mutable-merge policy below) so every fresh index carries an
        // explicit value. Non-fatal — retrieval falls back to no dedup.
        if let Err(e) = index.set_dedup_by_source(recipe.retrieval.dedup_by_source) {
            tracing::warn!(
                corpus = %recipe.corpus.id,
                path = %index_path.display(),
                error = %e,
                "ingest_inner: failed to stamp [retrieval] dedup_by_source — \
                 retrieval falls back to no dedup for this corpus"
            );
        }

        // Stamp the mutable-merge policy from the recipe so future
        // merges of this index against peer partitions take the
        // chosen reconciliation rule. None preserves classic
        // content-hash dedupe; only `alignment`-style recipes opt in.
        if let Err(e) = index.set_mutable_merge(recipe.corpus.mutable_merge) {
            tracing::warn!(
                corpus = %recipe.corpus.id,
                path = %index_path.display(),
                error = %e,
                "ingest_inner: failed to stamp mutable_merge policy — \
                 next merge will fall back to content-hash dedupe"
            );
        }

        // Stamp provenance immediately after the index handle (and
        // therefore the meta file) exists. We do this before any
        // long-running work so a daemon kill mid-ingest leaves the
        // partition correctly tagged as `PeerPulled`, which auto-resume
        // uses to skip re-firing peer-pulled work the local user
        // didn't initiate. SelfInitiated is the default; only stamp
        // explicitly when peer_pulled is true.
        if peer_pulled {
            if let Err(e) =
                crate::index::set_provenance(index_path, crate::index::CorpusProvenance::PeerPulled)
            {
                tracing::warn!(
                    corpus = %recipe.corpus.id,
                    path = %index_path.display(),
                    error = %e,
                    "ingest_inner: failed to stamp PeerPulled provenance — \
                     auto-resume may re-fire this partition after a daemon restart"
                );
            }
        }

        // Persist the active filter scope to `_corpus_meta.json` so the
        // UI can offer "Expand to full <corpus>" when this scope is
        // narrower than the source. Idempotent — overwriting is fine
        // when a resume run lands on the same scope.
        //
        // Skipped for unit-scoped (pull-queue) runs because the
        // partition is part of a larger ingest the merge leader will
        // finalize; scope only makes sense at the partition-wide level.
        if !unit_scoped {
            if let Some(ref scope) = scope_meta {
                if let Err(e) = index.write_scope(Some(scope.clone())) {
                    tracing::warn!(corpus = %recipe.corpus.id, "Failed to persist scope meta: {e}");
                }
            } else {
                // No filter active — clear any prior scope (e.g. an
                // expansion that just removed the filter). `expandable`
                // flips to false implicitly via `read_scope() == None`.
                if let Err(e) = index.write_scope(None) {
                    tracing::warn!(corpus = %recipe.corpus.id, "Failed to clear scope meta: {e}");
                }
            }
        }

        // ── Shard-set drift detection ────────────────────────────────────
        //
        // `committed_iter_pos` is meaningful only within the iteration
        // produced by the shard set the previous run was iterating. If
        // `processed_shards` mutated between runs (a shard's boundary
        // crossed during a prior flush), the assigned set computed at
        // the top of `ingest()` shrinks — and `committed_iter_pos`
        // becomes a stale coordinate. Same numeric value, different
        // source position. This is the "Wikipedia ingestion declared
        // complete after killing the daemon" bug: the resume cursor
        // landed past the end of the smaller iteration, the for-loop
        // exhausted with zero docs processed, and the pipeline fell
        // through to indexing as if it had finished.
        //
        // Detection: compare the saved `committed_shard_set` against
        // the current run's assigned set. If they differ, treat the
        // saved iter_pos as untrustworthy: reset to 0 and merge the
        // existing chunks' source_doc_ids into the `already_indexed`
        // skipset so the fresh iteration only embeds documents that
        // aren't already in the table.
        //
        // Skipped for unit-scoped runs because their iter_pos is
        // bounded by the work-queue lease (file_indices/article_range
        // override), not by `committed_iter_pos` — the merge leader
        // owns finalisation, not us.
        let (resume_iter_pos, already_indexed) = if !unit_scoped {
            let saved_shard_set = index.committed_shard_set().ok().flatten();
            let drift = match (&saved_shard_set, &assigned_shard_set) {
                (Some(saved), Some(current)) => saved != current,
                // Legacy index (no saved shard set) with a sharded run
                // AND a non-zero saved iter_pos: this is exactly the
                // pre-fix state that produced the wikipedia data loss.
                // The iter_pos was computed against an unknown shard
                // set; we can't verify it against the current set, so
                // treat it as untrustworthy. The skipset path will
                // re-yield every doc, and the filter + skipset combine
                // to embed only what's missing. Pure overhead when the
                // index actually IS complete; correct recovery when
                // it isn't.
                (None, Some(_)) if resume_iter_pos > 0 => true,
                // Fresh install on a sharded corpus (iter_pos = 0):
                // nothing to drift against, no-op.
                (None, Some(_)) => false,
                // No assigned shard set on the current run (single-
                // shard / non-Wikipedia). Iter_pos space is implicit
                // and stable across runs.
                (_, None) => false,
            };
            if drift {
                tracing::warn!(
                    corpus = %recipe.corpus.id,
                    saved = ?saved_shard_set,
                    current = ?assigned_shard_set,
                    saved_iter_pos = resume_iter_pos,
                    "shard-set drift detected on resume — \
                     falling back to source_doc_id skipset"
                );
                eprintln!(
                    "[{}] Shard-set drift on resume — saved set differs from current. \
                     Loading existing source_doc_ids as skipset; iter_pos reset to 0.",
                    recipe.corpus.id,
                );
                let mut skip = match index.list_indexed_source_doc_ids().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(
                            corpus = %recipe.corpus.id,
                            error = %e,
                            "shard-set drift: list_indexed_source_doc_ids failed; \
                             cannot safely resume — proceeding with iter_pos as-is"
                        );
                        return Err(Error::Database(format!(
                            "shard-set drift recovery failed: {e}"
                        )));
                    }
                };
                if let Some(ref existing) = already_indexed {
                    for id in existing.iter() {
                        skip.insert(id.clone());
                    }
                }
                tracing::info!(
                    corpus = %recipe.corpus.id,
                    skipset_size = skip.len(),
                    "shard-set drift: skipset built from existing chunks"
                );
                // Force a fresh index build over the union of existing
                // and newly-embedded chunks. Without this, the
                // `indexes_are_built` short-circuit would leave the
                // recovery chunks unsearchable.
                if let Err(e) = index.reset_for_drift_recovery() {
                    tracing::warn!(
                        corpus = %recipe.corpus.id,
                        error = %e,
                        "shard-set drift: failed to reset index-built flags — \
                         IVF-PQ + FTS may not include recovered chunks"
                    );
                }
                (0u64, Some(std::sync::Arc::new(skip)))
            } else {
                (resume_iter_pos, already_indexed)
            }
        } else {
            (resume_iter_pos, already_indexed)
        };

        // Initialise counters. On resume these start from where we left off.
        let mut total_chunks = index.chunk_count().await.unwrap_or(0);
        let mut docs_processed = 0u64; // successful docs in THIS run
        let mut docs_skipped = 0u64; // docs skipped due to extraction errors this run
        let mut iter_pos = 0u64; // absolute position in the source iterator

        // Circuit-breaker: a run-ending guard against an extractor whose
        // iterator yields errors without end (the classic case is a
        // misconfigured `local_file` pointing at a directory, whose fd
        // returns EISDIR on every read). Reset to 0 on every successful
        // document, so a corpus with sparse bad records keeps going; only
        // an UNBROKEN streak with zero good docs trips it. Turns a silent
        // multi-million phantom-skip runaway into a fast, legible abort.
        const MAX_CONSECUTIVE_EXTRACTION_ERRORS: u64 = 1000;
        let mut consecutive_extraction_errors = 0u64;

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
        let mut source_manifest: Option<SourceFileManifest> =
            SourceFileManifest::load(index_path).unwrap_or(None);
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

        // ── Embed-side dedup gate ───────────────────────────────
        //
        // The resume-cursor-rewind bug surfaced in the wild left
        // up to ~65% duplicate-content rows in some indexes.
        // Embedding is the dominant cost (~30-40 chunks/sec on
        // qwen-embedding-0.6b on M-class), so re-embedding content
        // we already wrote is the most expensive way to make this
        // mistake. The fix: load every existing chunk's
        // `content_hash` into a HashSet at startup; before pushing
        // a new chunk into the embed queue, skip if its hash is
        // already in the set.
        //
        // Why a HashSet rather than per-chunk DB lookups: a single
        // table scan is cheap (~seconds at 4M rows), where 4M
        // individual `only_if(content_hash = '…')` queries would
        // dominate runtime. The memory cost is bounded:
        // `1.5M unique hashes × ~150 bytes/entry ≈ 225 MB`. For a
        // first-time ingest the set starts empty and grows lazily
        // as chunks are emitted in this run — caps the same-content
        // dupes that show up within a single shard.
        //
        // We populate the set ONCE at startup and add to it inline
        // as new chunks are emitted (before they're embedded). This
        // means within-batch duplicates are also caught — a section
        // that appears identically in two source docs gets embedded
        // exactly once.
        let mut seen_hashes: std::collections::HashSet<String> =
            match index.list_indexed_content_hashes().await {
                Ok(set) => {
                    if !set.is_empty() {
                        eprintln!(
                            "[{}] Embed-side dedup gate: {} existing content_hashes loaded — \
                         resume will skip already-embedded chunks",
                            recipe.corpus.id,
                            set.len()
                        );
                    }
                    set
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %recipe.corpus.id,
                        error = %e,
                        "ingest: failed to seed embed-side dedup gate; \
                         proceeding with empty seen-set (resume may re-embed already-written rows)"
                    );
                    std::collections::HashSet::new()
                }
            };
        let mut dedup_skipped: u64 = 0;

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
            if resuming {
                format!(" from iter {resume_iter_pos}")
            } else {
                String::new()
            },
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
                Ok(d) => {
                    consecutive_extraction_errors = 0;
                    d
                }
                Err(e) => {
                    docs_skipped += 1;
                    consecutive_extraction_errors += 1;
                    tracing::warn!(
                        corpus = %recipe.corpus.id,
                        iter_pos,
                        docs_skipped,
                        consecutive_extraction_errors,
                        error = %e,
                        "skipping document due to extraction error"
                    );
                    if consecutive_extraction_errors >= MAX_CONSECUTIVE_EXTRACTION_ERRORS {
                        return Err(Error::Extraction(format!(
                            "aborting ingest of '{}' after {consecutive_extraction_errors} \
                             consecutive extraction errors with no successful document — the \
                             source is likely malformed or the wrong type (e.g. a directory \
                             where a single file was expected, or a binary file fed to a text \
                             extractor). Last error: {e}",
                            recipe.corpus.id
                        )));
                    }
                    continue;
                }
            };

            // ── File-boundary detection (runs BEFORE resume-skip) ────────
            // When `source_file` transitions from A → B, file A's last doc
            // was the previous document (iter_pos - 1). Record that boundary
            // so we can mark A as Complete after the next tier-2 flush.
            //
            // CRITICAL ordering: this MUST run before the resume-skip
            // continue so shards traversed only during fast-forward
            // record their boundaries. Without this, shards we already
            // processed in an earlier run (whose docs are now indexed
            // and skipped via iter_pos) would never get marked into
            // `processed_shards`, so the next restart's
            // `apply_jsonl_shard_override` keeps re-assigning them.
            // The assigned-shard set then stays inflated and the
            // iter_pos coordinate space drifts — exactly the bug that
            // dropped 31k Vital Articles from the wikipedia ingest.
            //
            // The skipset check is also moved below this block so a
            // skipset hit (`already_indexed`) doesn't suppress
            // boundary recording — we still know which file the doc
            // came from regardless of whether we re-embed it.
            if let Some(ref sf) = doc.source_file {
                let file_changed = prev_source_file.as_deref() != Some(sf.as_str());
                if file_changed {
                    if let Some(ref old_sf) = prev_source_file.take() {
                        // iter_pos already incremented at top of loop.
                        file_boundary_iter_pos.insert(old_sf.clone(), iter_pos - 1);
                    }
                    // Transition InProgress state in manifest if present.
                    if let Some(ref mut manifest) = source_manifest {
                        if let Some(record) = manifest.files.iter_mut().find(|r| &r.filename == sf)
                        {
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

            // Skip documents that were already committed in a previous run.
            // Boundary detection above keeps `file_boundary_iter_pos`
            // populated even for these fast-forwarded docs, so
            // `mark_complete_shards` can promote shards that finished
            // entirely during a previous run.
            if iter_pos <= resume_iter_pos {
                continue;
            }

            // ── Expansion / drift skipset ────────────────────────────────
            // When `expand_corpus` runs the pipeline against an existing
            // index with a relaxed filter, it threads in the set of
            // already-indexed `source_doc_id`s so this run only embeds
            // newly-accepted documents. The shard-set-drift recovery
            // path also populates this set with the index's existing
            // source_doc_ids when iter_pos has been reset to 0.
            if let Some(skip) = already_indexed.as_ref() {
                let key = doc
                    .url
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(doc.source_id.as_str());
                if !key.is_empty() && skip.contains(key) {
                    docs_skipped += 1;
                    continue;
                }
            }

            docs_processed += 1;

            let cleaned_content = normalize_content(&doc.content);
            let text_chunks = chunker.chunk(&cleaned_content);

            // `doc.embed_text` is honored only when the configured chunker
            // yields exactly one chunk for this document — i.e. the
            // extractor was paired with `passthrough`. The override is a
            // doc-level summary; mapping it across multiple chunks would
            // be ambiguous, so we silently fall through to per-chunk
            // content embedding for multi-chunk extractors. See the
            // `embed_text` doc on `ExtractedDoc` for context.
            let single_chunk_embed_override = if text_chunks.len() == 1 {
                doc.embed_text.as_deref()
            } else {
                None
            };

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
                // Embed-side dedup gate. If we already have a row
                // with this content_hash (from a prior run, or from
                // earlier in this run), skip re-embedding. This is
                // the load-bearing mitigation for the resume-cursor
                // bug — without it, a second ingest pass over the
                // same source data re-embeds everything and
                // compounds the duplicate count.
                if seen_hashes.contains(&content_hash) {
                    dedup_skipped += 1;
                    continue;
                }
                seen_hashes.insert(content_hash.clone());
                // Promote code-intelligence metadata from the extractor's
                // metadata JSON into typed columns. Non-code extractors
                // leave the JSON untouched and `code_meta_from_json`
                // returns all-None → stored as Null columns.
                let code = crate::index::code_meta_from_json(doc.metadata.as_ref());
                let embed_input = single_chunk_embed_override
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| content.clone());
                pending_texts.push(embed_input);
                pending_chunks.push(InsertChunk {
                    content,
                    title: doc.title.clone(),
                    url: doc.url.clone(),
                    metadata: doc.metadata.as_ref().map(|m| m.to_string()),
                    content_hash: Some(content_hash),
                    source_doc_id: doc.url.clone().or_else(|| Some(doc.source_id.clone())),
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
                    // Cooperative yield to foreground inference.
                    //
                    // An embed batch is an atomic `llama_decode` that
                    // can occupy the GPU for several seconds; while
                    // it runs the primary chat slot can't interleave
                    // tokens. So before STARTING the next batch we
                    // ask the daemon "is the user actively chatting?"
                    // and park here until they're idle. Granularity
                    // is per-batch, which is the finest the backend
                    // actually allows.
                    //
                    // Cancel beats yield: if the user cancels mid-yield
                    // we exit the same way the per-doc cancel check
                    // does, so /internal/corpus/pause stays
                    // responsive.
                    if let Some(hook) = self.yield_hook() {
                        let mut announced = false;
                        while hook.should_yield() {
                            if cancel_flag.is_cancelled() {
                                tracing::info!(
                                    corpus = %recipe.corpus.id,
                                    iter_pos,
                                    total_chunks,
                                    "ingest cancelled while yielding to foreground inference"
                                );
                                return Err(Error::Cancelled(recipe.corpus.id.clone()));
                            }
                            if !announced {
                                announced = true;
                                tracing::info!(
                                    corpus = %recipe.corpus.id,
                                    "yield: pausing embed batches for foreground inference"
                                );
                                if let Some(ref cb) = progress {
                                    cb(IngestProgress::Embedding {
                                        chunks_embedded: total_chunks + index_buffer.len() as u64,
                                        total: 0,
                                        docs_processed: resume_iter_pos + docs_processed,
                                        chunks_per_sec: 0.0,
                                        expected_docs: expected_filter_docs,
                                    });
                                }
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                        if announced {
                            tracing::info!(
                                corpus = %recipe.corpus.id,
                                "yield: resumed embed batches — foreground idle"
                            );
                            embed_timer = Instant::now();
                        }
                    }

                    let embed_start = Instant::now();
                    let embed_count = pending_texts.len();
                    // `vector = false` (recipe `[index]`) means no ANN index is
                    // built — so running the embedding model here, the dominant
                    // ingest cost, is pure waste. Store correctly-sized zero
                    // vectors: the chunk schema stays satisfied, FTS still
                    // indexes the text, and `build_indexes(build_vector=false, …)`
                    // skips the IVF-PQ build, so these vectors are never read.
                    // This lets a deterministic, atoms-only corpus (e.g. the SF
                    // parcel roll, whose analytics read atoms.json, not vectors)
                    // ingest ~40× faster. Gated on the flag, so every
                    // `vector = true` ingest is byte-for-byte unchanged.
                    let embeddings: Vec<Vec<f32>> = if !recipe.index.vector {
                        let dims = recipe.index.embedding_dimensions.max(1);
                        vec![vec![0.0f32; dims]; pending_texts.len()]
                    } else if let Some(ref batch_embed) = self.batch_embed {
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

                    // Per-batch throttle. Distinct from `should_yield`
                    // (which fully pauses while chat is active): this
                    // shares the machine over a 24h ingest by sleeping
                    // a fraction of the wall time after each batch. At
                    // factor 1.0 (default) the sleep is zero. At 0.5
                    // we sleep `embed_ms` after each `embed_ms` of
                    // work — duty cycle 50%.
                    if let Some(hook) = self.yield_hook() {
                        let factor = hook.throttle_factor().clamp(0.001, 1.0);
                        if factor < 1.0 && embed_ms > 0 {
                            let sleep_ms = ((embed_ms as f32) * (1.0 / factor - 1.0)) as u64;
                            if sleep_ms > 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(sleep_ms))
                                    .await;
                            }
                        }
                    }

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
                            expected_docs: expected_filter_docs,
                        });
                    }
                }

                // Tier 2: flush to index when buffer is large enough.
                if index_buffer.len() >= INDEX_FLUSH_SIZE {
                    let flush_count = index_buffer.len();
                    let insert_start = Instant::now();
                    index.insert_batch(&index_buffer).await?;
                    let insert_ms = insert_start.elapsed().as_millis();
                    if !unit_scoped {
                        // Partition-wide resume cursor is meaningless for
                        // unit-scoped runs — the next unit comes with its
                        // own bounded source iterator, not a continuation.
                        // Pass the captured `assigned_shard_set` so the
                        // next resume can detect drift if `processed_shards`
                        // mutates between this commit and the restart.
                        let _ = index.update_committed_iter_pos_with_shards(
                            iter_pos,
                            assigned_shard_set.as_deref(),
                        );
                    }
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
            if !unit_scoped {
                let _ = index
                    .update_committed_iter_pos_with_shards(iter_pos, assigned_shard_set.as_deref());
            }

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
        // Unit-scoped runs skip this entirely — the merge leader builds
        // indexes once after every peer's unit is merged, so a per-unit
        // build both wastes work and corrupts the partition-of-self state
        // for the next unit that lands in this dir.
        // Otherwise, skip if already completed in a previous run — this is
        // the common case when a process was killed after build_indexes()
        // but before mark_ingestion_complete(). We detect it via the
        // `indexes_built` flag so we don't waste minutes rebuilding.
        if unit_scoped {
            eprintln!(
                "[{}] Unit-scoped run — deferring index build to merge leader",
                recipe.corpus.id,
            );
        } else if CorpusIndex::indexes_are_built(index_path) {
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
            let est_partitions = est_partitions.clamp(2, 512);
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
                progress
                    .as_ref()
                    .map(|cb| -> Box<dyn Fn(u64, u64) + Send + Sync> {
                        Box::new(move |done, total_phases| {
                            cb(IngestProgress::Indexing {
                                chunks_indexed: total_chunks * done / total_phases,
                                total: total_chunks,
                            });
                        })
                    });
            index
                .build_indexes(build_vector, build_fts, sub_phase_cb.as_deref())
                .await?;
            // Checkpoint: if killed after this point, resume can skip rebuild.
            let _ = index.mark_indexes_built();
        }

        // Optional enrichment phase: field model enrichment.
        if let Some(enrichment_config) = recipe.enrichment.as_ref() {
            if enrichment_config.enabled {
                match self.inference.as_ref() {
                    Some(inference) => {
                        // Cooperative yield before enrichment kicks
                        // off. Each enrichment phase issues many
                        // long-running calls into the chat slot
                        // (atlas extraction, cluster labelling) — the
                        // exact contention foreground chat is trying
                        // to avoid. Block here until the user is
                        // idle, then proceed. Once the phase starts,
                        // mid-phase preemption is intentionally not
                        // attempted: cluster state is built up
                        // incrementally and a partial run would
                        // corrupt the checkpoint.
                        if let Some(hook) = self.yield_hook() {
                            let mut announced = false;
                            while hook.should_yield() {
                                if cancel_flag.is_cancelled() {
                                    tracing::info!(
                                        corpus = %recipe.corpus.id,
                                        "ingest cancelled while yielding before enrichment"
                                    );
                                    return Err(Error::Cancelled(recipe.corpus.id.clone()));
                                }
                                if !announced {
                                    announced = true;
                                    tracing::info!(
                                        corpus = %recipe.corpus.id,
                                        "yield: deferring enrichment for foreground inference"
                                    );
                                }
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                            if announced {
                                tracing::info!(
                                    corpus = %recipe.corpus.id,
                                    "yield: resuming enrichment — foreground idle"
                                );
                            }
                        }

                        // Tiered enrichment path (spec
                        // `sovereign/docs/specs/CONV_TIERED_PORT.md`).
                        // Replaces the legacy field-model atlas
                        // pipeline for conversation corpora. The two
                        // paths are mutually exclusive; tiered skips
                        // the FieldModelEngine block via labeled
                        // break and falls through to the same
                        // IngestResult shape via the post-block
                        // `index.info()` summary.
                        'enrichment: {
                            if enrichment_config.enrichment_type == "tiered" {
                                // Two tiered variants: the conv-grouping
                                // one (`run_tiered_enrichment`) buckets
                                // chunks by `conv_uuid` (per the conv
                                // corpora schema), and the folder-grouping
                                // one (`run_folder_tiered_enrichment`)
                                // buckets by `source_doc_id` (one bag per
                                // file, what watched-folder and vault
                                // corpora produce). Pick by recipe's
                                // display.category — vault + watched
                                // folders take the folder variant.
                                let display_category = recipe
                                    .display
                                    .as_ref()
                                    .and_then(|d| d.category.as_deref())
                                    .unwrap_or("");
                                let is_folder_shape =
                                    matches!(display_category, "vault" | "watched_folder");
                                if is_folder_shape {
                                    crate::enrichment::tiered::run_folder_tiered_enrichment(
                                        &recipe.corpus.id,
                                        index_path,
                                        self.tiered_provider(),
                                        self.chunk_entity_extractor(),
                                    )
                                    .await?;
                                } else {
                                    crate::enrichment::tiered::run_tiered_enrichment(
                                        recipe,
                                        index_path,
                                        self.tiered_provider(),
                                        self.chunk_entity_extractor(),
                                    )
                                    .await?;
                                }
                                break 'enrichment;
                            }

                            // Investigation enrichment is an explicit, opt-in
                            // step (`sovereign enrich investigation build
                            // <id>`) that runs the typed entity/relationship
                            // pipeline — NOT the field-model domain registry
                            // below. Skip it here so an investigation-type
                            // recipe installs + finalizes cleanly instead of
                            // tripping `UnknownEnrichmentDomain` when the
                            // recipe's `enrichment.domain` isn't a registered
                            // field-model domain.
                            if enrichment_config.enrichment_type == "investigation" {
                                tracing::info!(
                                    corpus = %recipe.corpus.id,
                                    "install: skipping auto-enrichment for investigation recipe — \
                                     run `sovereign enrich investigation build <id>` to enrich"
                                );
                                break 'enrichment;
                            }

                            let field_engine =
                                crate::enrichment::field_engine::FieldModelEngine::from_recipe(
                                    recipe,
                                    self.embed.clone(),
                                    inference.clone(),
                                )?;
                            let id = recipe.corpus.id.clone();
                            // Bridge enrichment-phase events to the outer
                            // `IngestProgress` channel so HTTP consumers
                            // (desktop UI, CLI poll) see real-time phase
                            // transitions during Phase 1 / 1b / 2 /
                            // clustering / 3 instead of staring at the
                            // last `Embedding` event. Without this bridge,
                            // a long enrichment phase looked like a hang
                            // (observed 2026-05-20: conversations-anthropic
                            // ingest stuck at "Embedding chunks…" while
                            // HDBSCAN clustered 16326×1024 silently).
                            let outer_progress = progress.as_ref();
                            let progress_fn =
                                move |p: crate::enrichment::clustering::EnrichmentProgress| {
                                    use crate::enrichment::clustering::EnrichmentProgress as EP;
                                    // Existing stderr-render path — unchanged so
                                    // log consumers see the same lines.
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
                                EP::Phase2bProgress { clusters_done, clusters_total, clusters_failed, consecutive_failures, last_error } => {
                                    if *consecutive_failures >= 4 {
                                        eprintln!(
                                            "[{id}] Cluster labeling: {clusters_done}/{clusters_total} — {consecutive_failures} consecutive failures (last: {})",
                                            last_error.as_deref().unwrap_or("?"),
                                        );
                                    } else if *clusters_done == *clusters_total
                                        || clusters_done % 16 == 0
                                    {
                                        eprintln!(
                                            "[{id}] Cluster labeling: {clusters_done}/{clusters_total} ({clusters_failed} failed)"
                                        );
                                    }
                                }
                                EP::Phase2bComplete { labeled_count } =>
                                    eprintln!("[{id}] Cluster labeling complete: {labeled_count} clusters labeled"),
                            }

                                    // New: forward to the IngestProgress channel.
                                    // Mapping rules:
                                    //   - Phase variants emit `Enriching` with a
                                    //     stable machine-token phase name. The
                                    //     desktop UI maps these to display labels.
                                    //   - Numeric progress (Phase1Progress,
                                    //     ClusteringComplete) sets `fraction` so
                                    //     progress bars can move.
                                    if let Some(cb) = outer_progress {
                                        let evt = match &p {
                                    EP::Phase { phase, name, note } => {
                                        let detail = if note.is_empty() {
                                            format!("Phase {phase}: {name}")
                                        } else {
                                            format!("Phase {phase}: {name} ({note})")
                                        };
                                        Some(IngestProgress::Enriching {
                                            phase: format!("phase-{phase}"),
                                            detail,
                                            fraction: None,
                                        })
                                    }
                                    EP::PhaseSkipped { phase, name } => Some(IngestProgress::Enriching {
                                        phase: format!("phase-{phase}-skipped"),
                                        detail: format!("Phase {phase}: {name} — skipped (checkpoint)"),
                                        fraction: None,
                                    }),
                                    EP::Resuming { from_phase } => Some(IngestProgress::Enriching {
                                        phase: "resuming".into(),
                                        detail: format!("Resuming enrichment from {from_phase}"),
                                        fraction: None,
                                    }),
                                    EP::ClusteringStarted { total_chunks } => Some(IngestProgress::Enriching {
                                        phase: "clustering".into(),
                                        detail: format!("Clustering {total_chunks} chunks…"),
                                        fraction: None,
                                    }),
                                    EP::ClusteringStep { step, detail } => Some(IngestProgress::Enriching {
                                        phase: "clustering".into(),
                                        detail: format!("{step}: {detail}"),
                                        fraction: None,
                                    }),
                                    EP::ClusteringComplete { cluster_count, noise_chunks } => Some(IngestProgress::Enriching {
                                        phase: "clustering-complete".into(),
                                        detail: format!("Clustering complete: {cluster_count} clusters, {noise_chunks} noise"),
                                        fraction: Some(1.0),
                                    }),
                                    EP::Phase1Progress { batches_done, batches_total } => {
                                        let frac = if *batches_total > 0 {
                                            Some(*batches_done as f32 / *batches_total as f32)
                                        } else {
                                            None
                                        };
                                        Some(IngestProgress::Enriching {
                                            phase: "skeleton-extraction".into(),
                                            detail: format!("Skeleton extraction: {batches_done}/{batches_total} batches"),
                                            fraction: frac,
                                        })
                                    }
                                    EP::Phase2bProgress { clusters_done, clusters_total, clusters_failed, consecutive_failures, last_error } => {
                                        let frac = if *clusters_total > 0 {
                                            Some(*clusters_done as f32 / *clusters_total as f32)
                                        } else {
                                            None
                                        };
                                        let detail = if *consecutive_failures >= 4 {
                                            format!(
                                                "Cluster labeling: {clusters_done}/{clusters_total} ({clusters_failed} failed, {consecutive_failures} consecutive — last: {})",
                                                last_error.as_deref().unwrap_or("?"),
                                            )
                                        } else {
                                            format!("Cluster labeling: {clusters_done}/{clusters_total} ({clusters_failed} failed)")
                                        };
                                        Some(IngestProgress::Enriching {
                                            phase: "cluster-labeling".into(),
                                            detail,
                                            fraction: frac,
                                        })
                                    }
                                    EP::Phase2bComplete { labeled_count } => Some(IngestProgress::Enriching {
                                        phase: "cluster-labeling-complete".into(),
                                        detail: format!("Cluster labeling complete: {labeled_count} clusters labeled"),
                                        fraction: Some(1.0),
                                    }),
                                };
                                        if let Some(evt) = evt {
                                            cb(evt);
                                        }
                                    }
                                };
                            field_engine.enrich(&index, &progress_fn).await?;
                        } // end 'enrichment: block (tiered vs legacy field-model)
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

        // Unit-scoped runs intentionally leave `ingestion_in_progress: true`
        // and never set the source_path. The merge leader flips the flag
        // once every peer's unit is merged — marking it here would make
        // the next unit's create_or_resume fall through to fresh-create
        // and hit "Table 'chunks' already exists" from LanceDB.
        if !unit_scoped {
            // Mark the index as fully committed so it survives a restart as "Indexed"
            // rather than being treated as a partial/incomplete ingest.
            if let Err(e) = index.mark_ingestion_complete() {
                tracing::warn!(
                    "Failed to mark ingestion complete for '{}': {e}",
                    recipe.corpus.id
                );
            }

            // Stamp the canonical content fingerprint as the last
            // write — after `mark_ingestion_complete` so a peer
            // pulling against this fingerprint can trust the chunk
            // set is stable. Failures are logged but non-fatal:
            // the index is still locally usable; mesh sync just
            // falls back to chunk-count comparisons.
            if let Err(e) = index.compute_and_stamp_fingerprint().await {
                tracing::warn!(
                    corpus = recipe.corpus.id.as_str(),
                    error = %e,
                    "finalise: fingerprint stamping failed; mesh sync \
                     will fall back to chunk-count comparisons"
                );
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

            // Phase B incremental NER hook (spec
            // `sovereign/docs/specs/PROGRESSIVE_ENRICHMENT.md`
            // §"Incremental update strategy"). For
            // conversation-category corpora — `conversation-history`
            // via the KnowledgeView debouncer, `conversations-personal`
            // via Settings → Imports — every successful re-ingest
            // fires `extract_delta_for_corpus` on the wired GliNER
            // extractor so chunks added since the last Phase A
            // backfill get NER mentions without an operator-initiated
            // `sovereign corpus extract-entities` run. Best-effort:
            // a missing extractor (model not installed) or a transient
            // store failure logs and the ingest still finishes green.
            let is_conv_category = recipe
                .display
                .as_ref()
                .and_then(|d| d.category.as_deref())
                .map(|c| c == "conversation")
                .unwrap_or(false);
            if is_conv_category {
                if let Some(extractor) = self.chunk_entity_extractor() {
                    match extractor
                        .extract_delta_for_corpus(&recipe.corpus.id, index_path)
                        .await
                    {
                        Ok(0) => tracing::debug!(
                            corpus = %recipe.corpus.id,
                            "phase_b: incremental NER — no new chunks since last extraction"
                        ),
                        Ok(n) => tracing::info!(
                            corpus = %recipe.corpus.id,
                            new_mentions = n,
                            "phase_b: incremental NER complete"
                        ),
                        Err(e) => tracing::warn!(
                            corpus = %recipe.corpus.id,
                            error = %e,
                            "phase_b: incremental NER failed (non-fatal — Phase A snapshot retained)"
                        ),
                    }
                }
            }
        }

        eprintln!(
            "[{}] Ingestion complete — {total_chunks} chunks in {}m{}s",
            recipe.corpus.id,
            duration_secs / 60,
            duration_secs % 60,
        );
        if dedup_skipped > 0 {
            // Surface the gate's effect so operators can verify the
            // resume-cursor mitigation is doing real work. A
            // non-zero count means we avoided embedding chunks
            // whose content_hash was already in the index — the
            // exact failure mode that blew up the original
            // wikipedia ingest.
            eprintln!(
                "[{}] Embed-side dedup gate skipped {} chunks (already-embedded content)",
                recipe.corpus.id, dedup_skipped
            );
        }

        // ── Deterministic typed-atom emission for `tabular_atoms`
        // recipes (e.g. the SF assessor parcel roll). Runs
        // unconditionally — independent of any `[enrichment]` block —
        // re-reading the acquired rows and writing one typed `Entity`
        // atom per row via the canonical atlas writer. No inference: the
        // figures the LVT analytics later cite are read from these atoms,
        // never originated by a model (ARCH glassbox + the "no
        // confabulated numbers" invariant). The `Extractor` trait can't
        // do this (it yields docs, has no atlas dir), so the orchestrator
        // calls the sibling pure builder over the same parsed rows.
        if let ExtractorConfig::TabularAtoms {
            document_path,
            id_column,
            entity_type,
            numeric_attributes,
            string_attributes,
        } = &recipe.extract
        {
            let cfg = crate::extractors::tabular_atoms::TabularAtomsConfig {
                document_path: document_path.clone().unwrap_or_else(|| "$[*]".to_string()),
                id_column: id_column.clone(),
                entity_type: entity_type.clone().unwrap_or_else(|| "row".to_string()),
                numeric_attributes: numeric_attributes.clone(),
                string_attributes: string_attributes.clone(),
            };
            let rows =
                crate::extractors::tabular_atoms::parse_rows(&source_path, &cfg.document_path)?;
            let atoms =
                crate::extractors::tabular_atoms::build_atoms(&rows, &cfg, &recipe.corpus.id);
            let atlas_dir = self.index_dir.join(&recipe.corpus.id).join("atlas");
            let atom_count = atoms.len();
            crate::enrichment::atlas::writer::write_atlas(&atlas_dir, &atoms, &[], &[])?;
            tracing::info!(
                corpus = %recipe.corpus.id,
                atoms = atom_count,
                rows = rows.len(),
                "tabular_atoms: wrote deterministic typed atoms to atlas"
            );
        }

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
            CorpusSpec::Inline(recipe) => Ok((**recipe).clone()),
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
                    AcquirerConfig::HuggingFaceDataset {
                        ref mut file_indices,
                        ..
                    },
                    _,
                ) => {
                    *file_indices = Some(indices);
                }
                (
                    _,
                    ExtractorConfig::WikipediaJsonl {
                        ref mut shard_indices,
                        ..
                    },
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
        if let (
            Some(range),
            ExtractorConfig::WikipediaJsonl {
                ref mut article_range,
                ..
            },
        ) = (article_range, &mut recipe.extract)
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

        // `ingest_with_overrides` is the entrypoint for peer-pulled
        // work — both the legacy `ingest_partition` route and the
        // pull-loop. Stamp PeerPulled so a daemon-restart auto-resume
        // can distinguish this from a self-initiated install.
        self.ingest_inner(&recipe, output_path, &progress, unit_id, true)
            .await
    }
}
