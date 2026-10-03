// SPDX-License-Identifier: AGPL-3.0-or-later
//! The inbound tiered-enrichment port (phase-b-49, FIVE_PROGRAMS §2c). The
//! engine declares and dispatches these hooks and svrn implements them over
//! its own conversation store, so the traits and the values their methods
//! name are spoken by both programs and live here beside the other ingest
//! ports. `corpus_engine::enrichment::tiered` re-exports every item at its
//! historical path.

use std::path::Path;
use std::sync::Arc;

use crate::index::EnrichmentChunkRow;
use crate::Result;

/// Shared handle to a `TieredEnrichmentProvider` impl. `Arc<dyn>` so
/// the daemon can pass one instance through `CorpusEngine` without
/// taking ownership.
pub type TieredProviderHandle = Arc<dyn TieredEnrichmentProvider>;

/// Shared handle for the per-chunk NER extractor (GliNER today, via
/// `sovereign-gliner`). Optional second hook fired by the
/// tiered runner ahead of the heavy `TieredEnrichmentProvider` call
/// — runs the cheap CPU-only NER pass first so the chunk_entities
/// table populates even when the LLM-side enrichment fails or is
/// killed mid-run. `None` falls back to RAPTOR-derived entities only.
pub type ChunkEntityExtractorHandle = Arc<dyn ChunkEntityExtractor>;

/// What one NER pass produced — and what it REFUSED.
///
/// The refusal count travels in the return type rather than in a log
/// line because a caller cannot drop a field it has to destructure
/// (ARCH 6, ARCH 10). An implementor that bounds its input (the GLiNER
/// one does — `ingest/crates/corpus-engine/src/enrichment/chunk_ner_bound.rs`) reports the
/// chunks it declined to send here; the runners accumulate it and stamp
/// it on `_enrichment_state.json` via
/// [`crate::enrichment_state::EnrichmentStateFile::record_refused_over_cap`], so "this corpus's
/// entities are thin because 412 chunks were too long for the model" is
/// a fact on disk instead of a guess.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkNerOutcome {
    /// Mentions persisted by this call.
    pub mentions: usize,
    /// Chunks NOT handed to inference because they exceeded the
    /// per-chunk input bound. Refused whole — never truncated.
    pub refused_over_cap: usize,
}

impl ChunkNerOutcome {
    /// The common case: everything fit.
    pub fn mentions(mentions: usize) -> Self {
        Self {
            mentions,
            refused_over_cap: 0,
        }
    }
}

/// Per-chunk named-entity extractor. corpus-engine declares the
/// trait so the dispatch loop can fire it per-conversation;
/// sovereign-tools owns the concrete impl (where the `gline-rs` dep
/// lives) and the SqliteStateStore persistence path.
///
/// One call per conversation: implementor batches chunks internally
/// for throughput, under whatever input bound its backend needs.
/// Returns the mentions persisted plus the chunks it refused, so the
/// runner can surface both.
#[async_trait::async_trait]
pub trait ChunkEntityExtractor: Send + Sync {
    async fn extract_for_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
    ) -> Result<ChunkNerOutcome>;

    /// Phase B incremental hook (spec
    /// `svrn/docs/specs/PROGRESSIVE_ENRICHMENT.md` §"Incremental
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
    ) -> Result<ChunkNerOutcome> {
        Ok(ChunkNerOutcome::default())
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
