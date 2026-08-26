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
/// needs to be action-guidance, not a stack trace. `pub` so the desktop
/// can recognise this specific case and present it as a calm assistant
/// turn (graceful guidance) rather than a raw "Error: Invalid input:"
/// bubble that reads as a crash.
pub const OVERSIZE_MESSAGE_HINT: &str =
    "This message is too long for the chat pipeline (over 16,000 characters). \
     For document-sized content, attach it as a file instead — Sovereign \
     routes attachments through a map-reduce pipeline designed for long \
     inputs. Or summarise your question into a paragraph or two.";

/// Graceful clarification shown when a turn carries no actual question — empty,
/// whitespace, or punctuation/symbols only (e.g. "?"). Without this the turn
/// routed into the generative path and produced a generic essay over nothing,
/// which the UX judge (rightly) scored as broken. Mirrors `OVERSIZE_MESSAGE_HINT`:
/// `pub` so the desktop recognises it and renders a calm assistant turn instead
/// of a raw error bubble. Brief, warm, points to a path forward.
pub const DEGENERATE_MESSAGE_HINT: &str =
    "I didn't catch a question there — what would you like to know? Ask about \
     anything in your knowledge bases (a fact, a summary, or how two ideas \
     connect) and I'll dig in.";

/// A turn message carries no question to answer: it has no alphanumeric
/// character at all (empty, whitespace, or punctuation/symbols only, like "?").
/// Unicode-aware, so a question in any script (CJK, etc.) is NOT degenerate —
/// only genuinely contentless input is. Guards the chat entry points alongside
/// the oversize check.
pub fn is_degenerate_message(message: &str) -> bool {
    !message.chars().any(|c| c.is_alphanumeric())
}

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

pub(crate) mod text_utils;

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
    resolve_output_budget, resolve_synthesis_route, EvidenceShape, ExpansionStrategy,
    SynthesisRoute, EVIDENCE_MIN_TOKEN_COVERAGE,
};
pub(crate) use self::intent_helpers::{
    build_clarification_question, default_oicp_for_intent, format_interpretation, intent_hint,
    label_for_intent, parse_intent_hint,
};
pub(crate) use self::merge_select::{
    concept_obligations_enabled, merge_demand_select, merge_select_enabled,
};
pub(crate) use self::question_analysis::{
    cap_chunks_per_article, comparison_axis, extract_commitment_phrase,
    extract_comparison_entities, extract_question_entities, locator_hint_from_coarse,
    parse_metalingual_locator, project_retrieved_chunks, raptor_late_inject_enabled,
    reserve_atom_enum_chunks, reserve_chunks_per_entity, reserve_raptor_chunks, MetalingualLocator,
    COARSE_CONVERSATION_ARCHIVE_EMBED, COARSE_CONVERSATION_LOCATOR_DIRECT,
    COARSE_CONVERSATION_LOCATOR_EMBED,
};
pub(crate) use self::retrieval_helpers::{
    apply_cross_corpus_discipline, atlas_grounding_enabled, blend_query_aware,
    build_per_corpus_k_overrides, build_retrieval_query, collect_hot_corpora,
    cross_corpus_sort_cmp, drop_no_overlap_chunks, inject_meta_atlas_hits,
    reweight_by_query_relevance,
};
pub use self::types::{
    ContradictionProv, EvidenceRetrieval, HistoryEntryProv, HistoryRecallProv, HistorySummaryProv,
    MetaAtlasHitRecord, RecalledMemoryProv, StreamHandle, TurnProvenance,
};
pub(crate) use self::types::{KnowledgeContext, KnowledgeQueryPlan};

pub(crate) use self::formatters::{
    build_coverage_gaps_note, build_provenance_components, format_scored_chunks,
    format_scored_chunks_counted, format_scored_chunks_with_kinds, MAX_KNOWLEDGE_CHARS,
};

