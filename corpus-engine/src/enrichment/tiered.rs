// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation tiered-retrieval enrichment runner (Phase B port).
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Replaces the legacy field-model atlas enrichment for conversation
//! corpora. Reads chunks from the corpus's Lance index, groups by
//! `source_doc_id` (= `conv_uuid`), and runs the Phase A tiered
//! pipeline per-conversation:
//!
//! - **T2** lean entity extraction + action atoms + per-conv entity
//!   graph (for PPR multi-hop retrieval).
//! - **T3** RAPTOR cluster tree + TF-IDF motifs + TextTiling segments
//!   + overview (overview reuses `metadata.title` from the source
//!   export per v0 opt-3).
//!
//! Writes go to the SQLite sidecar tables `conv_skeletons`,
//! `conv_raptor_nodes`, `conv_motifs` (migration:
//! `sovereign-store/src/migrations.rs::run_conv_tiered_migration`).
//!
//! ## v0 status (Move 3 step 3)
//!
//! - Reads Lance chunks, groups by `source_doc_id`, classifies each
//!   conversation into a size bucket per the spec's "Performance
//!   budget" section, and emits per-bucket stats to tracing.
//! - **Persistence and per-conv RAPTOR / entity-graph / motif calls
//!   are deferred to step 4 / 5.** Those land via a
//!   `TieredEnrichmentProvider` trait wired through `CorpusEngine`
//!   (similar pattern to `InferenceFn`) so corpus-engine doesn't take
//!   a cyclic dep on `sovereign-tools` where `build_raptor_atlas`
//!   lives.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::enrichment::state::{EnrichmentPhase, EnrichmentStateFile};
use crate::error::Result;
use crate::index::{CorpusIndex, EnrichmentChunkRow};
use crate::recipe::Recipe;

/// Shared handle to a `TieredEnrichmentProvider` impl. `Arc<dyn>` so
/// the daemon can pass one instance through `CorpusEngine` without
/// taking ownership.
pub type TieredProviderHandle = Arc<dyn TieredEnrichmentProvider>;

/// Shared handle for the per-chunk NER extractor (GliNER today, via
/// `sovereign-tools::gliner_ner`). Optional second hook fired by the
/// tiered runner ahead of the heavy `TieredEnrichmentProvider` call
/// — runs the cheap CPU-only NER pass first so the chunk_entities
/// table populates even when the LLM-side enrichment fails or is
/// killed mid-run. `None` falls back to RAPTOR-derived entities only.
pub type ChunkEntityExtractorHandle = Arc<dyn ChunkEntityExtractor>;

/// Per-chunk named-entity extractor. corpus-engine declares the
/// trait so the dispatch loop can fire it per-conversation;
/// sovereign-tools owns the concrete impl (where the `gline-rs` dep
/// lives) and the SqliteStateStore persistence path.
///
/// One call per conversation: implementor batches chunks internally
/// for throughput. Returns the count of mentions persisted so the
/// runner can surface a "extracted N entities" log line.
#[async_trait::async_trait]
pub trait ChunkEntityExtractor: Send + Sync {
    async fn extract_for_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
    ) -> Result<usize>;

    /// Phase B incremental hook (spec
    /// `sovereign/docs/specs/PROGRESSIVE_ENRICHMENT.md` §"Incremental
    /// update strategy"). Called by `CorpusEngine::ingest` after a
    /// conversation-category corpus's ingest succeeds. Implementor
    /// scans the index for chunks NOT yet in `chunk_entities`, runs
    /// extraction only on the delta, and flips
    /// `chunk_entity_progress.state` to `"incremental"`.
    ///
    /// Default impl is a no-op so extractors that haven't opted into
    /// incremental (e.g. RAPTOR-only paths, a hypothetical static-
    /// corpus extractor) keep working with the snapshot-only Phase A
    /// CLI.
    async fn extract_delta_for_corpus(
        &self,
        _corpus_id: &str,
        _index_path: &Path,
    ) -> Result<usize> {
        Ok(0)
    }
}

