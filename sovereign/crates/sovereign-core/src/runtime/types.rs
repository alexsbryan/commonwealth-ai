// SPDX-License-Identifier: AGPL-3.0-or-later
//! Data types shared between runtime dispatch paths.
//!
//! These are the shapes that `Runtime`'s handlers and pre-flight helpers
//! produce and consume; they live here (instead of in `runtime.rs`) so
//! the per-intent handler modules in `runtime/handlers/` can construct
//! and pattern-match on them without a circular import back into the
//! main runtime file. Re-exported at the top of `runtime.rs` so the
//! public façade (`sovereign_core::runtime::{TurnProvenance, ...}`) is
//! unchanged.

use std::collections::HashMap;
use std::pin::Pin;

use futures::Stream;

use crate::error::Result;
use crate::traits::FolderMetadata;
use crate::types::{CompletionRequest, CoverageNote, SourceSummary, Speed};

use super::{EvidenceShape, SynthesisRoute};

/// Pre-computed knowledge context shared between streaming and non-streaming
/// response paths. Produced by [`super::Runtime::prepare_knowledge_context`] so
/// the two paths cannot diverge in how they search, build prompts, or report
/// provenance.
pub(crate) struct KnowledgeContext {
    pub(crate) chunks: Vec<corpus_engine::ScoredChunk>,
    pub(crate) prompt: String,
    pub(crate) system: String,
    pub(crate) speed: Speed,
    pub(crate) search_method: Option<String>,
    pub(crate) sources: Vec<SourceSummary>,
    /// Summaries of retrieved chunks for frontend source linking.
    pub(crate) retrieved_chunks: Vec<serde_json::Value>,
    /// Folder-ingest v1 §6.3: per-turn coverage assessment over the
    /// user's watched-folder corpora. `None` when no folder corpus
    /// contributed retrieval; `Some(thin)` when at least one folder
    /// came back below the chunk-count threshold. Threaded through to
    /// `ResponseProvenance.coverage` so the streaming and
    /// non-streaming paths surface the same chip data.
    pub(crate) coverage: Option<CoverageNote>,
    /// TEACHABLE P0 — active-lesson snapshot taken when this context
    /// was built (same discipline as `prompt_budget_note`: the spawn
    /// records what the request was actually built from, so what
    /// applied and what's in metadata cannot drift).
    pub(crate) lessons: TurnLessons,
}

/// TEACHABLE P0 — what the active lessons contributed to this turn.
/// Built once at prepare time from the same snapshot the request was
/// assembled with; rides into the streaming spawn, which (a) runs the
/// post-gate term-avoid pass over `term_avoid`, (b) records `applied`
/// in `Message.metadata.lessons_applied` (dropping the transform entry
/// when the pass changed nothing), (c) stamps `first_application`
/// lessons and emits the one-time `kept_lesson` whisper, and (d)
/// re-injects `prompt_form` into the refinement prompt (today-anchor
/// precedent).
#[derive(Debug, Clone, Default)]
pub(crate) struct TurnLessons {
    pub(crate) term_avoid: Vec<String>,
    pub(crate) applied: Vec<crate::lessons::AppliedLessonMeta>,
    pub(crate) first_application: Vec<crate::lessons::ActiveLesson>,
    pub(crate) prompt_form: Option<String>,
}

