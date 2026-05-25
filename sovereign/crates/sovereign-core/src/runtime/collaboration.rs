//! Collaboration / refinement / Ask-move plumbing.
//!
//! Three concerns live here:
//!
//! 1. **`run_collaboration`** — the gap-check + refinement core. Called
//!    by `Runtime::maybe_collaborate` and by the spawned streaming
//!    post-processor; factoring it out as a free function lets the
//!    spawn invoke it with owned `Arc`s instead of `&self`.
//!
//! 2. **`run_post_stream_refinement`** — wraps `run_collaboration` with
//!    the persisted-message rewrite + `message-refined` event emit.
//!    Same factoring rationale — the streaming spawn doesn't hold a
//!    live `&Runtime`.
//!
//! 3. **`emit_ask_deliberation_chip`** + `ASK_MOVE_DELIBERATION_LINGER_MS`
//!    — the Ask-move narration cue that fires before the clarification
//!    card. Pure helper shared by the streaming and non-streaming Ask
//!    handlers.
//!
//! The `ContradictionCheck` DTO also lives here because it's the shape
//! `run_collaboration`'s sibling, `Runtime::detect_contradiction`,
//! decodes from the Pass-A witness check before invoking refinement.

use std::sync::Arc;

use crate::traits::{ApprovalChannel, InferenceProvider, RoutingEventSink, StateStore};
use crate::types::*;

use super::text_utils::now;

/// Linger time after the Ask-move deliberation chip is emitted,
/// before the clarification card itself lands. The Ask path runs
/// in milliseconds — without this the chip and the card race to
/// the UI in the same frame and the chip never registers. 400ms
/// is the empirical sweet spot: long enough to read "I'm not sure
/// — let me ask," short enough not to feel like the system is
/// stalling.
pub(crate) const ASK_MOVE_DELIBERATION_LINGER_MS: u64 = 400;

/// Pass-A output from the multi-shot witness synthesis. See
/// `Runtime::detect_contradiction` for the full design rationale —
/// short version: this is the structured "is there a clear factual
/// tension between current message and prior memories?" check that
/// runs before the witness reply, so the synthesis prompt can
/// include explicit prior_evidence instead of relying on the model
/// to find and surface it in one shot.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct ContradictionCheck {
    pub(crate) contradiction: bool,
    pub(crate) prior_evidence: String,
    #[serde(default)]
    pub(crate) current_claim: String,
}