pub mod acquisition;
mod code_trace;
mod collaboration;
pub mod epistemic;
mod evidence;
mod evidence_loop;
mod gk_rescue;
pub(crate) mod grounding;
// The gold-free value-presence primitive — shared by the gate (decides) and the
// chaos scorer (measures `blatant_confab_rate`). One implementation, one notion
// of "is this asserted value grounded," reachable as
// `sovereign_core::runtime::{assess_asserted_value, AssertedValue}`.
pub use grounding::{assess_asserted_value, AssertedValue};
// The shared decline-shape primitives — one notion of "this prose answers
// nothing", used by the gate's decline guard and the ledger-fidelity
// bench's verdict-vs-prose cross-check. `released_pure_decline` is the
// strict form (excludes caveated-GK pivots and decline-then-answer
// shapes); `answer_declines` is the loose contains-form the specifics
// guard uses.
pub use grounding::{answer_declines, released_pure_decline};
// The shared grounding-verifier threshold τ (SOVEREIGN_GV_THRESHOLD, else the
// bench-calibrated 0.9) — public so the chaos bench gates against the SAME
// default the production gate uses instead of re-deriving its own.
pub use grounding::grounding_gate_threshold;
// The register tau is calibrated ON, shared with the bench critic so the
// transfer argument is enforced by the compiler rather than by two matching
// string literals. See `grounding::judge::CHUNK_JUDGE_SYSTEM`.
pub use grounding::{chunk_judge_prompt, CHUNK_JUDGE_PASSAGE_CHARS, CHUNK_JUDGE_SYSTEM};
pub use grounding::{claim_extraction_prompt, CLAIM_EXTRACTION_SYSTEM};
// The gate's claim-extraction primitive — public so the Stream B corruption
// harness and `svrn bench verifier extract-claims` produce claims in the
// EXACT production register (same prompt, parser, claim budget) instead of
// re-implementing it in a script (VERIFIER_V0.md §3 Stream B).
pub use grounding::extract_claim_list;