impl TurnLessons {
    /// Build the turn manifest from the loaded snapshot plus which
    /// rungs actually engaged at prepare time. The transform rung is
    /// tentative here — the streaming spawn drops it (from `applied`
    /// AND `first_application`) when the post-gate pass changed
    /// nothing, so metadata records influence, not intent.
    pub(crate) fn from_snapshot(
        set: &crate::lessons::ActiveLessonSet,
        length_applied: bool,
        prompt_injected: bool,
    ) -> Self {
        let mut applied = Vec::new();
        let mut first_application = Vec::new();
        let mut track = |lesson: &crate::lessons::ActiveLesson, enforcement: &'static str| {
            applied.push(crate::lessons::AppliedLessonMeta {
                id: lesson.note_id.clone(),
                enforcement,
            });
            if lesson.payload.first_applied_at.is_none() {
                first_application.push(lesson.clone());
            }
        };
        if length_applied {
            if let Some(l) = &set.length {
                track(l, "param");
            }
        }
        let term_avoid = set.term_list();
        if !term_avoid.is_empty() {
            if let Some(l) = &set.term_avoid {
                track(l, "transform");
            }
        }
        let mut prompt_form = None;
        if prompt_injected {
            if let Some(l) = &set.prompt {
                track(l, "prompt");
                prompt_form = Some(l.payload.prompt_form.clone());
            }
        }
        Self {
            term_avoid,
            applied,
            first_application,
            prompt_form,
        }
    }
}

/// One meta-atlas anchor injection. The chat path logs a
/// `Vec<MetaAtlasHitRecord>` per question for observability; the
/// bench surface mirrors it into `EvalResult.meta_atlas_hits` so the
/// per-question JSON carries which entities the meta-atlas recognised
/// and which stream the anchor served.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetaAtlasHitRecord {
    /// Display name from the meta-atlas — what the operator reads
    /// ("Albert Einstein", not "albert einstein").
    pub entity: String,
    /// Corpus the injected chunks came from.
    pub corpus_id: String,
    /// `"inventory" | "argument" | "trace"` — the dominant
    /// articulation axis of the anchor that was picked.
    pub articulation: String,
    /// `"frozen" | "versioned" | "rolling" | null` — the per-corpus
    /// write contract. `null` when the corpus has no stream block
    /// (legacy / atlas-only sibling).
    pub stability: Option<String>,
    /// How many chunks the targeted search returned and were
    /// injected. Zero means the meta-atlas surfaced an anchor but
    /// the per-corpus search yielded nothing useful — diagnostic
    /// when title-coverage stays flat despite meta-atlas hits.
    pub chunks_added: usize,
}