/// Shared body of [`super::Runtime::maybe_collaborate`]. Factored out so the
/// streaming spawn (which doesn't hold a live `&self`) can invoke the
/// same logic via owned `Arc`s. See the method's doc comment for
/// behaviour; this function is called whether or not `auto_collaborate`
/// is enabled — it no-ops when disabled.
pub(crate) async fn run_collaboration(
    inference: &dyn InferenceProvider,
    approval: &dyn ApprovalChannel,
    inference_config: &InferenceConfig,
    conversation_id: &str,
    question: &str,
    response: &str,
    evidence: &str,
    // Optional narration channel: when both `routing_events` and
    // `session_id` are `Some`, surface "checking for gaps" /
    // "found a gap" chips alongside the gap-check work so the user
    // sees the deliberation that produces the INFORMATION REQUEST
    // instead of having the card pop in 30–60s after the answer
    // with no warning. Both `None` = silent (back-compat path for
    // synchronous callers without a live streaming session).
    routing_events: Option<Arc<dyn RoutingEventSink>>,
    session_id: Option<String>,
) -> String {
    if !inference_config.auto_collaborate {
        return response.to_string();
    }

    let t_start = std::time::Instant::now();

    // Glassbox chip: "I drafted, now I'm auditing the answer."
    // Emitted before `identify_gap` because that call can run
    // for tens of seconds on grammar-constrained Fast-slot
    // inference and the user is otherwise staring at a finished
    // answer wondering if anything else is happening. Bypasses
    // `try_emit_narration` — the session may already be past the
    // 30s retention window by the time gap-check fires, and the
    // chip's value is highest precisely on long turns.
    if let (Some(events), Some(sid)) = (routing_events.as_ref(), session_id.as_ref()) {
        events
            .emit_turn_narration(TurnNarration {
                session_id: sid.clone(),
                conversation_id: conversation_id.to_string(),
                event: NarrationEvent {
                    phase: NarrationPhase::GapCheckFired,
                    text:
                        "Drafted. Auditing the answer for anything worth asking you about."
                            .to_string(),
                    elapsed_ms: 0,
                },
            })
            .await;
    }

    // 1. Ask the gap-identifier whether anything external would sharpen
    //    the answer. Conservative on any error — we never want this
    //    hook to fail the turn.
    let gap = match crate::gap::identify_gap(inference, question, response, evidence).await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::info!(
                latency_ms = t_start.elapsed().as_millis() as u64,
                "maybe_collaborate: no gap identified — passing through"
            );
            return response.to_string();
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "maybe_collaborate: gap check failed — passing through"
            );
            return response.to_string();
        }
    };

    // 2. Stamp task/step on the request so the UI can correlate it
    //    with the current conversation. Force kind = Refinement here
    //    even though gap.rs already sets it — this is the single point
    //    where the runtime owns the contract that "anything coming out
    //    of run_collaboration is post-answer refinement," independent
    //    of what the gap-checker chose to put on the wire.
    let mut req = gap;
    req.task_id = conversation_id.to_string();
    req.step_id = 0;
    req.kind = InformationRequestKind::Refinement;
    req.task_title.clear();

    tracing::info!(
        gap_chars = req.gap.len(),
        "maybe_collaborate: surfacing information request"
    );

    // Glassbox chip: "I found something worth asking — here it
    // comes." Lands ~immediately before the INFORMATION REQUEST
    // card, mirroring the Ask-move chip-then-card pattern. The
    // gap text itself isn't surfaced in the chip — it'd duplicate
    // the card content the user is about to read.
    if let (Some(events), Some(sid)) = (routing_events.as_ref(), session_id.as_ref()) {
        events
            .emit_turn_narration(TurnNarration {
                session_id: sid.clone(),
                conversation_id: conversation_id.to_string(),
                event: NarrationEvent {
                    phase: NarrationPhase::GapCheckFired,
                    text: "Found something worth asking about — preparing the question."
                        .to_string(),
                    elapsed_ms: 0,
                },
            })
            .await;
    }

    // 3. Surface the card and wait for the user.
    let user_content = approval.request_information(&req).await;
    let content = match user_content {
        Some(c) if !c.trim().is_empty() => c,
        _ => {
            tracing::info!(
                latency_ms = t_start.elapsed().as_millis() as u64,
                "maybe_collaborate: user skipped or provided no content"
            );
            return response.to_string();
        }
    };

    // 4. Refinement synthesis — integrate the user's source. The prompt
    //    asks the model to distinguish corpus-derived content from
    //    user-provided content so provenance stays visible.
    //
    // The system message anchors today's date and instructs the model
    // to compare source-dates to "now" — critical because user-supplied
    // sources (especially web-search results) frequently include older
    // articles that PREDICT events the model would otherwise present as
    // still future. Surfaced 2026-05-19 M5-Mac-Studio session: model
    // refined with 2024-era rumor articles predicting "early 2026"
    // launches and presented those predictions as forecasts even
    // though it was already May 2026. Date-reasoning discipline is
    // shape-level per `feedback_no_teaching_to_test.md`.
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let refine_system = format!(
        "Current date: {today}. When integrating user-provided \
         sources, compare their publication date or context to \
         today's date. If a source predicts an event for a date \
         that has already passed, do NOT present the prediction as \
         still future — either the event happened (look for more \
         recent confirming evidence before asserting it) or the \
         prediction was wrong (say so). Sources that are silent on \
         dates may still be stale; flag uncertainty when the answer \
         depends on time-sensitive facts."
    );
    let refine_prompt = format!(
        "The user asked: {question}\n\n\
         Your initial answer (drawn from the local corpus):\n{response}\n\n\
         Additional source the user provided:\n{content}\n\n\
         Refine the answer to integrate the user's source. Be explicit \
         about what came from the corpus vs. what came from the user's \
         source. Mark anything that remains uncertain — especially \
         claims that hinge on dates that may now be in the past."
    );

    let refine_req = CompletionRequest {
        prompt: refine_prompt,
        system_message: Some(refine_system),
        preferred_speed: Speed::Slow,
        max_tokens: Some(inference_config.max_tokens),
        temperature: Some(inference_config.temperature),
        think_budget: Some(inference_config.think_budget),
        structured_output: None,
        top_k: inference_config.top_k,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
                    model_id: None,
                    enable_thinking: None,
    sampling_mode: None,
    assistant_prefix: None,
    cmd_prefix: None,
    url_allowlist: None,
    evidence_id_allowlist: None,
    lark_grammar: None,
    };

    match inference.complete(&refine_req).await {
        Ok(c) => {
            tracing::info!(
                had_user_content = true,
                latency_ms = t_start.elapsed().as_millis() as u64,
                refined_chars = c.text.len(),
                "maybe_collaborate: refined answer produced"
            );
            c.text
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "maybe_collaborate: refinement inference failed — falling back to original"
            );
            response.to_string()
        }
    }
}