/// Provider trait for the heavy tiered-enrichment work
/// (`build_raptor_atlas`, entity-graph extraction, motif
/// classification, persistence). corpus-engine knows about the trait
/// but ships no concrete impl — sovereign-tools provides one (where
/// `build_raptor_atlas` lives) and injects it into `CorpusEngine`
/// before ingest runs, mirroring the existing `InferenceFn`
/// inversion.
///
/// The provider owns the entire per-conversation work unit including
/// SQLite persistence to the `conv_skeletons` / `conv_raptor_nodes` /
/// `conv_motifs` sidecar tables; corpus-engine just dispatches one
/// call per non-`Tiny` conversation.
///
/// **`Tiny` conversations bypass the provider entirely** — the
/// dispatch runner persists a synthetic single-node entry directly
/// (opt-2 in the spec performance budget).
#[async_trait::async_trait]
pub trait TieredEnrichmentProvider: Send + Sync {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> Result<()>;

    /// Called once after every per-source `enrich_conversation` for a
    /// corpus has completed (success or failure). Implementations use
    /// this to run cross-source synthesis work that depends on the
    /// full per-source set being persisted — e.g. the vault-wide
    /// RAPTOR theme synthesis in `FolderTieredProvider`. The default
    /// is a no-op so providers that don't need finalization (the
    /// conversation provider) inherit it for free without a change.
    ///
    /// Errors here are logged by the dispatcher but do not bubble up
    /// to the corpus ingest as fatal — the per-source enrichment is
    /// the load-bearing output; finalization is a briefing-only
    /// enhancement.
    async fn finalize_corpus(&self, _corpus_id: &str) -> Result<()> {
        Ok(())
    }

    /// Re-run per-source enrichment for only the source_doc_ids
    /// supplied. Used by the watched-folder sweeper to do incremental
    /// re-enrichment after `apply_watched_diff` lands a delta.
    /// Default no-op so the conv provider inherits a sensible
    /// fallback; `FolderTieredProvider` overrides to do per-doc work
    /// + a finalize pass.
    async fn reenrich_sources(&self, _corpus_id: &str, _source_doc_ids: &[String]) -> Result<()> {
        Ok(())
    }

    /// Skip-already-built fast path for `run_folder_tiered_enrichment`.
    /// Answers "is `conv_uuid` already fully enriched AND unchanged since,
    /// so the runner can skip it entirely?" — no chunk fetch, no LLM, no
    /// checkpoint load. This is what makes an interrupted vault build
    /// "pick up from note 320" instead of re-grinding all 320 already-
    /// built notes: the per-note RAPTOR checkpoint only makes a *re-run*
    /// cheap, but a re-run of 320 done notes is still 320 store round-
    /// trips + node re-persists. Skipping them outright is the real win.
    ///
    /// Default `false` (never skip) so the conversation provider keeps
    /// its rebuild-everything behavior unchanged. `chunk_count` is the
    /// live count the runner is about to dispatch; an impl must return
    /// `true` only when its persisted state for `conv_uuid` is terminal
    /// (`Ready`) AND still matches that count, so a note whose chunk set
    /// changed (a content edit re-chunks with new ids) still rebuilds.
    /// A pending user correction must also veto the skip so the guided
    /// re-enrich actually runs.
    async fn note_already_current(
        &self,
        _corpus_id: &str,
        _conv_uuid: &str,
        _chunk_count: usize,
    ) -> bool {
        false
    }

    /// Skip-already-built fast path for `run_tiered_enrichment` (the
    /// CONVERSATION runner). Same intent as [`Self::note_already_current`]
    /// but keyed on chunk CONTENT rather than chunk_count, because
    /// conversation corpora have no changed-source sweep the way watched
    /// folders do (`reenrich_sources`): the folder runner can trust
    /// chunk_count because a genuine content edit re-enrichs via the
    /// sweep, but a conversation is only ever re-touched by a whole-
    /// archive RE-IMPORT — so an edited conversation that happens to
    /// re-chunk to the SAME count must still rebuild. The runner passes
    /// the chunks it just fetched (chunk ids are reallocated on re-import,
    /// so an id-based signal is useless — the impl must hash the text). An
    /// impl returns `true` only when its persisted state for `conv_uuid`
    /// is terminal (`Ready`) AND the stored content hash matches these
    /// chunks AND no pending user correction vetoes. Default `false` so
    /// providers that don't track content hashes never skip.
    async fn note_content_current(
        &self,
        _corpus_id: &str,
        _conv_uuid: &str,
        _chunks: &[EnrichmentChunkRow],
    ) -> bool {
        false
    }

