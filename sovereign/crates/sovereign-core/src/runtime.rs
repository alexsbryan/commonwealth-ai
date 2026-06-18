// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;


use crate::context::{build_context, format_history_as_prompt};
use crate::error::Result;
use crate::executor::{Executor, TaskContext};
use crate::memory;
use crate::query_session::{SessionStore, SharedSessionStore};
use crate::registry::ToolRegistry;
use crate::skills::{SkillRegister, SkillRegistry};
use crate::traits::{
    ApprovalChannel, InferenceProvider, NoOpRoutingEventSink, Planner, Router, RoutingEventSink,
    StateStore,
};
use crate::types::*;

/// Hard ceiling on the size of a single user turn's message.
///
/// ~16k chars ≈ 4k tokens. Keeps every downstream Fast-slot call
/// (working-memory compression, topic-context extraction, router
/// classification, query embedding) safely under typical 8k-token
/// context even when combined with conversation history + system
/// prompts. A 20-page document pasted as a message body is
/// ~40k tokens — it used to hang the pipeline for minutes before
/// this guard; now it errors cleanly and the user sees a hint to
/// use the document-attach flow instead.
///
/// Document-sized inputs belong in the `[Document attached: ...]`
/// prefix path, which routes through map-reduce and scales to
/// arbitrary length.
pub const MAX_TURN_MESSAGE_CHARS: usize = 16_000;

/// Error text shown when a message exceeds `MAX_TURN_MESSAGE_CHARS`.
/// Surfaced unchanged to the user via the Tauri command layer, so it
/// needs to be action-guidance, not a stack trace.
pub(crate) const OVERSIZE_MESSAGE_HINT: &str =
    "This message is too long for the chat pipeline (over 16,000 characters). \
     For document-sized content, attach it as a file instead — Sovereign \
     routes attachments through a map-reduce pipeline designed for long \
     inputs. Or summarise your question into a paragraph or two.";

pub use self::voice_prompts::{
    __voice_test_epistemic_contract_for, __voice_test_factual_base_prompt,
    __voice_test_relational_base_prompt, __voice_test_relational_expressive_prompt,
    __voice_test_render_temporal_tensions,
};
pub(crate) use self::voice_prompts::{
    build_witness_grounding, epistemic_contract_for, render_temporal_tensions,
    RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT,
};

mod voice_prompts;
pub(crate) use self::text_utils::{
    audit_pipeline_stage, format_conversation_history, now, today_anchor_block, truncate_chars,
    truncate_with_ellipsis,
};

mod text_utils;

// Prompt constants/builders + retrieval budgets + refusal detection —
// the pure policy layer, decomposed 2026-06-10 (see prompts.rs).
mod prompts;
pub(crate) use self::prompts::*;

pub(crate) use self::collaboration::{
    emit_ask_deliberation_chip, run_collaboration, run_post_stream_refinement, ContradictionCheck,
    ASK_MOVE_DELIBERATION_LINGER_MS,
};
pub use self::evidence::build_test_evidence_shape;
pub(crate) use self::evidence::{
    compute_evidence_shape, decide_expansion_strategy, is_grounding_candidate, operation_of,
    resolve_synthesis_route, EvidenceShape, ExpansionStrategy, SynthesisRoute,
    EVIDENCE_MIN_TOKEN_COVERAGE,
};
pub(crate) use self::intent_helpers::{
    build_clarification_question, default_oicp_for_intent, format_interpretation, intent_hint,
    label_for_intent, parse_intent_hint,
};
pub(crate) use self::question_analysis::{
    cap_chunks_per_article, comparison_axis, extract_commitment_phrase,
    extract_comparison_entities, extract_question_entities, parse_metalingual_locator,
    project_retrieved_chunks, raptor_late_inject_enabled, reserve_atom_enum_chunks,
    reserve_raptor_chunks, reserve_chunks_per_entity, MetalingualLocator,
};
pub(crate) use self::retrieval_helpers::{
    atlas_grounding_enabled, blend_query_aware, build_per_corpus_k_overrides, build_retrieval_query,
    collect_hot_corpora, cross_corpus_sort_cmp, drop_no_overlap_chunks, inject_meta_atlas_hits,
    reweight_by_query_relevance,
};
pub use self::types::{
    ContradictionProv, HistoryEntryProv, HistorySummaryProv, MetaAtlasHitRecord,
    RecalledMemoryProv, StreamHandle, TurnProvenance,
};
pub(crate) use self::types::{KnowledgeContext, KnowledgeQueryPlan};

