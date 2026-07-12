// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from the monolithic types.rs (ARCH §3.2); re-exported by types/mod.rs,
//! so every sovereign_core::types::* import path is unchanged (behaviour-preserving).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use crate::oicp;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

/// A single (intent, confidence) candidate. The classifier emits one
/// primary plus up to a few alternatives.
#[derive(Debug, Clone)]
pub struct IntentCandidate {
    pub intent: Intent,
    /// Confidence in [0.0, 1.0]. A pre-checked heuristic (topic
    /// continuity, content processing, temporal signal) pins this to
    /// 1.0; an LLM Pass 1 returns whatever the model asserts.
    pub confidence: f32,
}

/// Returned by `Router::classify()`. Carries the primary intent, any
/// alternatives the classifier surfaced, and the diagnostic fields
/// that were previously squirreled away in `routing_log` and
/// invisible in the UI.
///
/// `alternatives` is empty in PR1 — the field is reserved for PR2 when
/// the `Ask` move uses a cheap keyword heuristic to suggest clickable
/// disambiguations. Keeping the field here (instead of building it in
/// the runtime) lets future classifiers populate it without a second
/// trait change.
#[derive(Debug, Clone)]
pub struct RouterClassification {
    pub primary: IntentCandidate,
    pub alternatives: Vec<IntentCandidate>,
    /// One-clause justification from the classifier, when available.
    /// Surfaced in the UI for glassbox integrity (ARCH §0.1). `None`
    /// when the classifier is a pre-check or a stub.
    pub rationale: Option<String>,
    /// Raw coarse-classification label: "SIMPLE", "LOOKUP",
    /// "REASONING", "ACTION", or "TOPIC_CONTINUITY" for the override.
    pub coarse_intent: Option<String>,
    /// Self-assessment result — populated only on SIMPLE paths that
    /// went through the gate: "Confident", "Uncertain",
    /// "NeedsWebSearch".
    pub self_assessment: Option<String>,
    /// Iter6: per-stage routing breakdown for performance
    /// instrumentation. None on pure-stub classifiers; populated by
    /// the LLM-backed router so the runtime can roll the slice into
    /// the response metrics.
    pub timing: Option<RoutingTiming>,
    /// Optional scope hint sourced from the nearest router exemplar.
    /// Orthogonal to `primary.intent`; consumed downstream by
    /// retrieval to bias corpus selection. Today's only value is
    /// `Some("personal")` — set when the matched exemplar is tagged
    /// with `scope = "personal"` in `sovereign/router/exemplars.toml`
    /// (conversation-history / personal-vault shapes). `None` =
    /// no scope hint (current default), retrieval uses every
    /// installed knowledge corpus.
    pub scope: Option<String>,
}

/// Iter6: per-call routing latency slice. Surfaces the cost of the
/// pre-check chain vs the LLM Pass 1 vs the parse step so the
/// 14% / 6s routing slice from the iter5 waterfall can be
/// diagnosed concretely.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingTiming {
    /// Wall-clock time spent walking the heuristic pre-check chain
    /// (force_conation, force_action, force_metalingual,
    /// force_commissive, force_comparison, force_expressive_short,
    /// force_expressive_memref, force_content_reasoning, force_deep).
    /// Sub-millisecond when none fire; instant when any short-circuits
    /// because we stop walking once the first fires.
    pub precheck_ms: u64,
    /// LLM Pass 1 call time (`classify_call_json`). Zero when a
    /// pre-check fired and the LLM call was skipped.
    pub llm_ms: u64,
    /// `parse_coarse` step. Should be sub-millisecond — included for
    /// completeness so the three slices sum to the router's total.
    pub parse_ms: u64,
    /// Whether the LLM Pass 1 actually fired. False = a pre-check
    /// short-circuited; True = `classify_call_json` ran.
    pub used_llm: bool,
}