    /// Best-effort work that runs AFTER the runner stamps the terminal
    /// `Complete` — so it can never gate the user-facing "map ready"
    /// signal (the desktop "Building the map" banner). The folder
    /// provider uses this for the bench-side typed-extension pass
    /// (atoms.json): chat retrieval is unaffected by it, yet it is
    /// LLM-heavy and would otherwise hold the vault non-terminal for
    /// minutes while it ran inside `finalize_corpus`. Implementations
    /// should return promptly (spawn detached work if it is slow); the
    /// runner does not await any spawned task and the corpus is already
    /// `Complete`, so a killed deferred pass simply re-runs on the next
    /// enrichment. Default no-op.
    async fn post_finalize_corpus(&self, _corpus_id: &str) {}
}

/// Size bucket for a single conversation; drives the slot routing
/// (Fast vs Slow) and the skip-RAPTOR opt-2 decision per the spec
/// "Performance budget" section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConvBucket {
    /// `< 8` chunks. Opt-2: persist a synthetic single node from the
    /// conv title; no LLM call.
    Tiny,
    /// `8..=30` chunks. Opt-1: route summarization through `Speed::Fast`
    /// (9B model). Batched 8-at-a-time per opt-4 when v1 lands.
    Small,
    /// `31..=100` chunks. Fast slot per leaf, ~1-3 LLM calls.
    Medium,
    /// `101..=300` chunks. Fast slot for leaves, Slow for root.
    Large,
    /// `> 300` chunks. Full Phase A treatment, Slow slot throughout.
    LongTail,
}

impl ConvBucket {
    pub fn classify(chunk_count: usize) -> Self {
        match chunk_count {
            0..=7 => ConvBucket::Tiny,
            8..=30 => ConvBucket::Small,
            31..=100 => ConvBucket::Medium,
            101..=300 => ConvBucket::Large,
            _ => ConvBucket::LongTail,
        }
    }

    /// Bucket classification for per-FILE units (vault notes,
    /// watched-folder documents) instead of chat conversations.
    ///
    /// `classify`'s 8-chunk Tiny floor is tuned to chat exports,
    /// where a sub-8-chunk conversation genuinely is small talk. A
    /// vault note at the semantic chunker's ~2048 chars/chunk is a
    /// COMPLETE argumentative essay at 3-7 chunks — bucketing it
    /// Tiny replaces its RAPTOR summary with a title-only synthetic
    /// node, which silently exempts the note from everything
    /// downstream of `conv_raptor_nodes`: T3 signposts, vault
    /// themes, and the typed-extension pass. Measured on the live
    /// vault (2026-06-11): 23 of 46 notes — including 5 of the
    /// obsidian golden's 10 sampled essays — were Tiny under
    /// `classify`, which is why the typed axes scored near zero.
    ///
    /// Per-file Tiny is therefore only the truly degenerate case:
    /// a 0-or-1-chunk file, where there is nothing to cluster.
    pub fn classify_note(chunk_count: usize) -> Self {
        match chunk_count {
            0..=1 => ConvBucket::Tiny,
            2..=30 => ConvBucket::Small,
            31..=100 => ConvBucket::Medium,
            101..=300 => ConvBucket::Large,
            _ => ConvBucket::LongTail,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ConvBucket::Tiny => "tiny",
            ConvBucket::Small => "small",
            ConvBucket::Medium => "medium",
            ConvBucket::Large => "large",
            ConvBucket::LongTail => "long_tail",
        }
    }
}