pub(crate) use self::formatters::{
    build_coverage_gaps_note, build_provenance_components, format_scored_chunks,
    format_scored_chunks_with_kinds, MAX_KNOWLEDGE_CHARS,
};

mod collaboration;
mod evidence;
mod evidence_loop;
mod grounding;
// The gold-free value-presence primitive — shared by the gate (decides) and the
// chaos scorer (measures `blatant_confab_rate`). One implementation, one notion
// of "is this asserted value grounded," reachable as
// `sovereign_core::runtime::{assess_asserted_value, AssertedValue}`.
pub use grounding::{assess_asserted_value, AssertedValue};
mod formatters;
mod handlers;
mod intent_helpers;
mod numeric_audit;
mod prompt_budget;
mod question_analysis;
mod retrieval;
mod retrieval_helpers;
/// Public (doc-hidden) so the integration-test harness can drive the
/// runner with mocked steps against a real `Runtime` — the in-crate
/// unit-test route is blocked by the sovereign-store circular dev-dep
/// (two `sovereign_core` identities). Not a supported external API.
#[doc(hidden)]
pub mod retrieval_pipeline;
mod streaming;
mod system_message;
mod turn;
mod types;

pub(crate) use self::retrieval_pipeline::{deep_pipeline, kq_pipeline, PipelineState};