/// Which of the three antifragile moves the runtime should take.
///
/// - `Commit`: proceed directly. No banner, no prompt. Default.
/// - `Propose`: stream a response AND show the interpretation banner
///   so the user can cheaply redirect. PR2 wires the UI; PR1 never
///   returns this variant.
/// - `Ask`: suppress synthesis and surface a clarification card. PR2
///   wires the UI; PR1 never returns this variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveKind {
    Commit,
    Propose,
    Ask,
}

/// Bucketed confidence tier. Derived from `primary.confidence` and
/// the active `ConfidenceThresholds`. Kept as an enum (ARCH §2.1) so
/// downstream glassbox rendering is stringly-typed only at the
/// serialization boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceTier {
    High,
    Moderate,
    Low,
}

/// Thresholds consulted by `decide_policy`. Defaults err toward
/// committing so first-time users see a responsive system; the
/// "Propose" move activates in the moderate band where the
/// interpretation banner adds value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceThresholds {
    /// confidence ≥ high  → `ConfidenceTier::High`  / `MoveKind::Commit`
    pub high: f32,
    /// high > confidence ≥ moderate → `ConfidenceTier::Moderate` / `MoveKind::Propose`
    /// moderate > confidence        → `ConfidenceTier::Low`      / `MoveKind::Ask`
    pub moderate: f32,
}

impl Default for ConfidenceThresholds {
    fn default() -> Self {
        Self {
            high: 0.80,
            moderate: 0.55,
        }
    }
}

/// Runtime-side policy: what we're actually going to do with the
/// classifier's opinion. Pure function of `RouterClassification` +
/// `ConfidenceThresholds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub move_kind: MoveKind,
    pub tier: ConfidenceTier,
    /// Snapshot of the thresholds used to produce this decision.
    /// Surfaced in glassbox metadata so users and the operator log
    /// can see why the router picked what it picked (ARCH §0.1).
    pub thresholds_used: ConfidenceThresholds,
}

