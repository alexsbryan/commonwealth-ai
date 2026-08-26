// SPDX-License-Identifier: AGPL-3.0-or-later
//! Response-provenance / UI / insight types — split from monolithic types.rs
//! (ARCH §3.2); re-exported by types/mod.rs (paths unchanged).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

// ─── Response Provenance ──────────────────────────────────────

/// Which classifiers were LIVE when a turn was routed — the DECIDER set, not
/// the decision.
///
/// `ResponseProvenance` already records what a turn was routed AS
/// (`intent`, `coarse_intent`). It records nothing about what did the routing,
/// so a turn classified by a live embed router and a turn that fell through
/// with no classifier at all are indistinguishable in the data.
///
/// That gap has a measured cost. On 2026-08-26 this host's embed slot died;
/// `build_llm_router` returned `None` for all four classifiers, atlas
/// grounding went from 1082 loads to zero, and turns kept answering — worse.
/// The degradation reached the operator as a `progress.note` and reached the
/// regression harness as a QUALITY REGRESSION: SEP overview title-coverage
/// 1.00 -> 0.83, which cost most of a session to attribute (note `f4972e1b`).
/// A turn produced in that state is not a measurement, and nothing in the
/// record said so.
///
/// [`Self::routed_by_none`] is the question worth asking, and it has one
/// implementation (ARCH §10.6) so a bench, a UI and a log line cannot each
/// decide "degraded" differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RouterStamp {
    /// The embedding-similarity router over exemplars.
    pub embed: bool,
    /// Scope classifier (is this about the estate, the web, the code?).
    pub scope: bool,
    /// Effort classifier (how much work does this deserve?).
    pub effort: bool,
    /// Current-info classifier (does this need fresh data?).
    pub current_info: bool,
}

impl RouterStamp {
    /// Build from the four `Option`s a router bootstrap yields.
    pub fn from_liveness(embed: bool, scope: bool, effort: bool, current_info: bool) -> Self {
        Self {
            embed,
            scope,
            effort,
            current_info,
        }
    }

    /// **No classifier was live.** The turn was routed by fallback, and
    /// nothing it produced should be read as a quality measurement.
    ///
    /// This is the state a dead embed slot puts the whole host into, and the
    /// one a harness must exclude rather than score — the same rule
    /// `EvalResult::error` enforces for a failed turn (ARCH §18.2, §18.3).
    pub fn routed_by_none(&self) -> bool {
        !(self.embed || self.scope || self.effort || self.current_info)
    }

    /// How many of the four were live. For a log line or a status row that
    /// wants degradation as a degree rather than a boolean.
    pub fn live_count(&self) -> u8 {
        u8::from(self.embed)
            + u8::from(self.scope)
            + u8::from(self.effort)
            + u8::from(self.current_info)
    }
}

/// Glassbox provenance attached to an assistant message — how the answer was
/// produced (route, sources, backend, cost). Rendered by the desktop
/// `RoutingMeta` footer; stored in message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseProvenance {
    /// Serialized intent label the turn was routed as.
    pub intent: String,
    /// Free-form label of the retrieval path used (e.g. `"CorpusEngine"`, `"document"`); `None` when nothing was retrieved.
    pub search_method: Option<String>,
    /// Per-corpus retrieval contributions, for the sources line.
    pub sources: Vec<SourceSummary>,
    /// `model_id` of the completion that served the turn — peer-attributed on mesh routes (e.g. `"Qwen3.5-9B @ peer mac-peer"`).
    pub inference_backend: String,
    /// Debug-rendered OICP `match_quality` from the completion's `oicp_meta`; `None` when no OICP metadata was attached.
    pub oicp_match: Option<String>,
    /// Whole-turn wall clock, milliseconds.
    pub total_latency_ms: u64,
    /// Total tokens the turn consumed, as reported by the provider.
    pub tokens_used: usize,
    /// Coarse router classification ("SIMPLE", "LOOKUP", "REASONING", "ACTION").
    /// `None` for old messages that predate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coarse_intent: Option<String>,
    /// Which classifiers were live when this turn was routed. `None` for old
    /// messages; `Some(stamp)` where `stamp.routed_by_none()` means the host
    /// was DEGRADED and the turn is not a measurement. See [`RouterStamp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<RouterStamp>,
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

/// One corpus's contribution to a response — the unit of the provenance sources line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSummary {
    /// Corpus id or origin label the hits came from.
    pub origin: String,
    /// How many retrieved chunks came from this origin.
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
    /// `"thin"` today; `"ok"` reserved (see type doc — fine coverage omits the note entirely).
    pub kind: String,
    /// Chunk-count floor below which a contributing folder counts as thin.
    pub thin_threshold: usize,
    /// The folder corpora that came back thin.
    pub thin_folders: Vec<ThinFolder>,
}