/// Per-bucket aggregate stats reported up the call stack and through
/// tracing so operators can see the work shape before steps 4/5 fire
/// the actual LLM calls.
#[derive(Debug, Clone, Default)]
pub struct TieredDispatchPlan {
    pub corpus_id: String,
    pub total_conversations: usize,
    pub total_chunks: usize,
    pub per_bucket: BTreeMap<ConvBucket, BucketSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct BucketSummary {
    pub conversations: usize,
    pub chunks: usize,
    /// Max chunk count in this bucket (drives long-tail outlier
    /// awareness even though the bucket boundary already gates).
    pub max_chunks_in_conv: usize,
}

/// Run tiered enrichment over a corpus's Lance index.
///
/// v0 (step 3): reads the index, groups by `source_doc_id`, classifies
/// each conversation into a `ConvBucket`, emits stats. Returns the
/// dispatch plan for upstream observability. No LLM calls, no SQLite
/// writes.
///
/// Subsequent steps will:
///
/// - **Step 4**: invoke a `TieredEnrichmentProvider` trait (impl in
///   `sovereign-tools`) per non-`Tiny` conversation to build the T2
///   entity graph + persist into `conv_skeletons.skeleton_json`.
/// - **Step 5**: same trait extends to T3 — `build_raptor_atlas`
///   per conv, motif extraction + classification, segments via
///   TextTiling, persist into `conv_raptor_nodes` / `conv_motifs`.
pub async fn run_tiered_enrichment(
    recipe: &Recipe,
    index_path: &Path,
    provider: Option<&TieredProviderHandle>,
    entity_extractor: Option<&ChunkEntityExtractorHandle>,
) -> Result<TieredDispatchPlan> {
    let corpus_id = recipe.corpus.id.clone();
    tracing::info!(
        corpus = %corpus_id,
        index = %index_path.display(),
        has_provider = provider.is_some(),
        has_entity_extractor = entity_extractor.is_some(),
        "tiered enrichment: scanning chunks index for per-conversation grouping"
    );

    let index = CorpusIndex::open(index_path).await?;
    let groups = index.group_chunks_by_source_doc().await?;

    let mut plan = TieredDispatchPlan {
        corpus_id: corpus_id.clone(),
        total_conversations: groups.len(),
        total_chunks: groups.values().map(|v| v.len()).sum(),
        per_bucket: BTreeMap::new(),
    };

    let mut conv_buckets: Vec<(String, ConvBucket)> = Vec::with_capacity(groups.len());
    for (conv_uuid, chunk_ids) in &groups {
        let bucket = ConvBucket::classify(chunk_ids.len());
        let entry = plan.per_bucket.entry(bucket).or_default();
        entry.conversations += 1;
        entry.chunks += chunk_ids.len();
        entry.max_chunks_in_conv = entry.max_chunks_in_conv.max(chunk_ids.len());
        conv_buckets.push((conv_uuid.clone(), bucket));
    }

    tracing::info!(
        corpus = %corpus_id,
        conversations = plan.total_conversations,
        chunks = plan.total_chunks,
        "tiered enrichment: dispatch plan computed (per-bucket stats below)"
    );
    for (bucket, summary) in &plan.per_bucket {
        tracing::info!(
            corpus = %corpus_id,
            bucket = bucket.label(),
            conversations = summary.conversations,
            chunks = summary.chunks,
            max_chunks_in_conv = summary.max_chunks_in_conv,
            "tiered enrichment: bucket summary"
        );
    }

    let Some(provider) = provider else {
        tracing::warn!(
            corpus = %corpus_id,
            "tiered enrichment: no TieredEnrichmentProvider injected — emitting dispatch plan only. Wire one via CorpusEngine::with_tiered_provider to actually run T2/T3."
        );
        return Ok(plan);
    };

    // Per-conversation dispatch. Sort by ascending chunk count so the
    // cheapest (`Tiny`) work fires first — operator sees progress
    // quickly and any provider crash on a tiny conv surfaces before
    // committing to long-tail Slow-slot work.
    conv_buckets.sort_by_key(|(uuid, _)| groups.get(uuid).map(|v| v.len()).unwrap_or(0));

    let mut completed = 0usize;
    let mut failed = 0usize;
    for (conv_uuid, bucket) in conv_buckets {
        let rows = match index
            .chunks_for_source_doc_with_embeddings(&conv_uuid)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    conv = %conv_uuid,
                    error = %e,
                    "tiered enrichment: chunk fetch failed; skipping conv"
                );
                failed += 1;
                continue;
            }
        };
        if rows.is_empty() {
            // Group said this conv had chunks but the embedding-aware
            // fetch returned none. Likely T1 embeddings missing for
            // this conv — log and skip.
            tracing::warn!(
                corpus = %corpus_id,
                conv = %conv_uuid,
                "tiered enrichment: conv has no embedded chunks; skipping"
            );
            failed += 1;
            continue;
        }
        let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();

        // Skip-already-built (conversation runner): if this conv is
        // already `Ready` and its content is byte-identical to the last
        // enrichment, skip the expensive NER + RAPTOR passes below. This
        // is what stops a chat RE-IMPORT from re-grinding every already-
        // enriched conversation. Content-hash (not chunk_count) because
        // conversation corpora have no changed-source sweep, so an
        // edited-but-same-length conv must still rebuild — see
        // `TieredEnrichmentProvider::note_content_current`. The fetch
        // above is cheap; the GliNER + LLM work below is the cost we save.
        if provider
            .note_content_current(&corpus_id, &conv_uuid, &chunks)
            .await
        {
            tracing::debug!(
                corpus = %corpus_id,
                conv = %conv_uuid,
                "tiered enrichment: conv already Ready and content unchanged; skipping NER + RAPTOR"
            );
            completed += 1;
            continue;
        }

        // Cheap CPU-only pass first: per-chunk NER. Runs ahead of
        // the LLM-heavy provider call so even if the provider fails
        // (e.g. inference timeout), chunk_entities still
        // populates and the conv-entity-graph builder has something
        // dense to work with. Optional — None means "skip NER".
        if let Some(extractor) = entity_extractor {
            // Clone chunks because the provider call below consumes
            // the originals. Cheap relative to the NER work itself.
            let chunks_for_ner = chunks.clone();
            match extractor
                .extract_for_conversation(&corpus_id, &conv_uuid, chunks_for_ner)
                .await
            {
                Ok(n) => {
                    tracing::debug!(
                        corpus = %corpus_id,
                        conv = %conv_uuid,
                        mentions = n,
                        "tiered enrichment: per-chunk NER persisted"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %corpus_id,
                        conv = %conv_uuid,
                        error = %e,
                        "tiered enrichment: per-chunk NER failed; continuing with RAPTOR-only entities for this conv"
                    );
                }
            }
        }

        if let Err(e) = provider
            .enrich_conversation(&corpus_id, &conv_uuid, chunks, embeddings, bucket)
            .await
        {
            tracing::warn!(
                corpus = %corpus_id,
                conv = %conv_uuid,
                bucket = bucket.label(),
                error = %e,
                "tiered enrichment: provider failed for conv; continuing with next"
            );
            failed += 1;
            continue;
        }
        completed += 1;
    }
    tracing::info!(
        corpus = %corpus_id,
        completed,
        failed,
        "tiered enrichment: per-conv dispatch finished"
    );

    // Run-level terminal stamp. Same reasoning as the folder variant:
    // the provider no longer stamps a per-conversation `Complete` on the
    // shared per-corpus state file, so the runner owns the one terminal
    // transition once the whole per-conv loop has settled. (The engine
    // wires `FolderTieredProvider` for both runners, so both must stamp.)
    stamp_folder_terminal(index_path, &corpus_id, completed, failed);

    Ok(plan)
}