/// Which substantive phase a narration entry marks. Serialized to
/// the UI so narration chips can carry an icon per phase (retrieval,
/// routing, synthesis, etc.). Extend additively; the UI should
/// fallback gracefully for unknown variants (via `#[serde(other)]`
/// on the consuming side).
///
/// Two families coexist:
///
/// - **Legacy single-stage variants** (`RoutingCommitted`,
///   `PrimarySynthesisStart`, `GapCheckFired`, `RetrievalComplete`)
///   were emitted by the pre-team-pipeline dispatch path. They are
///   kept so existing callers and tests work unchanged.
/// - **Team-pipeline stage frames** (`RoutingStart` /
///   `RoutingComplete`, `RetrievalStart`, `CurationStart` /
///   `CurationComplete`, `DraftingStart` / `DraftingComplete`,
///   `PresentationStart` / `PresentationComplete`, `StageError`)
///   are emitted by the five-stage pipeline introduced by the
///   situated-team plan. The desktop renders each as an inline
///   chip; payloads label the chip ("Curated 5 of 18 chunks").
///
/// `RetrievalComplete` was migrated from a unit variant to a struct
/// variant — emit sites must now supply `chunks_in` and `corpora`.
/// The Copy derive was dropped because struct variants with `String`
/// / `Vec` payloads cannot be Copy; all known consumers move or
/// clone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NarrationPhase {
    // ── Legacy (single-stage) variants ────────────────────────
    /// Routing committed, substantive work about to begin.
    RoutingCommitted,
    /// Primary-slot synthesis beginning (Slow path).
    PrimarySynthesisStart,
    /// Gap-check fired and found a missing piece.
    GapCheckFired,
    /// Grounding gate is verifying the drafted answer against the
    /// retrieved passages before releasing it (the stream is held).
    GroundingVerifyStart,
    /// A durative coaching turn produced a draft lesson card
    /// (TEACHABLE P0). The chip tethers into the Learn-this card the
    /// same way `GapCheckFired` tethers into the information-request
    /// card.
    LessonDrafted,

    // ── Team-pipeline stage frames ────────────────────────────
    /// Router invocation began. Pairs with `RoutingComplete`.
    RoutingStart,
    /// Router classified the turn. Carries the verdict so the
    /// desktop can label the stage chip.
    RoutingComplete {
        intent: String,
        register: String,
        confidence: f32,
    },
    /// Retriever began (vector + FTS + atlas).
    RetrievalStart,
    /// Retrieval finished. Carries shape so the chip can read
    /// e.g. "Read 12 chunks across [sep, wikipedia]". Migrated
    /// from the legacy unit variant; on the wire this is a struct
    /// variant under `#[serde(rename_all = "snake_case")]`.
    RetrievalComplete {
        chunks_in: usize,
        corpora: Vec<String>,
    },
    /// Curator began (Fast slot, structured output).
    CurationStart,
    /// Curator finished. `chunks_kept` is the number that
    /// survived curation; `skeleton` is the ordered list of
    /// section labels the Drafter will fill; `sufficient` is the
    /// glass-box honesty signal — `false` short-circuits the
    /// Drafter and routes the Presenter to an honest "I don't
    /// have grounding for this" message.
    CurationComplete {
        chunks_kept: usize,
        skeleton: Vec<String>,
        sufficient: bool,
    },
    /// Drafter began (Primary slot).
    DraftingStart,
    /// Drafter finished. `tokens` is `completion_tokens`;
    /// `finish_reason` is the OpenAI-style `stop` / `length` /
    /// `cancelled` / `error`, sourced from the typed
    /// `StreamFrame::Finish` introduced in the Phase 1.1 plumbing.
    DraftingComplete { tokens: u32, finish_reason: String },
    /// Presenter began (Fast slot, voice-shaping pass).
    PresentationStart,
    /// Presenter finished. `judge_score` is the optional
    /// post-presentation voice-judge score (None when register
    /// is Factual or the judge is disabled). Arrives on a
    /// delayed narration frame from the async judge task.
    PresentationComplete { judge_score: Option<u8> },
    /// Any stage emitted an error. The pipeline records this for
    /// telemetry; user-facing messaging is decided per stage.
    StageError { stage: String, error: String },

    // ── Tool-invocation frames (table-stakes "Searching for X…" UX) ──
    //
    // Unlike the pipeline-stage frames above (Routing → Retrieval →
    // Curation → Drafting → Presentation, which fire at most once each),
    // tool invocations can fan out — a single turn may call web_search +
    // knowledge_search in parallel, then web_fetch on a follow-up. The
    // `call_id` correlates Start with Complete so the desktop can resolve
    // out-of-order arrivals back into per-call cards.
    //
    // These frames intentionally bypass the 3-event narration cap and
    // 5s-elapsed suppression in `QuerySession`: the user needs to see
    // tool activity *immediately* (within 200ms) for the "feels alive"
    // contract. Emit via `emit_turn_narration` directly, not via
    // `try_emit_narration`.
    /// A tool call has started. `tool_id` is the canonical id
    /// (`web_search`, `knowledge_search`, `web_fetch`, `document`, etc.);
    /// `summary` is a one-line user-facing description ("Searching the
    /// web for *quantum entanglement*", "Reading docs.python.org") that
    /// the desktop chip can render without re-interpreting tool args.
    ToolInvocationStart {
        call_id: String,
        tool_id: String,
        summary: String,
    },
    /// A tool call has finished. `ok` distinguishes success (chip turns
    /// done-coloured) from failure (chip turns muted, paired with the
    /// graceful-failure prompt rule). `result_summary` is a short
    /// user-facing outcome ("Retrieved 4 results", "No matches found",
    /// "404 Not Found") — never the raw tool output.
    ToolInvocationComplete {
        call_id: String,
        tool_id: String,
        ok: bool,
        result_summary: String,
    },

    // ── Live synthesis heartbeat ──────────────────────────────
    /// The grounding gate holds every token until the drafted answer
    /// is verified, so on a slow model there's a long window where the
    /// answer is forming but nothing streams. This heartbeat carries
    /// the running token COUNT (never the held content) so the desktop
    /// can show the answer growing — "writing… 142 tokens" ticking up.
    /// Emitted repeatedly (throttled ~250ms) via `emit_turn_narration`
    /// directly — it bypasses the cap/suppression like the tool frames,
    /// and the desktop REPLACES rather than appends it (one live chip,
    /// not a log entry). Cleared when the turn's terminal arrives.
    SynthesisProgress { tokens: u32 },

    /// EXPERIMENT (`SOVEREIGN_DRAFT_STREAM=1`): live DRAFT text preview.
    /// Streams the unverified draft's incremental text during the gated
    /// hold so the desktop can render a visually-PROVISIONAL section
    /// ("drafting — verifying…", thinking-section style) that collapses
    /// when the gated answer arrives through the normal message stream.
    /// Release semantics are UNCHANGED — message-chunks still emit only
    /// after the gate verdict; this channel is additive perception, and
    /// the affordance contract is that draft text must never be styled
    /// as final. Throttled with the SynthesisProgress cadence; `delta`
    /// is the text appended since the previous frame. TTFT-perceived
    /// (first draft glyphs) drops to draft latency while the official
    /// TTFT metric keeps measuring the gated stream honestly.
    DraftDelta { delta: String },
}