pub struct Runtime {
    pub inference: Arc<dyn InferenceProvider>,
    pub router: Box<dyn Router>,
    pub planner: Box<dyn Planner>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub skills: Arc<SkillRegistry>,
    pub approval: Arc<dyn ApprovalChannel>,
    pub inference_config: InferenceConfig,
    /// Per-conversation record of the last turn's REAL assembled
    /// prompt sizes, written by the prompt-budget guard at the two
    /// request-construction sites. Phase 2 of the budget-sensor
    /// redesign: `estimate_compaction_pressure` uses it as a floor
    /// (its component estimate sees ~⅓ of the prompt), and the
    /// Phase-3 allocator derives next-turn knowledge/history budgets
    /// from it. Bounded (cleared past 512 conversations); never
    /// persisted — a fresh process re-learns within one turn.
    pub(crate) assembly_memo:
        std::sync::RwLock<std::collections::HashMap<String, prompt_budget::MeasuredAssembly>>,
    pub corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
    /// Optional structural link graph for a corpus that exposes one
    /// (today: Wikipedia, via metadata `outgoing_links` /
    /// `pov_count` / `section_path`). Populated by the bootstrap
    /// when a `wikipedia_graph.db` is found alongside the corpus's
    /// LanceDB table. When present, the retrieval path can opt into
    /// one-hop neighbor expansion (env-gated) and surfaces
    /// `(contested)` markers on chunks whose source has at least
    /// one editor-flagged contested section. `None` preserves the
    /// pre-graph behaviour.
    pub wikipedia_graph: Option<Arc<corpus_engine::WikipediaGraph>>,
    /// Optional note store. Populated by the daemon bootstrap; absent
    /// in the chat-CLI path where commitment persistence isn't wired.
    /// Consumed by `handle_commissive_query` to write `kind="commitment"`
    /// and `kind="todo"` notes anchored to `working_memory.current_goal`
    /// (or honestly anchorless when no situated goal is loaded).
    pub note_store: Option<Arc<corpus_engine_notes::NoteStore>>,
    /// Optional rolling-summary compaction worker. When present,
    /// `end_conversation` notifies it after writing extracted
    /// memories so a conversation that crossed the threshold gets
    /// its oldest memories folded into a `MemoryKind::Summary` in
    /// the background. `None` preserves the pre-2026-05-23
    /// uncompacted behaviour exactly.
    pub compaction: Option<Arc<crate::memory_compaction::CompactionWorker>>,
    /// Read-side handle for conversation tiered-retrieval enrichment
    /// (`conv_skeletons` / `conv_raptor_nodes` / `conv_motifs`). Spec
    /// `sovereign/docs/specs/CONV_TIERED_PORT.md`. When present, the
    /// prompt-assembly path renders per-conversation briefings ahead
    /// of the raw chunk block via
    /// [`crate::conv_briefing::build_conv_tiered_briefings`].
    /// `None` preserves the pre-tiered behaviour exactly — the model
    /// gets only the standard `format_scored_chunks_with_kinds`
    /// output for conv corpora.
    pub conv_tiered_reader: Option<Arc<dyn crate::conv_tiered::ConvTieredReader>>,
    /// Optional mesh-knowledge client. Populated by the desktop
    /// bootstrap when an `EmbeddedDaemon` is running — the Runtime
    /// fans out knowledge queries through its local Commonwealth
    /// daemon at `127.0.0.1:9741/v1/knowledge/search`, which then
    /// searches local + peer corpora. `None` means "no mesh" — the
    /// standalone (pre-mesh) behavior is preserved exactly.
    pub mesh_knowledge: Option<Arc<dyn crate::traits::MeshKnowledgeSource>>,
    /// Optional [`LandscapeDigestProvider`][crate::traits::LandscapeDigestProvider]
    /// (typically the `sovereign-tools` `KnowledgeViewManager`). When
    /// present, Runtime calls it after routing to splice
    /// `knowledge_view_digests` onto the `ConversationContext` — the
    /// landscape-of-terrain summary consumed by the prompt assembly
    /// layer. `None` = pre-KnowledgeView behaviour preserved exactly;
    /// digests stay `None` and the context carries only memories and
    /// corpus chunks.
    pub landscape_digests: Option<Arc<dyn crate::traits::LandscapeDigestProvider>>,
    /// In-memory per-turn scratch store for antifragile routing. Holds
    /// the `RouterClassification` + `RoutingPolicy` + cancellation
    /// token for the in-flight turn; PR2 will also cache retrieval
    /// and partial response so redirects can reuse work. Populated on
    /// every `classify` return; GC'd on next turn or after 30s.
    pub sessions: SharedSessionStore,
    /// Active confidence thresholds. Defaults (0.80 / 0.55) ship with
    /// every Runtime unless overridden by the host. PR4 will mutate
    /// this from structural-signal calibration; PR1 reads it verbatim.
    pub confidence_thresholds: ConfidenceThresholds,
    /// Sink for the three antifragile-routing UI events
    /// (interpretation-proposed, clarification-request, turn-narration).
    /// Desktop bootstrap injects a `TauriRoutingEventSink`; headless
    /// test/CLI harnesses get the default `NoOpRoutingEventSink`.
    pub routing_events: Arc<dyn RoutingEventSink>,
    /// Source of pre-embedded atlas Entity contexts, looked up at
    /// query time and fused into chunk-retrieval results as virtual
    /// `ScoredChunk`s. The daemon's `AtlasContextManager` populates
    /// this once at boot per installed corpus that has an `atlas/`
    /// dir. `None` = atlas-grounded retrieval is off (the pre-atlas
    /// chunk-only behaviour is preserved exactly).
    pub atlas_context_provider: Option<Arc<dyn crate::atlas_context::AtlasContextProvider>>,
    /// Reports which `corpus_id`s are flagged sensitive (e.g.
    /// folder-ingest v1 §3.4 watched-folder sensitivity). Consulted
    /// by [`Runtime::search_corpus_indexes`] before fanning out
    /// retrieval — sensitive corpora are dropped from the
    /// ambient-retrieval candidate set so they never contribute to
    /// pre-turn situated context.
    ///
    /// `None` = no sensitivity gate applied (all corpora eligible),
    /// which matches the pre-v1 behaviour exactly. The bootstrap
    /// wires sovereign-tools' `LocalCorpusManager` here.
    pub sensitive_corpora: Option<Arc<dyn crate::traits::SensitiveCorpusOracle>>,
    /// Per-folder metadata oracle. Folder-ingest v1 §6.3 — when
    /// retrieval pulls chunks from a watched-folder corpus, this
    /// provides the user-typed display name and the "what I don't
    /// have" gap counters so the synthesis prompt can say "your
    /// case-files folder" and surface skipped/failed-file notes.
    /// `None` = no folder corpora known (CLI fallback / tests),
    /// which preserves the pre-Phase-F label rendering exactly.
    pub folder_metadata: Option<Arc<dyn crate::traits::FolderMetadataOracle>>,
    /// Optional cross-encoder reranker. When `Some`, every call to
    /// `search_corpus_indexes` (and its filtered companion) hits
    /// `CorpusIndex::search_with_rerank` instead of `search`; the
    /// hybrid result gets re-ordered by a model trained to score
    /// (query, doc) relevance directly. `None` preserves baseline
    /// fusion-only behaviour exactly.
    ///
    /// Bootstrapped from `SOVEREIGN_RERANK=1` (or wired explicitly
    /// by the daemon when models.toml carries a `[rerank]` slot).
    pub rerank_fn: Option<corpus_engine::RerankFn>,
    /// Configuration for the rerank pass — overfetch size, optional
    /// threshold. Always present; `enabled = false` makes
    /// `search_with_rerank` no-op back to baseline regardless of
    /// `rerank_fn`'s presence.
    pub rerank_config: corpus_engine::RerankConfig,
    /// Cross-corpus meta-atlas index (Move 5). Built at bootstrap
    /// from `~/.sovereign/meta-atlas/canonical_atoms.json` (produced
    /// by `sovereign meta-atlas build`). The chat-path boost pass
    /// `Self::meta_atlas_boost` consults the index on every
    /// knowledge-query turn to surface stream-tagged anchors per
    /// question entity. `None` (or empty index) = no boost; retrieval
    /// falls back to cosine + entity-boost search exactly as before.
    pub meta_atlas: Option<Arc<corpus_engine::meta_atlas::MetaAtlasIndex>>,
    /// Cross-corpus bridge edges (typed topic-to-topic alignment from
    /// `sovereign meta-atlas align`), consumed by [`Self::bridge_boost`].
    /// `None`/empty → no-op (retrieval behaves as before).
    pub bridge: Option<Arc<corpus_engine::meta_atlas::BridgeIndex>>,
    /// Per-conversation last-turn provenance snapshot, written at
    /// dispatch inside [`Self::handle_expressive_query_stream`] and
    /// read by [`Self::get_last_turn_provenance`]. Last-write-wins
    /// per `conversation_id`; not persisted across restarts.
    ///
    /// The desktop's inner-work surface pulls this via a Tauri
    /// command bound to Cmd+? to surface "what did the model
    /// actually see on the most recent witness turn." Capture is
    /// scoped to the streaming witness path because that's where
    /// the bad-response signal originates; if the non-streaming
    /// path needs the same surface later, mirror the capture in
    /// `handle_expressive_query`.
    pub turn_provenance: Arc<std::sync::RwLock<HashMap<String, TurnProvenance>>>,
    /// Optional GLiNER entity extractor. Wired by the CLI/daemon bootstrap
    /// when the gliner_small-v2.1 ONNX model is installed. Used by
    /// `maybe_retrieve_relevant_history` for entity-aware query
    /// enrichment + hybrid cosine/jaccard scoring. `None` = pre-GLiNER
    /// behaviour preserved (pure cosine + MMR).
    pub gliner: Option<Arc<dyn crate::traits::EntityExtractor>>,
}