/// Run tiered enrichment over a watched-folder corpus's Lance index,
/// dispatching the provider once per `source_doc_id`. Each document
/// becomes its own RAPTOR tree + motif index keyed by
/// `conv_uuid = source_doc_id`, matching the shape conversation
/// corpora already use.
///
/// This mirrors [`run_tiered_enrichment`] in body — the only reason
/// it exists as a separate entry point is that watched-folder
/// callers carry a `corpus_id` directly (no `Recipe`), so the
/// dispatch starts from the raw id + index path.
///
/// Heterogeneous folder shapes (e.g. a finance memo next to a
/// physics paper) used to collapse into a single bag with one
/// noisy RAPTOR tree spanning unrelated topics; per-doc dispatch
/// preserves per-document topical coherence and makes the Atlas
/// view show one card per file.
pub async fn run_folder_tiered_enrichment(
    corpus_id: &str,
    index_path: &Path,
    provider: Option<&TieredProviderHandle>,
    entity_extractor: Option<&ChunkEntityExtractorHandle>,
) -> Result<TieredDispatchPlan> {
    let corpus_id = corpus_id.to_string();
    tracing::info!(
        corpus = %corpus_id,
        index = %index_path.display(),
        has_provider = provider.is_some(),
        has_entity_extractor = entity_extractor.is_some(),
        "tiered enrichment (folder): scanning chunks index for per-source_doc grouping"
    );

    let index = CorpusIndex::open(index_path).await?;

    // Cheap CPU-only NER pass first — populates `chunk_entities`
    // even if the heavy provider call below fails. The extractor's
    // delta variant scans per-source_doc internally and writes
    // each mention with `conv_uuid = source_doc_id`, matching the
    // key shape the per-doc RAPTOR dispatch below will use.
    if let Some(extractor) = entity_extractor {
        match extractor
            .extract_delta_for_corpus(&corpus_id, index_path)
            .await
        {
            Ok(n) => tracing::debug!(
                corpus = %corpus_id,
                mentions = n,
                "tiered enrichment (folder): per-chunk NER delta persisted"
            ),
            Err(e) => tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "tiered enrichment (folder): per-chunk NER failed; continuing with RAPTOR-only entities"
            ),
        }
    }

    let groups = index.group_chunks_by_source_doc().await?;

    let mut plan = TieredDispatchPlan {
        corpus_id: corpus_id.clone(),
        total_conversations: groups.len(),
        total_chunks: groups.values().map(|v| v.len()).sum(),
        per_bucket: BTreeMap::new(),
    };

    let mut doc_buckets: Vec<(String, ConvBucket)> = Vec::with_capacity(groups.len());
    for (doc_id, chunk_ids) in &groups {
        // Folder corpora are per-FILE units — a 3-chunk note is a
        // complete essay, not small talk. See `classify_note`.
        let bucket = ConvBucket::classify_note(chunk_ids.len());
        let entry = plan.per_bucket.entry(bucket).or_default();
        entry.conversations += 1;
        entry.chunks += chunk_ids.len();
        entry.max_chunks_in_conv = entry.max_chunks_in_conv.max(chunk_ids.len());
        doc_buckets.push((doc_id.clone(), bucket));
    }

    tracing::info!(
        corpus = %corpus_id,
        documents = plan.total_conversations,
        chunks = plan.total_chunks,
        "tiered enrichment (folder): dispatch plan computed (per-bucket stats below)"
    );
    for (bucket, summary) in &plan.per_bucket {
        tracing::info!(
            corpus = %corpus_id,
            bucket = bucket.label(),
            documents = summary.conversations,
            chunks = summary.chunks,
            max_chunks_in_doc = summary.max_chunks_in_conv,
            "tiered enrichment (folder): bucket summary"
        );
    }

    let Some(provider) = provider else {
        tracing::warn!(
            corpus = %corpus_id,
            "tiered enrichment (folder): no TieredEnrichmentProvider injected — emitting dispatch plan only"
        );
        return Ok(plan);
    };

    if groups.is_empty() {
        tracing::warn!(
            corpus = %corpus_id,
            "tiered enrichment (folder): corpus has no source documents; nothing to enrich"
        );
        // An empty folder is trivially complete — stamp the terminal so
        // the UI doesn't sit forever on whatever pre-loop phase (e.g.
        // EntityExtraction) the driver stamped.
        stamp_folder_terminal(index_path, &corpus_id, 0, 0);
        return Ok(plan);
    }

    // Ascending chunk count: cheapest docs (Tiny synthetic) fire
    // first so the operator sees the Atlas index populate quickly
    // and any provider crash surfaces before committing to the
    // long-tail RAPTOR work.
    doc_buckets.sort_by_key(|(doc_id, _)| groups.get(doc_id).map(|v| v.len()).unwrap_or(0));

    let mut completed = 0usize;
    let mut failed = 0usize;
    for (doc_id, bucket) in doc_buckets {
        // Skip-already-built: a note already `Ready` with an unchanged
        // chunk set needs no work, so an interrupted vault build resumes
        // where it stopped instead of re-grinding every already-enriched
        // note. This is one indexed store lookup — no chunk fetch, no
        // embedding load, no LLM. `groups` already holds the live
        // chunk-id set from the up-front scan, so the count is free.
        // Notes whose chunks changed (re-chunk → new count) or that
        // carry a pending user correction return false and fall through
        // to a full rebuild.
        let live_chunk_count = groups.get(&doc_id).map(|v| v.len()).unwrap_or(0);
        if provider
            .note_already_current(&corpus_id, &doc_id, live_chunk_count)
            .await
        {
            tracing::debug!(
                corpus = %corpus_id,
                doc = %doc_id,
                chunks = live_chunk_count,
                "tiered enrichment (folder): note already enriched and unchanged — skipping (skip-already-built)"
            );
            completed += 1;
            continue;
        }
        let rows = match index.chunks_for_source_doc_with_embeddings(&doc_id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    corpus = %corpus_id,
                    doc = %doc_id,
                    error = %e,
                    "tiered enrichment (folder): chunk fetch failed for source_doc; skipping"
                );
                failed += 1;
                continue;
            }
        };
        if rows.is_empty() {
            tracing::warn!(
                corpus = %corpus_id,
                doc = %doc_id,
                "tiered enrichment (folder): source_doc has no embedded chunks; skipping"
            );
            failed += 1;
            continue;
        }
        let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();

        // GliNER ran once up-front via `extract_delta_for_corpus`
        // — it already iterates per source_doc internally — so we
        // do NOT call the per-conversation extractor here. The
        // chunk_entities table is keyed `conv_uuid = source_doc_id`
        // already, so retrieval-time lookups match the per-doc
        // RAPTOR keys we're about to write.

        if let Err(e) = provider
            .enrich_conversation(&corpus_id, &doc_id, chunks, embeddings, bucket)
            .await
        {
            tracing::warn!(
                corpus = %corpus_id,
                doc = %doc_id,
                bucket = bucket.label(),
                error = %e,
                "tiered enrichment (folder): provider failed for source_doc; continuing with next"
            );
            failed += 1;
            continue;
        }
        completed += 1;
    }

    tracing::info!(
        corpus = %corpus_id,
        completed,
        failed,
        "tiered enrichment (folder): per-doc dispatch finished"
    );

    // Cross-source synthesis hook. For the conv provider this is the
    // default no-op; for the folder provider (vault corpora) it runs
    // the vault-wide RAPTOR pass that produces `vault_themes` rows
    // for the cross-note briefing block. Errors here are logged but
    // do not bubble up — the per-source enrichment is the
    // load-bearing output, finalization is a briefing-only
    // enhancement (synthesis briefing degrades to per-source-only
    // gracefully on empty `vault_themes`).
    if let Err(e) = provider.finalize_corpus(&corpus_id).await {
        tracing::warn!(
            corpus = %corpus_id,
            error = %e,
            "tiered enrichment (folder): finalize_corpus failed; continuing without cross-source synthesis"
        );
    }

    // Run-level terminal stamp — the ONE authoritative "this corpus's
    // folder enrichment finished" signal. The per-document provider
    // deliberately stamps only NON-terminal progress on the shared
    // per-corpus `_enrichment_state.json` (a single note completing or
    // failing must not flip the whole corpus to terminal — that was the
    // "went silent after one note" bug). So the runner, which alone
    // knows the full per-document loop settled, owns Complete/Failed.
    //
    // Stamped BEFORE `post_finalize_corpus` so the user-facing "map
    // ready" signal fires the moment the load-bearing work (per-note
    // enrichment + cross-note synthesis) is done — it must not wait on
    // best-effort/bench-side passes.
    stamp_folder_terminal(index_path, &corpus_id, completed, failed);

    // Post-terminal, best-effort work (folder provider: the bench-side
    // typed-extension pass). Runs AFTER Complete is stamped so it never
    // gates the banner; the provider spawns it detached, so this returns
    // promptly and the corpus stays `Complete` regardless of its outcome.
    provider.post_finalize_corpus(&corpus_id).await;

    Ok(plan)
}