/// One narration entry emitted in the model's voice during a long
/// turn. Accumulated in `QuerySession.narration` and streamed to the
/// UI as `turn-narration` Tauri events. PR2 emits these at
/// phase-boundary points; suppression < 5s total elapsed and a
/// 3-event cap keep the channel from polluting short turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrationEvent {
    pub phase: NarrationPhase,
    pub text: String,
    /// Milliseconds since turn start. Drives UI timeline rendering.
    pub elapsed_ms: u64,
}

// ─── Antifragile-routing UI event payloads ───────────────────

/// Emitted by the runtime when `decide_policy` picks `MoveKind::Propose`.
/// The UI renders an inline banner above the streaming message with
/// the `interpretation` text plus `alternatives` as redirect chips.
/// The banner persists through the turn; redirect stays cheap while
/// tokens are flowing (sampler cancels) and remains valid afterward
/// (full session retained for 30s).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationProposed {
    pub session_id: String,
    pub conversation_id: String,
    /// One-sentence statement of how the router read the input, e.g.
    /// "I'm reading this as a quick overview of the scheduler."
    pub interpretation: String,
    /// Ranked candidate interpretations the user can click to
    /// redirect. Drawn from `RouterClassification.alternatives`.
    pub alternatives: Vec<ProposedAlternative>,
    /// Confidence number for glassbox rendering (ARCH §0.1).
    pub confidence: f32,
}

/// One redirect option on an `InterpretationProposed` banner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAlternative {
    /// UI-facing label, e.g. "Walk me through the scoring function".
    pub label: String,
    /// Serialized `Intent` variant ("deep_query", "knowledge_query",
    /// etc.) the runtime will route to on redirect. Using a string
    /// rather than the full `Intent` enum here keeps the desktop
    /// payload simple; the runtime re-resolves on `redirect_turn`.
    pub intent_hint: String,
}

/// Emitted by the runtime when `decide_policy` picks `MoveKind::Ask`.
/// The UI renders a ClarificationCard with `options` as clickable
/// chips plus a free-text fallback. Synthesis is suppressed —
/// nothing streams until the user responds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationRequest {
    pub session_id: String,
    pub conversation_id: String,
    /// The question to show above the options, e.g. "I can approach
    /// this a few ways — are you trying to understand how it works,
    /// design changes to it, or debug it?"
    pub question: String,
    pub options: Vec<ClarificationOption>,
}

