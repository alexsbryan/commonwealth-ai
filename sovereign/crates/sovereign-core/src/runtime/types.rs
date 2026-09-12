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
    /// Corpora this turn would have searched and could not. Twin of the
    /// field on [`KnowledgeQueryPlan`] — the DeepQuery path carries it here.
    pub(crate) unavailable_corpora: Vec<crate::traits::CorpusUnavailable>,
    pub(crate) prompt: String,
    /// The call-graph block appended to `prompt` for code-intel hits, kept
    /// separately so the DeepQuery grounding gate can seal it into the turn's
    /// evidence universe. Same rationale as `KnowledgeQueryPlan::code_trace`:
    /// injected-but-unsealed meant every compiler-resolved caller fact came
    /// back "could not be confirmed". Empty on non-code turns.
    pub(crate) code_trace: String,
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
    /// Evidence-derived output budget (R3, un-deferred by the
    /// drafter-attribution-discipline order): `soft_target` fed the
    /// length directive spliced into `prompt`; `hard_ceiling` is what
    /// the streaming layer must pass as the request's `max_tokens`.
    /// ONE decider (`resolve_output_budget`, shared with the
    /// KnowledgeQuery path) for both numbers — before this field the
    /// deep path pled `inference_config.max_tokens` (2048) in the
    /// prompt while the request enforced `max(config, 4096)`, a 2x
    /// plea/parameter contradiction neither side derived from the
    /// evidence.
    pub(crate) output_budget: crate::runtime::evidence::OutputBudget,
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
    /// The call-graph block injected into `doc_context` for code-intel hits
    /// (`code_trace::build_code_trace_block`), kept separately so the
    /// grounding gate can seal it into the turn's evidence universe. Without
    /// it the gate verifies the answer against prose chunks only and reports
    /// every compiler-resolved caller fact as unconfirmed. Empty for the
    /// common non-code turn — the plan carries a `String`, not an `Option`,
    /// because "no trace" and "empty trace" are the same thing here.
    pub(crate) code_trace: String,
    pub(crate) shape: EvidenceShape,
    pub(crate) route: SynthesisRoute,
    pub(crate) gap_check_enabled: bool,
    /// Corpora this turn would have searched and could not — carried from
    /// `PipelineState::unavailable_corpora` so the answer surface can name
    /// them. Empty on every turn that lost nothing, which is the
    /// no-regression bar. See `runtime::unavailability`.
    pub(crate) unavailable_corpora: Vec<crate::traits::CorpusUnavailable>,
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
    /// H1's typed admission verdict, when the native grounding path ran
    /// — which since 2026-08-11 is every turn by default. `None` whenever
    /// the path did not run (opted out with
    /// `SOVEREIGN_NATIVE_GROUNDING=0`, or no instrument), and `None` is
    /// what keeps that behavior byte-identical to the incumbent: nothing
    /// downstream reads this field unless it is `Some`.
    ///
    /// Carried as DATA for the same reason `general_knowledge` is: the
    /// stage that decided hands the decision forward typed, and no later
    /// stage re-derives answerability from a score, a prefix, or a
    /// string (`NATIVE_GROUNDING.md §6`).
    pub(crate) grounding_verdict: Option<crate::types::GroundingVerdict>,
}

/// Why a turn fell back to parametric general knowledge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GkReason {
    /// Retrieval returned zero chunks — nothing to ground on.
    ZeroChunk,
    /// The agentic evidence loop ran and still judged the pool
    /// insufficient, and the question anchors to no enabled corpus.
    AgenticInsufficient,
    /// The gate abstained AND the coverage probe judged the topic
    /// uncovered by the enabled corpora — the answer was re-synthesized
    /// from parametric knowledge under the GK caveat (gk_rescue.rs).
    /// The probe verdict is what makes this safe: an in-topic
    /// (ClaimUncovered) abstention is NEVER rescued, so a labelled-but-
    /// confident in-world fabrication can't ride this path (the failure
    /// the 2026-07-01 exactval fix closed).
    OodRescue,
    /// Retrieval returned chunks but the evidence-shape verdict judged
    /// them quantitatively hopeless (semantic floor + token-coverage
    /// floor both failed — `evidence::evidence_early_decline`,
    /// SOVEREIGN_EVIDENCE_DECLINE_FLOOR). The turn declines FAST on a
    /// tiny parametric prompt instead of synthesizing over off-topic
    /// passages and re-deriving the same verdict through a gap-check
    /// judge 60s later (the 2026-07-21 slow-abstention pathology).
    WeakEvidence,
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
    /// Corpora this turn would have searched and could not — carried out of
    /// `PipelineState::unavailable_corpora` exactly as the chat surface
    /// carries it, so the MEASUREMENT surface can see the same loss the
    /// ANSWER surface reports.
    ///
    /// It was dropped here until 2026-08-29, and dropping it is what makes a
    /// bench score on partial retrieval look like a score. Both plan shapes
    /// compute it one function away (`KnowledgeQueryPlan::unavailable_corpora`
    /// and its `KnowledgeContext` twin); this struct simply had nowhere to put
    /// it, so the parity lane read a pool assembled without a corpus and
    /// reported a number for it. That is the success-shaped wrong result
    /// ARCH §18.3 forbids, and it is worst exactly where retrieval crosses the
    /// mesh: a peer that times out costs the pool a corpus and costs the run
    /// nothing.
    ///
    /// Empty on every turn that lost nothing, which is the no-regression bar.
    pub unavailable_corpora: Vec<crate::traits::CorpusUnavailable>,
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

/// MOVED DOWN 2026-09-11 (sv-surface svt-3): the turn-provenance record is
/// the answer of `GET /v1/conversations/{id}/provenance` and the desktop's
/// ProvenancePanel reads it over the wire, so the type lives in
/// `sovereign_contracts::daemon_wire` and this path is a re-export.
pub use sovereign_contracts::daemon_wire::{
    ContradictionProv, HistoryEntryProv, HistoryRecallProv, HistorySummaryProv,
    RecallVerificationProv, RecalledMemoryProv, TurnProvenance,
};

/// Was `HistoryRecallProv::from_context` until the type moved to
/// `sovereign_contracts::daemon_wire` (2026-09-11); an inherent impl cannot
/// follow a foreign type, and the projection reads core's
/// `ConversationContext`, so it stays here as a free function.
/// Longest excerpt the ledger keeps per recalled pair. The hit
/// bodies are already truncated to 600 chars per message at index
/// time; this trims further because provenance frames are
/// persisted per turn and the point here is attribution, not a
/// second copy of the conversation.
const EXCERPT_CHARS: usize = 240;

/// Project this turn's recall hits into ledger rows. Empty when
/// retrieval-over-history didn't run or found nothing above the
/// similarity floor — the same `None`/empty distinction the
/// prompt renderer makes.
pub(crate) fn history_recall_from_context(
    context: &crate::types::ConversationContext,
) -> Vec<HistoryRecallProv> {
    context
        .history_retrieval_hits
        .as_ref()
        .map(|hits| {
            hits.iter()
                .map(|h| HistoryRecallProv {
                    turn_index: h.turn_index,
                    similarity: h.similarity,
                    excerpt: h.content.chars().take(EXCERPT_CHARS).collect(),
                })
                .collect()
        })
        .unwrap_or_default()
}