/// Write the single run-level terminal enrichment stamp for a folder
/// build. `Complete` normally; `Failed` only when nothing enriched and
/// at least one document failed (a total wipeout — e.g. inference down
/// for the whole run). A partial run (some succeeded, some failed) is
/// `Complete` with the failure count in the message, because the
/// successful notes' skeletons are real, usable retrieval surface and
/// the per-note failures are recorded in `conv_skeletons.state`.
fn stamp_folder_terminal(index_path: &Path, corpus_id: &str, completed: usize, failed: usize) {
    let result = if completed == 0 && failed > 0 {
        EnrichmentStateFile::fail(
            index_path,
            corpus_id,
            &format!("all {failed} documents failed to enrich"),
        )
    } else {
        let message = if failed > 0 {
            format!("enriched {completed} notes ({failed} failed)")
        } else {
            format!("enriched {completed} notes")
        };
        EnrichmentStateFile::stamp(
            index_path,
            corpus_id,
            Some("folder_tiered"),
            EnrichmentPhase::Complete,
            completed as u64,
            (completed + failed) as u64,
            Some(&message),
        )
    };
    if let Err(e) = result {
        tracing::warn!(
            corpus = %corpus_id,
            error = %e,
            "tiered enrichment (folder): terminal state stamp failed; UI may not see completion"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_boundaries_match_spec() {
        assert_eq!(ConvBucket::classify(0), ConvBucket::Tiny);
        assert_eq!(ConvBucket::classify(7), ConvBucket::Tiny);
        assert_eq!(ConvBucket::classify(8), ConvBucket::Small);
        assert_eq!(ConvBucket::classify(30), ConvBucket::Small);
        assert_eq!(ConvBucket::classify(31), ConvBucket::Medium);
        assert_eq!(ConvBucket::classify(100), ConvBucket::Medium);
        assert_eq!(ConvBucket::classify(101), ConvBucket::Large);
        assert_eq!(ConvBucket::classify(300), ConvBucket::Large);
        assert_eq!(ConvBucket::classify(301), ConvBucket::LongTail);
        assert_eq!(ConvBucket::classify(510), ConvBucket::LongTail);
    }

    #[test]
    fn note_bucket_boundaries_only_degenerate_files_are_tiny() {
        // Per-file units: a 2-chunk note already carries a complete
        // argument — only 0/1-chunk files take the synthetic-node path.
        // Pinned because the conversation thresholds tiny-bucketed 23
        // of 46 live-vault notes (5 of 10 golden essays) and silently
        // exempted them from typed extraction (2026-06-11).
        assert_eq!(ConvBucket::classify_note(0), ConvBucket::Tiny);
        assert_eq!(ConvBucket::classify_note(1), ConvBucket::Tiny);
        assert_eq!(ConvBucket::classify_note(2), ConvBucket::Small);
        assert_eq!(ConvBucket::classify_note(7), ConvBucket::Small);
        assert_eq!(ConvBucket::classify_note(30), ConvBucket::Small);
        assert_eq!(ConvBucket::classify_note(31), ConvBucket::Medium);
        // Upper buckets match `classify` exactly.
        assert_eq!(ConvBucket::classify_note(101), ConvBucket::Large);
        assert_eq!(ConvBucket::classify_note(301), ConvBucket::LongTail);
    }

    #[test]
    fn bucket_labels_are_stable() {
        assert_eq!(ConvBucket::Tiny.label(), "tiny");
        assert_eq!(ConvBucket::Small.label(), "small");
        assert_eq!(ConvBucket::Medium.label(), "medium");
        assert_eq!(ConvBucket::Large.label(), "large");
        assert_eq!(ConvBucket::LongTail.label(), "long_tail");
    }

    #[test]
    fn folder_terminal_all_success_is_complete() {
        let tmp = tempfile::tempdir().unwrap();
        stamp_folder_terminal(tmp.path(), "vault", 314, 0);
        let s = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert_eq!(s.phase, EnrichmentPhase::Complete);
        assert!(s.completed_at.is_some());
        assert_eq!(s.step_current, 314);
        assert_eq!(s.step_total, 314);
        assert!(s.error.is_none());
    }

    #[test]
    fn folder_terminal_partial_failure_is_still_complete() {
        // Some notes failed but others produced real, usable skeletons —
        // the run is Complete (per-note failures live in conv_skeletons),
        // and the message surfaces the failure count for the operator.
        let tmp = tempfile::tempdir().unwrap();
        stamp_folder_terminal(tmp.path(), "vault", 300, 14);
        let s = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert_eq!(s.phase, EnrichmentPhase::Complete);
        assert_eq!(s.step_current, 300);
        assert_eq!(s.step_total, 314);
        assert!(s.message.as_deref().unwrap().contains("14 failed"));
    }

    #[test]
    fn folder_terminal_total_wipeout_is_failed() {
        // Nothing enriched and at least one failure → the whole run
        // failed (e.g. inference down). This is the ONLY path that
        // stamps a terminal Failed; a single bad note among successes
        // must never reach here.
        let tmp = tempfile::tempdir().unwrap();
        stamp_folder_terminal(tmp.path(), "vault", 0, 12);
        let s = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
        assert_eq!(s.phase, EnrichmentPhase::Failed);
        assert!(s.error.is_some());
        assert!(s.completed_at.is_none());
    }
}