/// One clickable disambiguation on a `ClarificationRequest` card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationOption {
    pub label: String,
    /// The follow-up message that will be sent back as if the user
    /// had typed it. The runtime correlates to the session via a
    /// session_ref and skips routing.
    pub follow_up: String,
    pub intent_hint: String,
}

/// Emitted by the runtime at phase-boundary points on long turns.
/// Rendered as inline model-voice chips in the UI (see
/// `NarrationChip.svelte`). Capped at 3 per turn; suppressed when
/// turn elapsed < 5s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnNarration {
    pub session_id: String,
    pub conversation_id: String,
    pub event: NarrationEvent,
}

/// Wire parameters carrying a "continue this earlier turn" request
/// from the UI back into the runtime. Produced when the user clicks
/// a ClarificationCard option or a NextStepOffer button.
///
/// The runtime uses this to:
///   - skip router classification (the intent was already picked),
///   - correlate with the prior `QuerySession` (PR2c will also reuse
///     the cached retrieval from that session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeSession {
    pub session_id: String,
    /// Wire-form `Intent` hint produced by `intent_hint()` in the
    /// runtime. Parsed back via `parse_intent_hint`. Unknown or
    /// malformed hints fall back to `Intent::SimpleQuery` so the
    /// session-continuation path never hard-fails from a typo.
    pub intent_hint: String,
}

// ─── Next-step offers (PR3) ──────────────────────────────────
//
// After a substantive KnowledgeQuery turn finishes, the runtime
// surfaces up to two grounded follow-up actions the user can click.
// Offers are:
//
//   1. *grounded* — derived from what retrieval actually found (not
//      a generic "anything else?" prompt), and
//   2. *cheap* — when `session_ref` is live (<30s from completion),
//      clicking reuses the session via `resume_session_stream` and
//      skips router classification.
//
// SimpleQuery / DeepQuery don't emit offers today — they have no
// retrieval grounding to draw on. Extend here if future intents
// produce meaningful follow-ups.

/// One clickable next-step chip on a completed assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NextStepOffer {
    /// UI button text. Short, action-shaped: "Tell me about X",
    /// "Compare other perspectives", "Go deeper on Y".
    pub label: String,
    /// Optional subtle hint rendered as a tooltip or below-label
    /// caption. Good place for "from <source_title>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The query that actually gets submitted if clicked. Usually
    /// a rephrased version of the offer, ready for synthesis.
    pub follow_up_query: String,
    /// Live `QuerySession.id` the runtime should resume against. The
    /// session's 30s retention window means a click more than 30s
    /// after render silently falls back to a fresh turn (runtime
    /// will return `session not found` → the UI must gracefully
    /// degrade to `send_message_stream`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    /// Wire-form `Intent` hint for the resume path (see
    /// `ResumeSession.intent_hint`). When `None`, the follow-up is
    /// treated as a fresh message that re-runs classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_hint: Option<String>,
}

/// Input to the offer generator. Decouples the pure function from
/// the specifics of the streaming pipeline's internal types so the
/// generator is trivially unit-testable.
#[derive(Debug, Clone)]
pub struct OfferContext<'a> {
    /// The user's original message — used to phrase a drill-down
    /// follow-up ("Tell me more about X in the context of this
    /// question").
    pub user_message: &'a str,
    /// Title of the source chunk that most shaped the answer. Used
    /// to offer "Compare other perspectives" when this source
    /// dominated retrieval.
    pub top_source_title: Option<&'a str>,
    /// Did the answer concentrate on one source? (Shape's
    /// `top_source_repeat_count >= 2`.) Governs whether the
    /// "compare perspectives" offer is worth surfacing.
    pub had_dominant_source: bool,
    /// Retrieved chunks in score order. The generator picks the
    /// highest-scoring one whose title differs from
    /// `top_source_title` as a drill-down target.
    pub retrieved_chunks: &'a [serde_json::Value],
    /// Live session id the UI should pass on click to take the
    /// cheap resume-session path.
    pub session_id: &'a str,
    /// PR5 — was the underlying retrieval off-target (dispersed
    /// noise, no title match, no source concentration)? When true,
    /// the generator returns zero offers: drilling down into
    /// irrelevant retrieval doubles down on the miss. Source of
    /// truth is `EvidenceShape::is_off_target()` in the runtime.
    pub retrieval_missed: bool,
}

