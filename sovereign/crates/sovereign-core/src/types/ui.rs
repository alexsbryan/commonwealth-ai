//! Response-provenance / UI / insight types — split from monolithic types.rs
//! (ARCH §3.2); re-exported by types/mod.rs (paths unchanged).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

// ─── Response Provenance ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseProvenance {
    pub intent: String,
    pub search_method: Option<String>,
    pub sources: Vec<SourceSummary>,
    pub inference_backend: String,
    pub oicp_match: Option<String>,
    pub total_latency_ms: u64,
    pub tokens_used: usize,
    /// Coarse router classification ("SIMPLE", "LOOKUP", "REASONING", "ACTION").
    /// `None` for old messages that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coarse_intent: Option<String>,
    /// Self-assessment gate result, set on SIMPLE paths only.
    /// `None` when not applicable or for old messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_assessment: Option<String>,
    /// Human-readable rationale for the coarse classification — set
    /// by the router itself (e.g. `"current/time-sensitive signal →
    /// external tool"`, `"factual-lookup shape (what/who/when/where)
    /// → knowledge query"`, `"first-person + content-discourse verb
    /// → personal-corpus lookup"`). Surfaced in the desktop
    /// RoutingMeta footer so the operator can tell whether a
    /// surprising route came from a heuristic shortcut or the LLM
    /// classifier, without having to scrape the daemon logs. `None`
    /// when no rationale was emitted (rare: usually only on errors)
    /// or for old messages that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_trigger: Option<String>,
    /// Folder-ingest v1 §6.3: per-turn coverage assessment over the
    /// user's watched-folder corpora. `None` for turns where no
    /// folder corpus contributed retrieval (the common "talked to a
    /// public knowledge base" case). When `Some`, the chat surface
    /// renders a quiet chip enumerating thin folders so the user
    /// learns *what we don't have* without a second click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageNote>,
    /// Why the streaming generation stopped. OpenAI-compatible string
    /// (`"stop"` / `"length"` / `"content_filter"` / `"cancelled"` /
    /// `"error"`). `None` on non-streaming paths and on old messages.
    /// `Length` is the load-bearing signal — desktop renders a chip
    /// + Continue offer so the user can tell the response was cut off
    /// at `max_tokens_budget`. Typed `FinishReason` serializes to the
    /// OpenAI-compatible lowercase string on the wire (e.g.
    /// `"length"`) — see `FinishReason`'s serde docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Token budget the request was capped at. Pairs with `tokens_used`
    /// and `finish_reason` so the truncation chip can read e.g. "Hit
    /// the 2048-token limit". `None` on paths that don't (yet) capture
    /// this; surfaced where it's reliably known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_budget: Option<usize>,
    /// Completion tokens generated on this turn (excludes prompt).
    /// Streamed paths can populate this from the terminal `Finish`
    /// frame's `usage`. `None` when the provider didn't report usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    /// Active chat-slot context window, sourced from
    /// `InferenceProvider::effective_context_size()`. Pairs with
    /// `tokens_used` so the desktop chat bubble can render a budget
    /// indicator — e.g. `2415 / 16384 (15%)`, brightening as the
    /// turn approaches the cap. `None` on providers without a local
    /// slot (remote API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSummary {
    pub origin: String,
    pub count: usize,
    /// When set, this corpus's hits came from a mesh peer — the
    /// string is the peer's human-readable `node_name` (matching what
    /// the mesh UI shows). Rendered as `"sep (6) via mac-peer"` by
    /// `RoutingMeta.svelte`. Locally-hosted corpora leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_peer: Option<String>,
    /// Folder-ingest v1 §6.3: when this corpus is a watched folder,
    /// the user-typed display name (e.g. "case files") that the
    /// chat surface renders instead of the opaque `corpus_id` slug.
    /// `None` for non-folder corpora (SEP, Wikipedia, mesh hits) so
    /// the UI keeps its existing label rendering for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Per-turn coverage assessment. `kind == "thin"` means at least one
/// folder corpus that contributed retrieval came back with fewer
/// than `thin_threshold` chunks — likely under-served by the user's
/// own materials. The chat surface renders a one-line chip listing
/// the thin folders so the user can decide whether to (a) accept
/// the result, (b) re-phrase, or (c) extend the folder's contents.
///
/// `kind == "ok"` is reserved for forward compatibility — today's
/// runtime simply omits the field (`coverage: None`) when coverage
/// is fine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageNote {
    pub kind: String,
    pub thin_threshold: usize,
    pub thin_folders: Vec<ThinFolder>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinFolder {
    pub corpus_id: String,
    pub display_name: String,
    pub chunks: usize,
    /// Files in this folder whose extension isn't in the watcher's
    /// accept list (e.g. `.pages`, `.key`). When non-zero, surfaces
    /// in the chip as ", N files in unsupported formats" so the
    /// user knows the gap is structural and not just retrieval-quality.
    pub skipped_files: usize,
    /// Files the watcher tried and failed to extract (encrypted,
    /// corrupt, etc). Surfaced same as `skipped_files`.
    pub failed_files: usize,
}

// ─── Action Preview (for approval) ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPreview {
    pub tool_id: ToolId,
    pub description: String,
    pub params: serde_json::Value,
}

// ─── Insight Types ────────────────────────────────────────────

/// A captured insight node — the output of a clip action.
/// Created when the user clips a paragraph from a conversation response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightNode {
    pub id: uuid::Uuid,
    /// The clipped paragraph text (verbatim).
    pub clipped_text: String,
    /// The conversation message this was clipped from.
    pub message_id: uuid::Uuid,
    /// The paragraph index within the message (for re-highlighting on revisit).
    pub paragraph_index: usize,
    /// Provenance: corpus and article.
    pub source: InsightSource,
    /// Field model position, if the paragraph carried position attribution.
    pub position: Option<InsightPosition>,
    /// System-inferred adjacent concepts (from embedding similarity).
    pub adjacent: Vec<String>,
    /// Embedding of the clipped text (for semantic search across the collection).
    pub embedding: Option<Vec<f32>>,
    /// When the clip was made.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Sink state: where this node lives / has been synced.
    pub sink_state: InsightSinkState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightSource {
    pub corpus_id: Option<String>,
    pub article_title: Option<String>,
    pub conversation_id: uuid::Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightPosition {
    pub name: String,
    pub style: PositionStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PositionStyle {
    Compatibilism,
    HardIncompatibilism,
    Libertarianism,
    /// For future field model positions not in the pre-defined set.
    /// Rendered with a neutral gray badge.
    Custom {
        bg: String,
        text: String,
        border: String,
    },
}

/// Where an insight currently lives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InsightSinkState {
    /// Stored in Sovereign's native SQLite insight store only.
    Local,
    /// Pending sync to a configured external sink (e.g. Obsidian vault).
    PendingSync,
    /// Successfully synced to an external sink.
    Synced {
        sink_id: String,
        synced_at: chrono::DateTime<chrono::Utc>,
    },
    /// Sync attempted but failed.
    SyncFailed { sink_id: String, error: String },
}
