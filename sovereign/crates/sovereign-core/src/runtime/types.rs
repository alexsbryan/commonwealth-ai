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
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RecalledMemoryProv {
    pub id: String,
    pub content: String,
    pub created_at: i64,
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