/// Produce up to two grounded next-step offers from a completed
/// KnowledgeQuery turn. Pure — no I/O.
///
/// Offer priority:
///   1. Drill-down into the highest-scoring non-dominant source
///      (when one exists).
///   2. "Compare other perspectives" (when the answer concentrated
///      on a single dominant source).
pub fn build_next_step_offers(ctx: &OfferContext<'_>) -> Vec<NextStepOffer> {
    // PR5 — suppress offers entirely when retrieval was off-target.
    // Drilling into "Cartoon Reel" after asking about "Commonwealth
    // scheduler" doubles down on noise; better to surface nothing
    // than to surface misdirecting chips.
    if ctx.retrieval_missed {
        return Vec::new();
    }

    let mut offers = Vec::new();

    // Drill-down: find the first retrieved chunk whose title is
    // meaningfully different from the dominant one. Skip entries
    // without titles (conversation-history chunks, etc.).
    if let Some(secondary_title) = ctx.retrieved_chunks.iter().find_map(|c| {
        let title = c.get("title")?.as_str()?;
        if title.is_empty() {
            return None;
        }
        if let Some(dominant) = ctx.top_source_title {
            if title.eq_ignore_ascii_case(dominant) {
                return None;
            }
        }
        Some(title.to_string())
    }) {
        offers.push(NextStepOffer {
            label: format!("Tell me about \"{secondary_title}\""),
            description: Some("Drawn from your retrieval".to_string()),
            follow_up_query: format!("Tell me what \"{secondary_title}\" says about this."),
            session_ref: Some(ctx.session_id.to_string()),
            intent_hint: Some("knowledge_query".to_string()),
        });
    }

    // Dominant-source → offer a comparative read.
    if ctx.had_dominant_source {
        if let Some(dominant) = ctx.top_source_title {
            let dominant_trunc = if dominant.len() > 40 {
                format!("{}…", &dominant[..40])
            } else {
                dominant.to_string()
            };
            offers.push(NextStepOffer {
                label: "Compare other perspectives".to_string(),
                description: Some(format!(
                    "Your answer leaned on \"{dominant_trunc}\" — pull in more sources."
                )),
                follow_up_query: format!(
                    "{} — what do other sources in my knowledge base say, besides \"{dominant}\"?",
                    ctx.user_message.trim()
                ),
                session_ref: Some(ctx.session_id.to_string()),
                intent_hint: Some("knowledge_query".to_string()),
            });
        }
    }

    // Cap at 2. If a future trigger produces a third, we want a
    // hard limit — three buttons under every answer is clutter.
    offers.truncate(2);
    offers
}

/// Map classification confidence to a concrete (tier, move_kind)
/// decision. Pure — no I/O, no awaits, no model calls. PR1 only
/// ever reaches the `Commit` branch in the runtime dispatcher; the
/// other branches are precomputed here so PR2 can wire them without
/// a second types-layer change.
pub fn decide_policy(
    classification: &RouterClassification,
    thresholds: &ConfidenceThresholds,
) -> RoutingPolicy {
    let c = classification.primary.confidence;
    let (tier, move_kind) = if c >= thresholds.high {
        (ConfidenceTier::High, MoveKind::Commit)
    } else if c >= thresholds.moderate {
        (ConfidenceTier::Moderate, MoveKind::Propose)
    } else {
        (ConfidenceTier::Low, MoveKind::Ask)
    };
    RoutingPolicy {
        move_kind,
        tier,
        thresholds_used: *thresholds,
    }
}