/// Everything `handle_knowledge_query` and the streaming KQ branch need
/// to issue a synthesis request. Produced by
/// [`super::Runtime::prepare_knowledge_query_plan`] so the two paths cannot
/// diverge in retrieval, expansion, or routing behaviour.
///
/// On the empty-retrieval path, `chunks` / `doc_context` /
/// `retrieved_chunks` / `source_map` are all empty and `result_quality`
/// is `"empty"`. The `request` is a parametric-knowledge prompt rather
/// than a retrieval-grounded one.
pub(crate) struct KnowledgeQueryPlan {
    pub(crate) request: CompletionRequest,
    pub(crate) chunks: Vec<corpus_engine::ScoredChunk>,
    /// The question names entities from the corpus's own world (atlas
    /// gazetteer match in the agentic loop). The grounding gate uses
    /// this to close the general-knowledge exemption: outside
    /// knowledge structurally cannot establish in-world facts, so a
    /// GK-caveated assertion still gets claim-extracted and verified.
    /// False on the parametric/empty paths and when the loop is off.
    pub(crate) gate_entity_anchored: bool,
    /// Formatted chunk text used as evidence for the gap check.
    /// Empty string on the parametric path.
    pub(crate) doc_context: String,
    pub(crate) shape: EvidenceShape,
    pub(crate) route: SynthesisRoute,
    pub(crate) gap_check_enabled: bool,
    pub(crate) search_ms: u64,
    pub(crate) retrieved_chunks: Vec<serde_json::Value>,
    pub(crate) source_map: HashMap<String, usize>,
    /// `"empty"` | `"focused"` | `"synthesis"` | `"routed"` —
    /// surfaced in message metadata for the UI to label the turn.
    pub(crate) result_quality: &'static str,
    /// Non-`None` when the prompt-budget guard trimmed the request to
    /// fit the context window (see `runtime::prompt_budget`). Rides
    /// into message metadata as `prompt_budget` so the degradation is
    /// operator-visible rather than silent.
    pub(crate) prompt_budget_note: Option<String>,
    /// Snapshot of the folder-metadata oracle taken when the plan
    /// was built. Carried through to the streaming spawn so the
    /// final assistant message's `ResponseProvenance` can include
    /// folder display names and the coverage chip without a second
    /// oracle round-trip. Empty map = no folder corpora known
    /// (CLI / test harness fallback) → coverage chip suppressed,
    /// `display_name` falls back to `corpus_id`.
    pub(crate) folder_meta: HashMap<String, FolderMetadata>,
    /// Meta-atlas hit records (Move 5). One per injected anchor
    /// (max 3 per matched meta-atom — one per articulation axis with
    /// a dominant anchor). Surfaced in synth metadata so the bench's
    /// per-question JSON can carry "which canonical entities did the
    /// meta-atlas recognise and which stream did each anchor
    /// serve" — the fourth legibility lens.
    pub(crate) meta_atlas_hits: Vec<MetaAtlasHitRecord>,
    /// TEACHABLE P0 — active-lesson snapshot taken when the plan was
    /// built. See [`TurnLessons`].
    pub(crate) lessons: TurnLessons,
    /// Why this plan answers from parametric general knowledge rather
    /// than retrieved evidence, when it does. Carried as DATA so the
    /// epistemic ledger reads the decision instead of re-deriving it
    /// from the `GK_CAVEAT_PREFIX` string (EPISTEMIC_STATE.md §4.2).
    /// `None` on evidence-grounded plans. The decode-committed prefix
    /// behavior itself is unchanged.
    pub(crate) general_knowledge: Option<GkReason>,
    /// The turn's demand set with coverage stamps (EPISTEMIC_STATE.md
    /// P1a) — retained through the turn so ledger assembly reads the
    /// same structure retrieval used, instead of re-deriving it from
    /// a string post-hoc (the gap.rs failure mode).
    pub(crate) demands: Vec<crate::types::Demand>,
    /// The pipeline's query embedding, retained for the gap-turn
    /// coverage probe (reuse, never re-embed).
    pub(crate) query_embedding: Vec<f32>,
}

/// Why a turn fell back to parametric general knowledge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GkReason {
    /// Retrieval returned zero chunks — nothing to ground on.
    ZeroChunk,
    /// The agentic evidence loop ran and still judged the pool
    /// insufficient, and the question anchors to no enabled corpus.
    AgenticInsufficient,
}

/// Retrieve-only projection of a [`KnowledgeQueryPlan`] — the evidence
/// pool the production KnowledgeQuery pipeline assembled for a query,
/// without a synthesis pass. Returned by
/// [`super::Runtime::retrieve_evidence`], which the bench parity lane
/// drives so the measured retrieval surface and the product surface are
/// the same code path (RETRIEVAL_REDESIGN.md §7.1).
pub struct EvidenceRetrieval {
    /// The merged, pipeline-composed evidence pool (post truncate tail).
    pub chunks: Vec<corpus_engine::ScoredChunk>,
    /// Wall time of the retrieval pipeline run, embed included.
    pub search_ms: u64,
    /// `"empty" | "focused" | "synthesis" | "routed"` — the plan's
    /// result-quality label (same value message metadata carries).
    pub result_quality: &'static str,
}

/// Streaming handle returned by [`super::Runtime::handle_message_stream`].
///
/// Holds the assistant message id (assigned up-front so callers can correlate
/// chunks) and a stream of text chunks. The runtime persists the full message
/// to the store after the stream is exhausted.
pub struct StreamHandle {
    pub message_id: String,
    pub stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
}