impl Runtime {
    /// Resolve the active-mode skill id for a conversation.
    ///
    /// Single source of truth post-2026-05-24 architecture redesign:
    /// the conversation's `skill_id` column (set at create-time by
    /// the surface that owns it) drives routing. Registry state is
    /// no longer consulted for workspace skills — that was the
    /// brittle lifecycle-glue path where every surface enter/leave
    /// triggered `rebuild_runtime` (~15s) plus a race-prone
    /// activate/deactivate dance across mount/destroy hooks.
    ///
    /// Validation: the tag is silently dropped when (a) the skill
    /// id isn't registered (skill removed since the conversation
    /// was tagged), or (b) the skill exists but is `Background`
    /// kind (frontend bug — backgrounds aren't surface skills).
    /// Both fall through to default-chat routing rather than
    /// crashing.
    ///
    /// Returns `None` for untagged conversations (default chat).
    pub(crate) async fn resolve_active_mode(&self, conversation_id: &str) -> Option<String> {
        let conv = self.store.get_conversation(conversation_id).await.ok()?;
        let tag = conv.skill_id?;
        let skill = self.skills.skill_by_id(&tag)?;
        if skill.activation_kind != crate::skills::ActivationKind::Workspace {
            tracing::debug!(
                conversation_id,
                skill_id = %tag,
                "resolve_active_mode: conversation tagged with non-workspace \
                 skill; falling through to default routing"
            );
            return None;
        }
        Some(tag)
    }