/// Per-chunk support probe in the gate's exact register — the bench
/// faithfulness lane's verdict primitive (see grounding/mod.rs docs).
pub use grounding::claim_chunk_support;
// The judge-replay harness's seams (`svrn bench judge-replay`): the joint
// per-claim register, its renderer, its system-turn fingerprint, and the
// specifics scan — pure delegations to the one production implementation,
// exported so an offline replay is scored by the register itself rather
// than a re-implementation (ARCH §10.6; order judge-calibration-replay).
pub use grounding::{
    replay_claim_violation_joint, replay_claims_support_batched, replay_judge_system_turn,
    replay_render_batched_claims_prompt, replay_render_claim_prompt,
    replay_scan_unsupported_specifics,
};
// The deterministic value-presence site checker — public so the Stream B
// export re-validates every constructed corruption with the PRODUCTION
// implementation (the flywheel generates against a pinned port of this fn;
// export is where the genuine article gets the final word).
pub use grounding::value_present_in_chunks;
// The native-grounding stack (`NATIVE_GROUNDING.md`). Default ON since
// 2026-08-11; `SOVEREIGN_NATIVE_GROUNDING=0` is the opt-out. What is on
// is DISPLAY (typed segments + answerability telemetry), never a
// decision. Only `span_resolver` is public within it:
// it is a pure function of `(span, chunks)` that the offline
// resolver-precision measurement replays over frozen transcripts, so it
// has a genuine out-of-crate consumer. The admission stage stays
// crate-private — nothing outside the runtime decides answerability.
pub use grounding::native_grounding;
// The FR-6 decorrelation instrument (order deep-research-t0b) — these two
// strings ARE the production gate functions, exported for the integration
// driver `tests/fr6_decorrelation.rs` that measures them against the labeled
// bank (directives 13efc5dc + e39f87b2). No re-implementation, no substitute
// register: what the driver calls is what the gate runs.
pub use grounding::{claim_violation_joint, scan_unsupported_specifics};
mod formatters;
mod handlers;
/// What enriches a turn, as a value stages receive rather than seven fields
/// they reach back into the `Runtime` to read (daemon-convergence Phase 4a).
pub mod lane;
pub use lane::{Lane, Rerank};
mod intent_helpers;
/// Public for the inner-chaos bench's verifier-calibration gate —
/// rubric changes to the recall grounding verifier must pass a
/// hand-labeled bank before they may ship (same discipline as the
/// recall judge's `--calibrate-recall`).
pub mod memory_grounding;
mod merge_select;
// Public: figure-emitting tools (sovereign-tools::sec_facts) build their
// declared allowed-token sets with `numeric_tokens` — the auditor's own
// lexer, so "allowed" and "audited" cannot drift (ARCH §10.6).
pub(crate) mod authority_guard;
pub mod numeric_audit;
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
/// Serving one turn — drive the stream, forward the narration, emit the
/// terminal metadata frame (`TOPOLOGY.md §10` phase 5c). The one place
/// that turns a `Runtime` into `TurnFrame`s, so a host does not have to be
/// in the same process as the store to learn what a turn concluded.
pub mod serve;
pub use serve::{message_metadata, serve_turn, TurnSink};
/// G4 — the per-turn stage attribution ledger
/// (`NATIVE_GROUNDING_ECONOMY.md` §3.4, §9 Phase 1). Measurement and
/// reporting only; nothing in the runtime branches on it.
mod stage_ledger;
mod streaming;
mod system_message;
mod turn;
mod types;
/// Answer-surface rendering of what retrieval could not reach (§9.6).
mod unavailability;
mod wellbeing;

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
    /// Content-keyed memo for retrieval-over-history candidate units.
    ///
    /// `maybe_retrieve_relevant_history` re-derives an embedding AND
    /// (when GLiNER is wired) an entity set for EVERY dropped
    /// user/assistant pair on EVERY turn. Both are pure functions of
    /// the pair's rendered body, and the set of pairs only ever grows
    /// by one per turn — so the naive path pays Θ(N) embeds on turn N,
    /// Θ(N²) over a conversation. Measured on a 44-turn longhaul
    /// fixture the embed batch alone dominated pre-retrieval latency
    /// past ~turn 20.
    ///
    /// Keyed by a hash of the unit body (not by turn index): index
    /// parity can flip when a turn appends an odd number of messages,
    /// and a content key degrades to a miss rather than to a WRONG
    /// embedding. Process-local and bounded — see
    /// [`Self::history_unit_vectors`].
    pub(crate) history_unit_memo: std::sync::Mutex<
        std::collections::HashMap<u64, self::retrieval::history::HistoryUnitVectors>,
    >,
    /// Per-conversation preemption for post-stream housekeeping: a new
    /// user turn cancels the prior turn's gap-check/refinement token
    /// so fresh turns never queue behind stale background work (the
    /// coach-A/B dead-turn class, 2026-07-11). See
    /// `collaboration::PostStreamPreemption`.
    pub(crate) post_stream_preemption: collaboration::PostStreamPreemption,
    pub corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
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
    /// Resolves the per-request principal from a conversation id so
    /// `build_context` can hide other principals' `Private` corpora on a
    /// multi-user hub. `None` (desktop / CLI / tests) ⇒ no corpus is hidden.
    pub corpus_principal: Option<Arc<dyn crate::traits::PrincipalResolver>>,
    /// Per-folder metadata oracle. Folder-ingest v1 §6.3 — when
    /// retrieval pulls chunks from a watched-folder corpus, this
    /// provides the user-typed display name and the "what I don't
    /// have" gap counters so the synthesis prompt can say "your
    /// case-files folder" and surface skipped/failed-file notes.
    /// `None` = no folder corpora known (CLI fallback / tests),
    /// which preserves the pre-Phase-F label rendering exactly.
    pub folder_metadata: Option<Arc<dyn crate::traits::FolderMetadataOracle>>,
    /// Sticky in-conversation memory pins (relational recall).
    /// conv_id → the memory ids most recently RENDERED to the user in
    /// that conversation. Once the witness has shown the user an
    /// entry, later turns keep it in view even when the new turn's
    /// query embeds elsewhere — hand-read transcripts (2026-07-09)
    /// showed per-turn retrieval swapping the window on follow-up
    /// meta-questions ("what does my record actually say?"), making
    /// the witness deny or retract entries it had surfaced one turn
    /// earlier: the single worst trust-breaker in the set. Process-
    /// local by design (a resumed conversation re-pins on its first
    /// recall); capped per conversation at the render window.
    pub(crate) recall_pins: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
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
    /// Everything that ENRICHES a turn, as one value the host names at
    /// construction (daemon-convergence Phase 4b).
    ///
    /// Was seven independent `Option` fields filled by eight `with_*`
    /// builders. The pair-independence pass over all three live
    /// `Runtime::new` sites found no variant structure among them — the
    /// divergence between hosts was omission, not topology, and a builder
    /// cannot prevent an omission because a forgotten call is
    /// indistinguishable from a host that genuinely has no such provider.
    /// So it is a constructor argument: a host names its providers, or names
    /// `LaneSources::none()`.
    ///
    /// Stages never read this. They receive [`Lane`] — the per-turn snapshot
    /// — as a parameter, and `tests/lane_reach_through_census.rs` fails if
    /// one reaches back in. Grouping alone would have bought nothing (§3.5:
    /// `self.gliner` becoming `self.lane.gliner` is the same coupling down a
    /// longer path); the grouping is only honest BECAUSE the reads went to
    /// zero first, in Phase 4a.
    pub lane_sources: lane::LaneSources,
}