/// Glassbox snapshot of what the witness path actually sent to the
/// model on a given turn. Captured at dispatch time inside
/// [`super::Runtime::handle_expressive_query_stream`] and stashed in
/// [`super::Runtime::turn_provenance`] so the desktop's inner-work surface
/// can pull it back via Cmd+? without instrumenting the live stream.
///
/// The shape is meant to be readable by a human investigating a bad
/// witness response: full assembled system prompt, the recalled
/// memories the witness drew on, the conversation history slice
/// actually passed to the inference call (today: empty — the
/// streaming witness path sends only the current user message), the
/// model id + token budget, and Pass A timing. When a response feels
/// untethered, the provenance answers "did the model see what we
/// thought it saw?" without anyone having to re-run the turn.
///
/// History note: the streaming path's `prompt: message` field puts
/// only the latest user message in front of the model; there is no
/// list of prior turns. `history_summary.sent_to_model` is therefore
/// empty in current capture sites — that emptiness is itself a
/// diagnostic. When history-injection is wired (a likely outcome of
/// the very investigations this struct exists to enable) the field
/// populates without a schema change.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TurnProvenance {
    pub conversation_id: String,
    pub message_id: String,
    /// Epoch seconds. Matches the `i64` shape the rest of the runtime
    /// uses (see `fn now()`); the desktop side reads it as a JS number.
    pub captured_at: i64,
    pub register: String,
    pub user_message: String,
    pub system_prompt: String,
    pub system_prompt_chars: usize,
    pub recalled_memories: Vec<RecalledMemoryProv>,
    pub history_summary: HistorySummaryProv,
    pub temporal_tensions: Vec<String>,
    pub contradiction: Option<ContradictionProv>,
    pub current_goal: Option<String>,
    pub recent_topic: Option<String>,
    pub last_assistant_excerpt: Option<String>,
    pub model_id: Option<String>,
    pub max_tokens: Option<usize>,
    pub enable_thinking: Option<bool>,
    pub pass_a_ms: Option<u64>,
    /// Outcome of the witness recall-grounding verifier for this turn
    /// (`runtime/memory_grounding.rs`). Previously computed and
    /// discarded after pinning; retained so the epistemic ledger can
    /// distinguish a verified recall from a fail-open one. `None` when
    /// the verifier didn't run (non-witness turns, older frames).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_verification: Option<RecallVerificationProv>,
}

/// Persisted outcome of the witness recall-grounding verifier.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecallVerificationProv {
    /// Whether the final reply's past-claims were confirmed contained
    /// in retrieved entries.
    pub grounded: bool,
    /// True when the verifier errored/declined and the reply shipped
    /// unchecked (the deliberate availability posture, made visible).
    pub fail_open: bool,
    /// 1-based index of the recalled entry the reply spoke about, when
    /// the verifier attributed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referenced: Option<usize>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecalledMemoryProv {
    pub id: String,
    pub content: String,
    pub created_at: i64,
    /// `"raw"` for an extraction; `"summary"` for a row written by
    /// the compaction worker. Optional in the JSON shape for
    /// backward-compat with provenance frames persisted before the
    /// compaction-fields wiring (2026-05-23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// For summaries: the ids of the source `Raw` memories this row
    /// folded. Empty (or absent) on raw memories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_memory_ids: Vec<String>,
    /// Stored confidence at recall time — the input the epistemic
    /// band derivation (`memory::band_for_confidence`) reads, retained
    /// so the ledger and the prompt agree on the band. Absent on
    /// provenance frames persisted before this field (2026-07-18).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HistorySummaryProv {
    /// Total messages on the conversation when the turn was dispatched.
    pub total_messages: usize,
    pub user_count: usize,
    pub assistant_count: usize,
    /// The slice that was actually passed to the inference call. The
    /// streaming witness path sends only the current user message
    /// today, so this is empty even when `total_messages` is large.
    pub sent_to_model: Vec<HistoryEntryProv>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntryProv {
    pub role: String,
    pub content_preview: String,
    pub full_chars: usize,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContradictionProv {
    pub prior_evidence: String,
    pub current_claim: String,
}