/// Post-stream refinement primitive: run the gap check and, if the
/// user provides content, overwrite the saved assistant message and
/// emit `message-refined`. Called both from `handle_message_stream`'s
/// spawn (which has owned `Arc`s but no live `&self`) and from the
/// corresponding method on `Runtime`.
pub(crate) async fn run_post_stream_refinement(
    inference: &dyn InferenceProvider,
    approval: &dyn ApprovalChannel,
    store: &dyn StateStore,
    inference_config: &InferenceConfig,
    conversation_id: &str,
    message_id: &str,
    question: &str,
    original_content: &str,
    evidence: &str,
    original_metadata: Option<serde_json::Value>,
    // Optional narration channel — see `run_collaboration` for
    // the contract. Spawns from the streaming path pass `Some`
    // so the user sees gap-check progress chips; non-streaming
    // / test callers pass `None`.
    routing_events: Option<Arc<dyn RoutingEventSink>>,
    session_id: Option<String>,
) -> Option<String> {
    let refined = run_collaboration(
        inference,
        approval,
        inference_config,
        conversation_id,
        question,
        original_content,
        evidence,
        routing_events,
        session_id,
    )
    .await;
    if refined == original_content {
        return None;
    }

    let updated = Message {
        id: message_id.to_string(),
        conversation_id: conversation_id.to_string(),
        role: Role::Assistant,
        content: refined.clone(),
        created_at: now(),
        metadata: original_metadata,
        version: now(),
    };
    if let Err(e) = store.save_message(&updated).await {
        tracing::warn!(
            error = %e,
            message_id = %message_id,
            "post-stream refinement: save_message failed"
        );
        return None;
    }

    approval.emit_message_refined(MessageRefinedPayload {
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        new_content: refined.clone(),
    });
    Some(refined)
}

/// Emit a "system is deliberating, about to ask" narration chip
/// before the clarification card lands. Bypasses
/// `try_emit_narration` because the Ask path runs in milliseconds
/// and would always be suppressed by the `NARRATION_MIN_ELAPSED`
/// gate — the whole point of the chip here is to fire fast and
/// give the user a glassbox cue that the system chose to ask
/// rather than guess. Pure helper, no `&self`, so both the
/// streaming and non-streaming Ask handlers can share it.
pub(crate) async fn emit_ask_deliberation_chip(
    routing_events: &dyn RoutingEventSink,
    session_id: &str,
    conversation_id: &str,
    classification: &RouterClassification,
) {
    // Three buckets, keyed off how many alternatives the
    // classifier surfaced. With ≥2 we know multiple intents
    // scored close; with 1 the model was on the fence; with 0 we
    // landed in Ask via low confidence on the primary alone (the
    // clarification card pads with a free-text option).
    let chip_text = match classification.alternatives.len() {
        0 => "I'm not quite sure how to read this — let me ask before I guess.".to_string(),
        1 => "On the fence about how to read this — about to ask.".to_string(),
        n => format!(
            "I see {} ways to read this — picking the most useful follow-up.",
            n + 1
        ),
    };
    let event = NarrationEvent {
        phase: NarrationPhase::RoutingCommitted,
        text: chip_text,
        elapsed_ms: 0,
    };
    routing_events
        .emit_turn_narration(TurnNarration {
            session_id: session_id.to_string(),
            conversation_id: conversation_id.to_string(),
            event,
        })
        .await;
}