/// Everything a host supplies to commission a [`Runtime`] — total, by
/// construction.
///
/// # The split brain this replaces
///
/// Measured 2026-08-25 across the three live commissioning sites (desktop
/// `state.rs`, server `main.rs`, `svrn chat` `bootstrap.rs`), the builder
/// surface was used like this:
///
/// | slot | desktop | server | chat |
/// |---|---|---|---|
/// | `corpus_engine` | yes | yes | yes |
/// | `routing_events` | yes | yes | **no** |
/// | `note_store` | conditional | conditional | conditional |
/// | `landscape_digests` | conditional | conditional | **no** |
/// | `mesh_knowledge` | conditional | **no** | conditional |
/// | `compaction` | yes | **no** | **no** |
/// | `sensitive_corpora` | conditional | **no** | **no** |
/// | `folder_metadata` | conditional | **no** | **no** |
/// | `corpus_principal` | **no** | yes | **no** |
/// | `sessions` | **no** | **no** | **no** |
///
/// Only one row is common to all three. Every **no** was indistinguishable
/// from an oversight, because a builder chain records a call and records
/// nothing at all about a call not made. Here each is a field the host must
/// write, so "this host has no folder metadata" and "this host forgot folder
/// metadata" stop being the same text.
///
/// `sessions` is the one row no host sets at all — its only caller anywhere is
/// a single test that needs the narration threshold wound to zero. It kept a
/// field rather than being deleted so that fact is written down instead of
/// rediscovered; a builder nobody called said nothing about why.
///
/// # Two things this does NOT yet fix, stated rather than implied
///
/// - `corpus_engine` is `Option` here even though all three hosts supply one
///   unconditionally, so §3.5 is right that it "was never optional". Making
///   the *field* non-optional means giving every test harness a real engine,
///   which is a separate change; naming the absence is what this one buys.
/// - `sensitive_corpora: None` still means "no sensitivity gate applied, all
///   corpora eligible" — a privacy control whose absence is permissive, which
///   §3.5 flags as §7 inverted. A host must now write the `None`, so the
///   choice is at least visible at the call site. The semantics are unchanged
///   and still wrong.
pub struct RuntimeParts {
    pub inference: Arc<dyn InferenceProvider>,
    pub router: Box<dyn Router>,
    pub planner: Box<dyn Planner>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub skills: Arc<SkillRegistry>,
    pub approval: Arc<dyn ApprovalChannel>,
    pub inference_config: InferenceConfig,
    /// The turn's enrichment stack, in one value (Phase 4b).
    pub lane: lane::LaneSources,
    pub corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
    pub note_store: Option<Arc<corpus_engine_notes::NoteStore>>,
    pub compaction: Option<Arc<crate::memory_compaction::CompactionWorker>>,
    pub mesh_knowledge: Option<Arc<dyn crate::traits::MeshKnowledgeSource>>,
    pub landscape_digests: Option<Arc<dyn crate::traits::LandscapeDigestProvider>>,
    pub sensitive_corpora: Option<Arc<dyn crate::traits::SensitiveCorpusOracle>>,
    pub corpus_principal: Option<Arc<dyn crate::traits::PrincipalResolver>>,
    pub folder_metadata: Option<Arc<dyn crate::traits::FolderMetadataOracle>>,
    /// Where narration, interpretation and clarification go. Not an `Option`:
    /// a host that wants none writes `Arc::new(NoOpRoutingEventSink)` and says
    /// so. `svrn chat` silently had no sink for the whole life of the builder
    /// surface, which is why this is the one absence that must be typed out.
    pub routing_events: Arc<dyn RoutingEventSink>,
    /// Per-conversation session table. `None` ⇒ the `Runtime` makes its own,
    /// which is what every host does — **no host sets this**. It is a field
    /// rather than a deleted builder because one test needs a store with the
    /// narration threshold wound down to zero, and a testing seam named in the
    /// shape is honest where a builder nobody called was not.
    pub sessions: Option<SharedSessionStore>,
}