/// One under-served watched-folder corpus in a `CoverageNote`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinFolder {
    /// The folder corpus id.
    pub corpus_id: String,
    /// User-typed folder display name (what the chip shows).
    pub display_name: String,
    /// Chunks this folder contributed to the turn (below `thin_threshold`).
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

/// What the user is asked to approve before a write-effectful tool step runs
/// (see `ApprovalChannel::request_approval`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPreview {
    /// Tool about to run.
    pub tool_id: ToolId,
    /// Human-readable summary of the pending action.
    pub description: String,
    /// The exact params the tool will receive — shown so consent is informed.
    pub params: serde_json::Value,
}

// ─── Insight Types ────────────────────────────────────────────

/// A captured insight node — the output of a clip action.
/// Created when the user clips a paragraph from a conversation response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightNode {
    /// Node id, minted at clip time.
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

/// Provenance of a clipped insight: which corpus/article it surfaced from and the conversation it was clipped in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightSource {
    /// Corpus the source passage came from; `None` when the reply wasn't corpus-grounded.
    pub corpus_id: Option<String>,
    /// Article/document title within the corpus, when known.
    pub article_title: Option<String>,
    /// Conversation the clip was made in.
    pub conversation_id: uuid::Uuid,
}

/// A position badge on an insight (the SEP field-model attribution, e.g. a free-will stance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightPosition {
    /// Position name as displayed on the badge.
    pub name: String,
    /// Badge colour styling.
    pub style: PositionStyle,
}

/// Badge styling for a field-model position. Pre-defined variants carry their own colours.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PositionStyle {
    /// Pre-defined free-will-debate position badge.
    Compatibilism,
    /// Pre-defined free-will-debate position badge.
    HardIncompatibilism,
    /// Pre-defined free-will-debate position badge.
    Libertarianism,
    /// For future field model positions not in the pre-defined set.
    /// Rendered with a neutral gray badge.
    Custom {
        /// CSS background colour.
        bg: String,
        /// CSS text colour.
        text: String,
        /// CSS border colour.
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
        /// Sink that accepted the node.
        sink_id: String,
        /// When the sync completed (UTC).
        synced_at: chrono::DateTime<chrono::Utc>,
    },
    /// Sync attempted but failed.
    SyncFailed {
        /// Sink the push was attempted against.
        sink_id: String,
        /// Why the push failed.
        error: String,
    },
}

#[cfg(test)]
mod router_stamp_tests {
    use super::*;

    /// `routed_by_none` is the question a harness asks before scoring a turn.
    ///
    /// Named failing input (ARCH §18.1), from production: on 2026-08-26 a dead
    /// embed slot left `build_llm_router` returning `None` for all four, and
    /// turns kept answering — SEP overview title-coverage 1.00 -> 0.83, read
    /// as a code regression for most of a session (note `f4972e1b`).
    #[test]
    fn a_router_with_no_live_classifier_says_so() {
        let degraded = RouterStamp::from_liveness(false, false, false, false);
        assert!(degraded.routed_by_none());
        assert_eq!(degraded.live_count(), 0);

        // One live classifier is not "none" — degradation is a degree, and a
        // partial router still routed.
        let partial = RouterStamp::from_liveness(true, false, false, false);
        assert!(!partial.routed_by_none());
        assert_eq!(partial.live_count(), 1);

        let healthy = RouterStamp::from_liveness(true, true, true, true);
        assert!(!healthy.routed_by_none());
        assert_eq!(healthy.live_count(), 4);
    }

    /// `None` on the provenance means "this router does not report", which is
    /// NOT the same fact as a stamp whose classifiers are all false. Collapsing
    /// them would make every stub router look like a degraded host (§18.3).
    #[test]
    fn absent_and_degraded_are_different_values() {
        let absent: Option<RouterStamp> = None;
        let degraded = Some(RouterStamp::default());
        assert_ne!(absent, degraded);
        assert!(degraded.unwrap().routed_by_none());
    }

    /// Old messages have no `router` key and must still deserialize.
    #[test]
    fn the_field_is_backward_compatible() {
        let legacy = r#"{"intent":"SIMPLE","search_method":null,"sources":[],
            "inference_backend":"m","oicp_match":null,"total_latency_ms":1,"tokens_used":2}"#;
        let p: ResponseProvenance = serde_json::from_str(legacy).expect("legacy provenance parses");
        assert_eq!(p.router, None);
    }
}