    /// Record a turn's measured assembly for this conversation —
    /// Phase 2 of the budget-sensor redesign. Bounded: clears the map
    /// past 512 conversations (per-process working set is far below
    /// this; the memo re-learns within one turn).
    pub(crate) fn record_assembly(
        &self,
        conversation_id: &str,
        measured: prompt_budget::MeasuredAssembly,
    ) {
        let mut memo = self
            .assembly_memo
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if memo.len() >= 512 && !memo.contains_key(conversation_id) {
            memo.clear();
        }
        memo.insert(conversation_id.to_string(), measured);
    }

    /// Last turn's real assembled sizes for this conversation, if the
    /// budget guard has run on it this process lifetime.
    pub(crate) fn last_assembly(
        &self,
        conversation_id: &str,
    ) -> Option<prompt_budget::MeasuredAssembly> {
        self.assembly_memo
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(conversation_id)
            .copied()
    }

    /// Phase-3 allocation for this conversation's NEXT assembly,
    /// derived from the previous turn's measured demand.
    pub(crate) fn allocation_for(&self, conversation_id: &str) -> prompt_budget::Allocation {
        prompt_budget::allocate(self.last_assembly(conversation_id).as_ref())
    }

    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        router: Box<dyn Router>,
        planner: Box<dyn Planner>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
        skills: Arc<SkillRegistry>,
        approval: Arc<dyn ApprovalChannel>,
        inference_config: InferenceConfig,
    ) -> Self {
        Self {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
            approval,
            inference_config,
            corpus_engine: None,
            wikipedia_graph: None,
            note_store: None,
            assembly_memo: std::sync::RwLock::new(std::collections::HashMap::new()),
            compaction: None,
            conv_tiered_reader: None,
            mesh_knowledge: None,
            landscape_digests: None,
            sessions: Arc::new(SessionStore::new()),
            confidence_thresholds: ConfidenceThresholds::default(),
            routing_events: Arc::new(NoOpRoutingEventSink),
            atlas_context_provider: None,
            sensitive_corpora: None,
            folder_metadata: None,
            rerank_fn: None,
            rerank_config: corpus_engine::RerankConfig::default(),
            meta_atlas: None,
            bridge: None,
            turn_provenance: Arc::new(std::sync::RwLock::new(HashMap::new())),
            gliner: None,
        }
    }

    /// Install a GLiNER entity extractor for entity-aware retrieval
    /// over conversation history. Used by
    /// `maybe_retrieve_relevant_history` to compute a hybrid
    /// cosine/jaccard score: 0.6·cosine(query, pair) +
    /// 0.4·jaccard(query_entities, pair_entities). When `None`,
    /// retrieval falls back to pure cosine + MMR (pre-GLiNER
    /// behaviour preserved).
    pub fn with_gliner(mut self, gliner: Arc<dyn crate::traits::EntityExtractor>) -> Self {
        self.gliner = Some(gliner);
        self
    }

    /// Install a cross-encoder reranker. Pure-additive: when enabled,
    /// every corpus search overfetches `config.candidates_k` candidates
    /// from the hybrid fusion path, scores them with `fn`, sorts by
    /// rerank score, and truncates to the caller's limit. When `fn`
    /// errors at runtime, the search-side fallback preserves baseline
    /// fusion ordering — enabling the reranker can never make retrieval
    /// worse than without it.
    pub fn with_rerank(
        mut self,
        rerank_fn: corpus_engine::RerankFn,
        config: corpus_engine::RerankConfig,
    ) -> Self {
        self.rerank_fn = Some(rerank_fn);
        self.rerank_config = config;
        self
    }

    /// Install rerank *config* without a reranker function. Used by
    /// the per-article-dedup-only ablation: overfetch + dedup using
    /// fusion scores only, no cross-encoder calls. Validates whether
    /// the SEP source-recall lift attributed to the reranker
    /// experiment is actually driven by dedup or by the
    /// cross-encoder logits.
    pub fn with_rerank_config(mut self, config: corpus_engine::RerankConfig) -> Self {
        self.rerank_fn = None;
        self.rerank_config = config;
        self
    }

    /// Fetch the most recent witness-turn provenance for `conversation_id`,
    /// if any. Returns `None` when no provenance has been captured for
    /// that conversation in this Runtime's lifetime (e.g. a fresh
    /// daemon, a non-relational classification, or a conversation that
    /// only ran on the non-streaming witness path).
    pub fn get_last_turn_provenance(&self, conversation_id: &str) -> Option<TurnProvenance> {
        let guard = self.turn_provenance.read().ok()?;
        guard.get(conversation_id).cloned()
    }

    /// Test-only knob: replace the default `SessionStore` so a
    /// suite can drive the runtime with a relaxed narration gate
    /// (e.g. `Duration::ZERO` so an instant stubbed turn still
    /// emits its `NarrationPhase` events). Production callers
    /// inherit the `NARRATION_MIN_ELAPSED` const default from
    /// [`SessionStore::new`].
    pub fn with_session_store(mut self, sessions: SharedSessionStore) -> Self {
        self.sessions = sessions;
        self
    }

    /// Install a `RoutingEventSink` to receive interpretation,
    /// clarification, and narration events. The desktop bootstrap
    /// calls this with a `TauriRoutingEventSink`; headless harnesses
    /// inherit the `NoOpRoutingEventSink` default from `new`.
    pub fn with_routing_events(mut self, sink: Arc<dyn RoutingEventSink>) -> Self {
        self.routing_events = sink;
        self
    }

    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
    }

    /// Install the cross-corpus meta-atlas index. Built by the
    /// bootstrap by loading `~/.sovereign/meta-atlas/canonical_atoms.json`
    /// (produced by `sovereign meta-atlas build`). Optional — when
    /// `None`, [`Self::meta_atlas_boost`] short-circuits and retrieval
    /// behaves exactly as before the meta-atlas substrate landed.
    pub fn with_meta_atlas(
        mut self,
        index: Arc<corpus_engine::meta_atlas::MetaAtlasIndex>,
    ) -> Self {
        self.meta_atlas = Some(index);
        self
    }

    /// Install the cross-corpus bridge index (typed topic-to-topic edges
    /// from `sovereign meta-atlas align`). Optional — `None` short-
    /// circuits [`Self::bridge_boost`] and retrieval is unchanged.
    pub fn with_bridge(
        mut self,
        index: Arc<corpus_engine::meta_atlas::BridgeIndex>,
    ) -> Self {
        self.bridge = Some(index);
        self
    }

    /// Install a source of pre-embedded atlas Entity contexts.
    /// Usually `sovereign-tools::AtlasContextManager` constructed by
    /// the daemon bootstrap; the eval CLI builds inline contexts and
    /// can call this with a one-shot provider for symmetry.
    pub fn with_atlas_context_provider(
        mut self,
        provider: Arc<dyn crate::atlas_context::AtlasContextProvider>,
    ) -> Self {
        self.atlas_context_provider = Some(provider);
        self
    }

    /// Install a structural link graph. The bootstrap does this
    /// when a graph DB is found alongside a corpus's LanceDB table;
    /// callers that don't wire one (e.g. tests, code-corpus chat)
    /// leave it `None` and retrieval behaves exactly as before.
    pub fn with_wikipedia_graph(mut self, graph: Arc<corpus_engine::WikipediaGraph>) -> Self {
        self.wikipedia_graph = Some(graph);
        self
    }

    /// Install a note store for commitment persistence. Daemon bootstrap
    /// wires this; CLI eval path leaves it `None`, in which case the
    /// commissive handler degrades to a clear "no notes store wired"
    /// reply rather than dropping the commitment silently.
    pub fn with_note_store(mut self, store: Arc<corpus_engine_notes::NoteStore>) -> Self {
        self.note_store = Some(store);
        self
    }

    /// Install the rolling-summary compaction worker. The daemon
    /// bootstrap constructs the worker via
    /// [`crate::memory_compaction::CompactionWorker::spawn`] (which
    /// starts the background drain task) and hands the resulting
    /// `Arc` here. The CLI eval path leaves `None`; `end_conversation`
    /// then skips the enqueue and the pre-compaction shape is
    /// preserved exactly.
    pub fn with_compaction(
        mut self,
        worker: Arc<crate::memory_compaction::CompactionWorker>,
    ) -> Self {
        self.compaction = Some(worker);
        self
    }

    /// Install the conversation tiered-retrieval reader so the
    /// prompt-assembly path surfaces per-conv briefings + signposts
    /// alongside the raw chunk block. The daemon wires this with the
    /// same `Arc<SqliteStateStore>` it hands to the
    /// `ConvTieredProvider` writer — one store, two views.
    pub fn with_conv_tiered_reader(
        mut self,
        reader: Arc<dyn crate::conv_tiered::ConvTieredReader>,
    ) -> Self {
        self.conv_tiered_reader = Some(reader);
        self
    }

    /// Install a `KnowledgeView` landscape-digest provider. Typically
    /// the `sovereign-tools::knowledge_view::KnowledgeViewManager`,
    /// constructed alongside the `StateStore` so the same `Arc` can
    /// also be passed as a `StateStoreObserver`.
    ///
    /// Opt-in: leaving this `None` preserves the pre-KnowledgeView
    /// behaviour exactly. Test harnesses that don't wire KnowledgeView
    /// inherit the no-op.
    pub fn with_landscape_digests(
        mut self,
        provider: Arc<dyn crate::traits::LandscapeDigestProvider>,
    ) -> Self {
        self.landscape_digests = Some(provider);
        self
    }

    /// Install a sensitive-corpus oracle (folder-ingest v1 §3.4).
    /// When wired, [`Runtime::search_corpus_indexes`] consults the
    /// oracle for each ambient retrieval and drops any corpus the
    /// oracle reports as sensitive *before* fanning out the search.
    /// Leaving this `None` preserves the pre-v1 behaviour exactly
    /// (no corpus is treated as sensitive).
    ///
    /// Per ARCH §7.4 (defence in depth), this is the runtime-side
    /// layer of enforcement — sovereign-tools' `WatchedFolderConfig`
    /// holds the flag, the on-disk state mirrors it, and the
    /// runtime applies the structural exclusion at the assembly
    /// seam. A failure at any single layer doesn't compromise the
    /// invariant because the other layers still apply.
    pub fn with_sensitive_corpora(
        mut self,
        oracle: Arc<dyn crate::traits::SensitiveCorpusOracle>,
    ) -> Self {
        self.sensitive_corpora = Some(oracle);
        self
    }

    /// Install the per-folder metadata oracle (Folder-ingest v1
    /// §6.3 source attribution + coverage). The runtime uses the
    /// snapshot to (a) replace `corpus_id`-as-label with the user's
    /// typed display name in the prompt's `[Source: …]` headers
    /// and (b) surface a "what I don't have" line when matched
    /// folders carry many failed/skipped files.
    ///
    /// `None` (the default) preserves the pre-Phase-F behaviour
    /// exactly, so test harnesses and the bare CLI path don't have
    /// to wire sovereign-tools' `LocalCorpusManager` to keep
    /// running.
    pub fn with_folder_metadata(
        mut self,
        oracle: Arc<dyn crate::traits::FolderMetadataOracle>,
    ) -> Self {
        self.folder_metadata = Some(oracle);
        self
    }

    /// Install a mesh-knowledge client. Only called when the desktop
    /// has an `EmbeddedDaemon` actually running — tests and the
    /// bare CLI path leave this `None`, in which case
    /// `prepare_knowledge_context` behaves exactly as before
    /// (local-only search, `search_method = "LocalOnly"`).
    pub fn with_mesh_knowledge(
        mut self,
        mesh: Arc<dyn crate::traits::MeshKnowledgeSource>,
    ) -> Self {
        self.mesh_knowledge = Some(mesh);
        self
    }

    /// Spawn a background task that generates an auto-title for the
    /// conversation if one isn't already set. Non-blocking — failures are
    /// logged and do not affect the caller.
    ///
    /// `try_auto_title` is idempotent: safe to call after every assistant
    /// message save. It exits early when the title is already set or the
    /// conversation doesn't have enough messages yet.
    fn spawn_auto_title(&self, conversation_id: &str) {
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let cid = conversation_id.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                crate::title::try_auto_title(inference.as_ref(), store.as_ref(), &cid).await
            {
                tracing::warn!(
                    conversation_id = %cid,
                    error = %e,
                    "auto-title: generation failed"
                );
            }
        });
    }

    /// Extract long-term memories from a conversation and save them.
    /// Call this when a conversation ends (user quits or session ends).
    pub async fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        let context = build_context(self.store.as_ref(), conversation_id, "").await?;
        if context.conversation.messages.len() < 4 {
            return Ok(());
        }

        let memory_rules = self.skills.memory_rules();
        let extracted = memory::extract_long_term_memories(
            self.inference.as_ref(),
            &context.conversation.messages,
            &memory_rules,
        )
        .await?;

        tracing::info!(
            count = extracted.len(),
            "memory: extracted long-term memories"
        );
        // Read the conversation's skill_id once before the loop. The
        // tag is denormalized onto each extracted memory so the
        // recall layer can wall scoped pools (e.g. inner-work) at the
        // SQL level without a join. `None` here means "general pool"
        // — the conversation predates the skill-tagging migration or
        // ran outside any skill.
        let source_skill_id = context.conversation.skill_id.clone();
        for mut mem in extracted {
            // Tag each extracted memory with the conversation it
            // came from. Enables the `personal-knowledge`
            // KnowledgeView to surface cluster membership
            // alongside conversation-level metadata (title, skill)
            // at digest time, and makes `memories.source_conversation_id`
            // no longer NULL on fresh writes post-migration.
            mem.source_conversation_id = Some(conversation_id.to_string());
            mem.source_skill_id = source_skill_id.clone();
            memory::save_with_contradiction_check(
                self.inference.as_ref(),
                self.store.as_ref(),
                mem,
            )
            .await?;
        }

        // Save-time hook for rolling-summary compaction. Fire-and-
        // forget — the worker re-checks the threshold before doing
        // real work, so over-enqueuing is harmless. Pre-2026-05-23
        // path (no worker wired) skips the notification.
        if let Some(worker) = &self.compaction {
            worker.maybe_enqueue(conversation_id);
        }

        // Pull a fresh entity inventory from the LandscapeDigestProvider
        // (typically `sovereign-tools::KnowledgeViewManager`). When
        // present, memories that mention any canonical entity name
        // decay at half rate (Phase 7 — relationship-weighted decay).
        // `None` = uniform decay, identical to the pre-Phase-7 path.
        let inventory = match self.landscape_digests.as_ref() {
            Some(p) => p.entity_inventory().await,
            None => None,
        };
        let pruned = memory::prune_decayed_memories_with_config(
            self.store.as_ref(),
            now(),
            memory::DEFAULT_DECAY_RATE,
            memory::DEFAULT_PRUNE_THRESHOLD,
            inventory.as_ref(),
        )
        .await
        .unwrap_or(0);
        if pruned > 0 {
            tracing::info!(pruned, "memory: pruned decayed memories");
        }

        Ok(())
    }


    // Turn dispatch lives in sibling files (decomposed 2026-06-10,
    // same impl-Runtime-across-files pattern as handlers/):
    //   streaming.rs — handle_message_stream + resume/redirect entry points
    //   turn.rs      — handle_message/handle_turn + seed + stream-drain
}

pub(crate) use self::attached_doc_render::{
    parse_tool_call_inline, render_attached_doc_conversation, truncate_for_chip, AttachedDocSegment,
};

mod attached_doc_render;

#[cfg(test)]
mod relational_intent_override_tests {
    use super::*;

    #[test]
    fn non_relational_register_is_passthrough() {
        let intent = Intent::MetalingualQuery;
        let out =
            crate::intent_policy::apply_witness_intent_override(&intent, SkillRegister::Factual);
        assert!(matches!(out, Intent::MetalingualQuery));
    }

    #[test]
    fn relational_overrides_metalingual_to_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::MetalingualQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_overrides_knowledge_to_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::KnowledgeQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_overrides_complex_task_to_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::ComplexTask,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_preserves_expressive() {
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::ExpressiveQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_preserves_deep_query() {
        // DeepQuery + Relational rides handle_simple's witness branch
        // and benefits from extended-thinking budget; don't downgrade.
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::DeepQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::DeepQuery));
    }

    #[test]
    fn relational_preserves_continuation() {
        // Continuation routes from the prior turn's rebound intent;
        // overriding here would mask the actual continuation context.
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::Continuation {
                task_id: "t-1".into(),
            },
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::Continuation { .. }));
    }
}