impl RuntimeParts {
    /// The nine slots every host must resolve, with the nine optional ones set
    /// to named absence. Hosts override what they have with struct-update
    /// syntax, so the overrides read as a diff against this baseline.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        router: Box<dyn Router>,
        planner: Box<dyn Planner>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
        skills: Arc<SkillRegistry>,
        approval: Arc<dyn ApprovalChannel>,
        inference_config: InferenceConfig,
        lane: lane::LaneSources,
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
            lane,
            corpus_engine: None,
            note_store: None,
            compaction: None,
            mesh_knowledge: None,
            landscape_digests: None,
            sensitive_corpora: None,
            corpus_principal: None,
            folder_metadata: None,
            routing_events: Arc::new(NoOpRoutingEventSink),
            sessions: None,
        }
    }
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

    /// Commission a `Runtime` from ONE total value.
    ///
    /// # Why there are no builders
    ///
    /// Phase 4b (2026-08-25) folded the eight enrichment builders into a
    /// required [`lane::LaneSources`] after measuring that a builder cannot
    /// enforce installation. The measurement's headline was not hypothetical:
    /// for months only `svrn chat` called `with_rerank`, while the ledger
    /// reported the reranker available on all three hosts.
    ///
    /// The remaining ten builders had exactly the same defect and it showed up
    /// as a THREE-WAY SPLIT BRAIN. Measured across the three live hosts on
    /// 2026-08-25, no two commissioned the same `Runtime`: the desktop called
    /// eleven builders, the server five, `svrn chat` three, and only
    /// `with_corpus_engine` was common to all three. Nothing in the type
    /// system said which were host-specific policy and which were simply
    /// forgotten, because a builder chain records neither.
    ///
    /// So the same move applies: every host-settable slot is a FIELD of
    /// [`RuntimeParts`], named at the call site whether it is supplied or not.
    /// The three hosts now differ in the DATA they write, which is diffable,
    /// instead of in which methods they remembered to call, which was not.
    ///
    /// `install_meta_atlas` deliberately survives as the one `&self`
    /// installer: the desktop's background index warm genuinely completes
    /// after commissioning, and that is a real deferral rather than a
    /// forgotten call.
    pub fn new(parts: RuntimeParts) -> Self {
        let RuntimeParts {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
            approval,
            inference_config,
            lane,
            corpus_engine,
            note_store,
            compaction,
            mesh_knowledge,
            landscape_digests,
            sensitive_corpora,
            corpus_principal,
            folder_metadata,
            routing_events,
            sessions,
        } = parts;
        Self {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
            approval,
            inference_config,
            corpus_engine,
            note_store,
            assembly_memo: std::sync::RwLock::new(std::collections::HashMap::new()),
            history_unit_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
            post_stream_preemption: collaboration::PostStreamPreemption::default(),
            compaction,
            mesh_knowledge,
            landscape_digests,
            sessions: sessions.unwrap_or_else(|| Arc::new(SessionStore::new())),
            confidence_thresholds: ConfidenceThresholds::default(),
            routing_events,
            sensitive_corpora,
            corpus_principal,
            folder_metadata,
            lane_sources: lane,
            recall_pins: std::sync::Mutex::new(std::collections::HashMap::new()),
            turn_provenance: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Merge this turn's fresh relational recall with the entries the
    /// witness has actually SPOKEN ABOUT in this conversation (pinned
    /// by `pin_referenced_memory` from the grounding verifier's
    /// `referenced` signal). Pinned entries lead — once the witness
    /// has named an entry to the user, a follow-up turn must still
    /// have it in view; per-turn similarity retrieval swaps the window
    /// on meta-questions and the witness ends up denying what it said
    /// one turn earlier.
    ///
    /// Pins are reference-driven, NEVER render-driven: a first version
    /// that pinned whatever was rendered locked warmup-turn noise into
    /// the window and displaced the correct recall on the turn that
    /// mattered (hand-read, 2026-07-09). At most 2 pins lead, so fresh
    /// recall always keeps at least one render slot.
    pub(crate) async fn merge_recall_pins(
        &self,
        conversation_id: &str,
        scope: &crate::traits::MemoryScope,
        fresh: Vec<crate::types::Memory>,
    ) -> Vec<crate::types::Memory> {
        const PIN_CAP: usize = 2;
        const WINDOW: usize = 5;
        let pinned_ids: Vec<String> = self
            .recall_pins
            .lock()
            .map(|m| m.get(conversation_id).cloned().unwrap_or_default())
            .unwrap_or_default();

        let mut merged: Vec<crate::types::Memory> = Vec::with_capacity(WINDOW);
        if !pinned_ids.is_empty() {
            // Pinned entries the fresh recall didn't resurface are
            // fetched from the scoped pool (same wall as recall).
            let mut by_id: std::collections::HashMap<String, crate::types::Memory> = self
                .store
                .get_all_memories_for_scope(scope)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|m| (m.id.clone(), m))
                .collect();
            for id in pinned_ids.iter().take(PIN_CAP) {
                if let Some(m) = fresh
                    .iter()
                    .find(|m| &m.id == id)
                    .cloned()
                    .or_else(|| by_id.remove(id))
                {
                    if !merged.iter().any(|x| x.id == m.id) {
                        merged.push(m);
                    }
                }
            }
        }
        for m in fresh {
            if merged.len() >= WINDOW {
                break;
            }
            if !merged.iter().any(|x| x.id == m.id) {
                merged.push(m);
            }
        }
        merged.truncate(WINDOW);
        tracing::info!(
            target: "memory_grounding",
            pins = ?pinned_ids,
            window = ?merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            "recall pin merge"
        );
        merged
    }

    /// Record that the witness's (grounded) reply spoke about this
    /// memory — called by the expressive handler with the entry the
    /// grounding verifier attributed the reply to. The most recently
    /// referenced entry leads; a short history of 2 keeps yesterday's
    /// thread available without crowding fresh recall.
    pub(crate) fn pin_referenced_memory(&self, conversation_id: &str, memory_id: &str) {
        if let Ok(mut pins) = self.recall_pins.lock() {
            if pins.len() > 512 {
                pins.clear();
            }
            let entry = pins.entry(conversation_id.to_string()).or_default();
            entry.retain(|id| id != memory_id);
            entry.insert(0, memory_id.to_string());
            entry.truncate(2);
        }
    }

    // EIGHT `with_*` ENRICHMENT BUILDERS WERE DELETED HERE
    // (daemon-convergence Phase 4b, 2026-08-25): `with_gliner`,
    // `with_rerank`, `with_rerank_config`, `with_meta_atlas`, `with_bridge`,
    // `with_atlas_context_provider`, `with_wikipedia_graph` and
    // `with_conv_tiered_reader`. Every one is now a field on the
    // [`lane::LaneSources`] value `new` requires.
    //
    // THE COUNT IS NOT THE POINT; THE FORGETTABILITY IS. A builder cannot
    // enforce that a host installs a provider, because from inside the
    // Runtime a forgotten call and a host that has no such provider are the
    // same state. That is not hypothetical here — it shipped: for months the
    // desktop and the hub server ran baseline fusion ordering because only
    // `svrn chat` called `with_rerank`, and the capability ledger recorded
    // the reranker as available on all three (see the comments still standing
    // at `sovereign-server/src/main.rs` and `sovereign-desktop/state.rs`).
    // The measurement banked on 2026-08-25 shows the same shape across all 19
    // builders: no variant structure, only omissions.
    //
    // `install_meta_atlas` SURVIVES and is below. It is the one member that
    // legitimately arrives after construction, and it is now a cell rather
    // than a second storage.

    /// Fetch the most recent witness-turn provenance for `conversation_id`,
    /// if any. Returns `None` when no provenance has been captured for
    /// that conversation in this Runtime's lifetime (e.g. a fresh
    /// daemon, a non-relational classification, or a conversation that
    /// only ran on the non-streaming witness path).
    pub fn get_last_turn_provenance(&self, conversation_id: &str) -> Option<TurnProvenance> {
        let guard = self.turn_provenance.read().ok()?;
        guard.get(conversation_id).cloned()
    }

    /// Attach the cross-corpus meta-atlas AFTER construction. Lets the
    /// desktop fire `backend-ready` fast and warm the ~900MB index in the
    /// background, then install it into the already-shared `Arc<Runtime>`
    /// (the field is interior-mutable for exactly this). Idempotent;
    /// overwrites any prior index. A poisoned lock is recovered rather
    /// than panicking — a failed warm must never wedge retrieval.
    pub fn install_meta_atlas(&self, index: Arc<corpus_engine::meta_atlas::MetaAtlasIndex>) {
        self.lane_sources.meta_atlas.store(Some(index));
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
        // Memory-extraction pass at conversation end — no retrieval, so no
        // principal scoping is needed.
        let context = build_context(self.store.as_ref(), conversation_id, "", None).await?;
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
    parse_tool_call_inline, render_attached_doc_conversation, strip_dangling_tool_calls,
    truncate_for_chip, AttachedDocSegment,
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
    fn relational_overrides_deep_query_to_expressive() {
        // Inverted 2026-07-08 (inner-chaos baseline receipts): the
        // DeepQuery "witness branch" leaked retrieved corpus evidence
        // into witness threads via kc.prompt, and the streaming
        // surface had no witness branch at all. Relational register
        // now has exactly one path: ExpressiveQuery.
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::DeepQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
    }

    #[test]
    fn relational_overrides_generative_to_expressive() {
        // Same receipts: GenerativeQuery routed a dependency-seeking
        // user into the creative path, which role-played as their
        // partner. No creative side door on the witness surface.
        let out = crate::intent_policy::apply_witness_intent_override(
            &Intent::GenerativeQuery,
            SkillRegister::Relational,
        );
        assert!(matches!(out, Intent::ExpressiveQuery));
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

/// Enrichment-seam invariant — the STRUCTURAL half of the desktop-vs-bench
/// parity gate (`docs/specs/DESKTOP_ENRICHMENT_PARITY.md`).
///
/// `bench parity-compare` gates the runtime BEHAVIOUR (does the desktop surface
/// the same enrichment legs as the bench, per question). This module gates the
/// STRUCTURE: the set of enrichment-consuming `Runtime` provider fields. The
/// desktop runtime (`sovereign-desktop/.../state.rs`) and the bench/CLI bootstrap
/// (`sovereign-cli-llm/.../chat_cmd/bootstrap.rs`) must BOTH wire this set; the
/// 2026-06 regression that motivated this whole effort was exactly a seam
/// (`atlas_context_provider`) wired in the bench but not the desktop, silently
/// killing atlas grounding there.
///
/// We can't build a full `Runtime` here (it needs an inference provider + corpus
/// engine — integration territory), so the invariant is enforced two ways that
/// DO hold at unit-test time:
///   1. `read_enrichment_seams` reads every provider field by name — if a seam is
///      renamed or removed it STOPS COMPILING, forcing reconciliation.
///   2. `seam_count_is_stable` pins the count, so ADDING a seam is a deliberate
///      edit that pulls the author's attention to both bootstraps + the harness.
#[cfg(test)]
mod enrichment_seam_invariant {
    use super::*;

    /// Field-existence change detector over the LANE. Takes `&LaneSources`
    /// rather than `&Runtime` since daemon-convergence Phase 4b, which is what
    /// makes it RUNNABLE — the old version could only ever be compiled,
    /// because it needed a full Runtime (inference provider + corpus engine)
    /// to call.
    fn lane_seams(l: &lane::LaneSources) -> Vec<(&'static str, bool)> {
        vec![
            ("atlas_context", l.atlas_context.is_some()),
            ("wikipedia_graph", l.wikipedia_graph.is_some()),
            ("meta_atlas", l.meta_atlas.load().is_some()),
            ("bridge", l.bridge.is_some()),
            ("rerank", l.rerank.f.is_some()),
            ("gliner", l.gliner.is_some()),
            ("conv_tiered", l.conv_tiered.is_some()),
        ]
    }

    /// The two seams that are NOT lane members and still sit on the Runtime:
    /// §3.5 has both leaving the Runtime entirely (`mesh_knowledge` dissolves
    /// into a loopback call to the daemon's own knowledge route;
    /// `landscape_digests` is a per-connection wire concern). Compiling IS the
    /// assertion; never run.
    #[allow(dead_code)]
    fn departing_seams(rt: &Runtime) -> Vec<(&'static str, bool)> {
        vec![
            ("landscape_digests", rt.landscape_digests.is_some()),
            ("mesh_knowledge", rt.mesh_knowledge.is_some()),
        ]
    }

    /// Adding or removing a lane member is a deliberate edit.
    ///
    /// This assertion USED TO BE `assert_eq!(ENRICHMENT_SEAM_COUNT, 8)` against
    /// a const defined three lines above it — a check with no input that could
    /// make it fail (ARCH §18.1), which is to say not a check. It is now taken
    /// from the reader, so removing a seam fails here rather than passing
    /// silently.
    #[test]
    fn lane_seam_count_is_stable() {
        assert_eq!(lane_seams(&lane::LaneSources::none()).len(), 7);
    }

    /// An empty lane reports every seam absent — the instrument reads real
    /// state rather than returning a fixed shape.
    #[test]
    fn an_empty_lane_reports_every_seam_absent() {
        assert!(lane_seams(&lane::LaneSources::none())
            .iter()
            .all(|(_, present)| !*present));
    }

    /// And a filled one reports it present, which is the half that catches a
    /// reader or a `snapshot` that hard-codes `false` — the failure mode where
    /// every host wires a provider and every stage still sees `None`.
    #[test]
    fn a_wired_seam_is_reported_present() {
        let mut l = lane::LaneSources::none();
        l.rerank.f = Some(std::sync::Arc::new(|_q: &str, docs: Vec<String>| {
            Box::pin(async move { Ok(vec![0.0_f32; docs.len()]) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = corpus_engine::Result<Vec<f32>>> + Send>,
                >
        }));
        assert!(lane_seams(&l).iter().any(|(n, p)| *n == "rerank" && *p));
    }

    /// The snapshot is what stages actually receive, so it — not the source —
    /// is where a dropped member would bite.
    #[test]
    fn the_snapshot_preserves_a_wired_seam() {
        let mut l = lane::LaneSources::none();
        l.rerank.config.enabled = true;
        l.rerank.f = Some(std::sync::Arc::new(|_q: &str, docs: Vec<String>| {
            Box::pin(async move { Ok(vec![0.0_f32; docs.len()]) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = corpus_engine::Result<Vec<f32>>> + Send>,
                >
        }));
        assert!(l.snapshot().rerank.active(), "snapshot dropped `rerank`");
        assert!(!lane::Lane::none().rerank.active());
    }
}
