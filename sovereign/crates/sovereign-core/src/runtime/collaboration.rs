// SPDX-License-Identifier: AGPL-3.0-or-later
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

use tokio_util::sync::CancellationToken;

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

/// Outcome of a `run_collaboration` invocation. Distinguishes the
/// "refinement never reached inference" cases (no gap, user skipped,
/// auto-collab off) from the "inference ran" cases (succeeded,
/// produced no change, or errored). Callers need this distinction
/// because the frontend's `m.refining` flag is set the moment the
/// user clicks Submit or Search on the InformationRequestCard
/// (`onRefiningStarted`); the backend must emit `message-refined`
/// to clear that flag whenever refinement was attempted, regardless
/// of whether the answer changed or the inference crashed.
///
/// Without this distinction, `run_post_stream_refinement` short-
/// circuits on `refined == original_content` and the UI stays stuck
/// on "Refining your answer" forever.
pub(crate) enum RefinementOutcome {
    /// Refinement was never attempted. Either auto-collaborate is
    /// disabled, the gap-check found no gap (or errored), or the
    /// user dismissed the card without providing content. The
    /// frontend never set `m.refining = true` on this path —
    /// `handleSkip` in `InformationRequestCard.svelte` does not
    /// fire `onRefiningStarted`. No emit needed.
    NotAttempted,
    /// User provided content; refinement inference ran and produced
    /// new text that differs from the original. Carries the user's
    /// content alongside the rewrite because the refinement re-gate
    /// must verify against the corpus evidence PLUS this source: the
    /// rewrite legitimately contains facts from it (that is the whole
    /// point of the affordance), and verifying against corpus-only
    /// evidence rejected every genuinely-new web rescue (measured
    /// 3/3 reverted, persona-QA 2026-07-10).
    Refined { text: String, user_content: String },
    /// User provided content; refinement inference ran but produced
    /// output identical to the original answer. Frontend's refining
    /// flag must clear — caller emits `message-refined` with the
    /// original content.
    NoChange,
    /// User provided content; refinement inference errored. Frontend's
    /// refining flag must clear — caller emits `message-refined` with
    /// the original content. `error` is the formatted error string
    /// for telemetry / narration; it is NOT shown verbatim in the
    /// chat bubble.
    Failed { error: String },
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
    // Gap DETECTION signal (I4-C retirement, bench/gap_check/DECISION.md):
    // the turn's ledger abstention — the same gate signal `TurnVerdict::
    // CannotKnowFromHere` derives from (D3). Replaces `gap.rs`'s post-hoc
    // LLM judge, which scored 12/12-equivalent to this deterministic
    // detector on the fixture bank while paying a 15-55s fast-slot call
    // on EVERY answered turn. `false` = no gap card, pass through.
    abstained: bool,
    // Optional narration channel: when both `routing_events` and
    // `session_id` are `Some`, surface "checking for gaps" /
    // "found a gap" chips alongside the gap-check work so the user
    // sees the deliberation that produces the INFORMATION REQUEST
    // instead of having the card pop in 30–60s after the answer
    // with no warning. Both `None` = silent (back-compat path for
    // synchronous callers without a live streaming session).
    routing_events: Option<Arc<dyn RoutingEventSink>>,
    session_id: Option<String>,
    // TEACHABLE P0: the turn's K=1 compiled prompt lesson. When Some,
    // it is re-appended to the refinement system message (today-anchor
    // precedent: injected into system AND refinement prompts) so a
    // style lesson survives a gap-check rewrite. `None` = no lesson /
    // legacy callers — byte-identical behavior.
    lesson_prompt: Option<String>,
    // Post-stream preemption: cancelled by the NEXT user turn on the
    // same conversation (`Runtime::post_stream_preemption`). Checked
    // at STEP BOUNDARIES only — an in-flight inference call can't be
    // interrupted (v1), but the remaining steps are skipped so user
    // turns never queue behind stale housekeeping. Observed failure
    // this fixes: coach A/B 2026-07-11, session-B turn 3 died with a
    // 150s dispatch AbortError queued behind turn 2's refinement on
    // the fast slot. `None` = legacy callers, never preempted.
    preempt: Option<CancellationToken>,
    // Acquisition-route resolution context (EPISTEMIC_STATE.md §4.3):
    // an engine handle for the recipe catalog + installed diff, plus
    // the turn's coverage verdict when a probe ran. `None` = no route
    // resolution (legacy callers) — the card renders as before.
    route_ctx: Option<crate::runtime::acquisition::RouteContext>,
) -> RefinementOutcome {
    if !inference_config.auto_collaborate {
        return RefinementOutcome::NotAttempted;
    }
    let preempted = |stage: &str| {
        let hit = preempt.as_ref().is_some_and(|t| t.is_cancelled());
        if hit {
            tracing::info!(
                target: "post_stream",
                stage,
                "post_stream: preempted by a newer turn — skipping remaining refinement steps"
            );
        }
        hit
    };
    if preempted("entry") {
        return RefinementOutcome::NotAttempted;
    }

    let t_start = std::time::Instant::now();

    // 1. Detection is STRUCTURAL (I4-C retirement): the card fires
    //    exactly when the turn abstained — the signal the ledger's
    //    `CannotKnowFromHere` verdict derives from. No LLM judges
    //    whether a gap exists; answered turns pass through instantly
    //    (the ledger still records minor uncovered facets as gap rows,
    //    without a card). This deleted `gap.rs`'s per-turn 15-55s
    //    grammar-constrained fast-slot audit.
    if !abstained {
        tracing::debug!("maybe_collaborate: turn answered — no gap card");
        return RefinementOutcome::NotAttempted;
    }

    // Glassbox chip: the turn came up short and the system is preparing
    // the ask. Emitted before the phrasing pass + route resolution
    // (each can take seconds on the fast slot). Bypasses
    // `try_emit_narration` — the session may already be past the 30s
    // retention window, and the chip's value is highest on long turns.
    if let (Some(events), Some(sid)) = (routing_events.as_ref(), session_id.as_ref()) {
        events
            .emit_turn_narration(TurnNarration {
                session_id: sid.clone(),
                conversation_id: conversation_id.to_string(),
                event: NarrationEvent {
                    phase: NarrationPhase::GapCheckFired,
                    text: "Came up short on this one — working out what would settle it."
                        .to_string(),
                    elapsed_ms: 0,
                },
            })
            .await;
    }

    // 2. Build the request DETERMINISTICALLY: the gap is the unanswered
    //    question itself. A fast-slot pass may PHRASE it into a concrete
    //    ask (D4: may phrase, never invent — the fallback is the user's
    //    question verbatim, and routes come only from the catalog
    //    resolver below).
    let gap_text = phrase_gap_question(inference, question, response)
        .await
        .unwrap_or_else(|| question.trim().to_string());
    let mut req = InformationRequest {
        current_understanding: String::new(),
        gap: gap_text,
        relevance: String::new(),
        satisfying_source: String::new(),
        search_hints: Vec::new(),
        task_id: conversation_id.to_string(),
        step_id: 0,
        // Runtime owns the contract that "anything coming out of
        // run_collaboration is post-answer refinement."
        kind: InformationRequestKind::Refinement,
        task_title: String::new(),
        // Routes are stamped by the resolver below, never by a model.
        routes: Vec::new(),
    };

    // Acquisition conjecture (EPISTEMIC_STATE.md §4.3): rank concrete
    // catalog-grounded routes for THIS gap. One embed call for the gap
    // text; catalog embeddings are disk-cached. Any failure ships the
    // card without routes — never blocks the request.
    if let Some(ctx) = route_ctx {
        req.routes =
            crate::runtime::acquisition::routes_for_gap(inference, &ctx, &req.gap).await;
    }

    tracing::info!(
        gap_chars = req.gap.len(),
        routes = req.routes.len(),
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

    // Boundary check: the phrasing + route resolution can take seconds
    // on the fast slot; if a newer turn arrived meanwhile, don't
    // surface a stale card.
    if preempted("post_gap_check") {
        return RefinementOutcome::NotAttempted;
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
            return RefinementOutcome::NotAttempted;
        }
    };
    // Boundary check: the card can pend indefinitely; if the user has
    // already moved the conversation on, a refinement of the OLD
    // answer is stale work occupying the primary slot.
    if preempted("post_user_content") {
        return RefinementOutcome::NotAttempted;
    }

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
    let mut refine_system = format!(
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
    // TEACHABLE: the K=1 prompt lesson survives refinement (see the
    // `lesson_prompt` parameter doc).
    if let Some(pf) = &lesson_prompt {
        refine_system.push_str("\n\n");
        refine_system.push_str(&crate::lessons::render_lesson_block(pf));
    }
    let refine_prompt = format!(
        "The user asked: {question}\n\n\
         Your initial answer (drawn from the local corpus):\n{response}\n\n\
         Additional source the user provided:\n{content}\n\n\
         Refine the answer to integrate the user's source, using ONLY \
         facts stated in the initial answer or in the source. If the \
         source names a thing without explaining it, name it without \
         explaining it — no mechanisms, numbers, or background from \
         memory. Be explicit about what came from the corpus vs. the \
         user's source. Mark anything that remains uncertain — especially \
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
        prompt_shape: None,
    };

    match inference.complete(&refine_req).await {
        Ok(c) => {
            tracing::info!(
                had_user_content = true,
                latency_ms = t_start.elapsed().as_millis() as u64,
                refined_chars = c.text.len(),
                "maybe_collaborate: refined answer produced"
            );
            if c.text == response {
                // Model declined to revise — glassbox the no-op so the
                // user knows their source was reviewed even though the
                // bubble didn't change. Caller still emits
                // `message-refined` with the original content so the
                // UI's `refining` flag clears.
                if let (Some(events), Some(sid)) = (routing_events.as_ref(), session_id.as_ref()) {
                    events
                        .emit_turn_narration(TurnNarration {
                            session_id: sid.clone(),
                            conversation_id: conversation_id.to_string(),
                            event: NarrationEvent {
                                phase: NarrationPhase::GapCheckFired,
                                text: "Reviewed the source — no change to the answer.".to_string(),
                                elapsed_ms: t_start.elapsed().as_millis() as u64,
                            },
                        })
                        .await;
                }
                RefinementOutcome::NoChange
            } else {
                RefinementOutcome::Refined {
                    text: c.text,
                    user_content: content,
                }
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            tracing::warn!(
                error = %err_str,
                "maybe_collaborate: refinement inference failed — falling back to original"
            );
            // Surface the failure as a stage-error narration so the
            // user sees *why* the bubble didn't change. Without this
            // chip the chat looks like nothing happened. The caller
            // is still responsible for emitting `message-refined`
            // with the original content to clear the UI's
            // `refining` flag — silence there is the stuck-state bug
            // this enum was introduced to prevent.
            if let (Some(events), Some(sid)) = (routing_events.as_ref(), session_id.as_ref()) {
                events
                    .emit_turn_narration(TurnNarration {
                        session_id: sid.clone(),
                        conversation_id: conversation_id.to_string(),
                        event: NarrationEvent {
                            phase: NarrationPhase::StageError {
                                stage: "refinement".to_string(),
                                error: err_str.clone(),
                            },
                            text: "Refinement failed — kept the original answer.".to_string(),
                            elapsed_ms: t_start.elapsed().as_millis() as u64,
                        },
                    })
                    .await;
            }
            RefinementOutcome::Failed { error: err_str }
        }
    }
}

/// Phrase the unanswered question into the card's concrete ask — the D4
/// "may phrase, never invent" pass (EPISTEMIC_STATE.md). One fast-slot
/// call, plain-text output, hard fallback to the user's question
/// verbatim: phrasing can improve the card's wording but can never
/// gate the card, invent a gap, or fail the flow. Routes are resolved
/// separately from the catalog and are never model-authored.
async fn phrase_gap_question(
    inference: &dyn InferenceProvider,
    question: &str,
    response: &str,
) -> Option<String> {
    const PHRASE_MAX_TOKENS: u32 = 72;
    let q: String = question.chars().take(600).collect();
    let r: String = response.chars().take(400).collect();
    let prompt = format!(
        "The assistant could not answer this from the user's connected sources.\n\n\
         Question: {q}\n\n\
         Reply given: {r}\n\n\
         Write ONE short line naming the missing information — the specific \
         fact, document, or source that would settle the question. Output the \
         line only — no preface, no quotes."
    );
    let mut request =
        crate::types::CompletionRequest::for_workload(crate::slot_policy::Workload::Housekeep, prompt)
            .with_system("You phrase information requests precisely. Output one line only.")
            .with_output_budget(PHRASE_MAX_TOKENS);
    request.temperature = Some(0.0);
    let resp = inference.complete(&request).await.ok()?;
    let cleaned = crate::title::strip_think_blocks(&resp.text);
    let line = cleaned.trim().lines().next()?.trim().trim_matches('"').trim();
    if line.is_empty() || line.chars().count() > 240 {
        return None;
    }
    Some(line.to_string())
}

/// Post-stream refinement primitive: run the gap check and, if the
/// user provides content, overwrite the saved assistant message and
/// emit `message-refined`. Called both from `handle_message_stream`'s
/// spawn (which has owned `Arc`s but no live `&self`) and from the
/// corresponding method on `Runtime`.
/// Re-gate capability for the refinement overwrite path. Carried by
/// callers whose original answer was released by the grounding gate:
/// the structural invariant is that a verified answer can NEVER be
/// overwritten by text that fails the same gate. `None` = today's
/// behavior, byte-identical.
pub(crate) struct RefinementGuard {
    pub inference: std::sync::Arc<dyn InferenceProvider>,
    pub evidence: crate::runtime::grounding::EvidenceContext,
}

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
    // TEACHABLE P0: forwarded to `run_collaboration` — see its doc.
    lesson_prompt: Option<String>,
    // Post-stream preemption — forwarded; see `run_collaboration`.
    preempt: Option<CancellationToken>,
    grounding_guard: Option<RefinementGuard>,
    // Acquisition-route context — forwarded; see `run_collaboration`.
    route_ctx: Option<crate::runtime::acquisition::RouteContext>,
) -> Option<String> {
    // Gap detection (I4-C retirement): read the turn's abstention off the
    // persisted gate metadata — the same field the epistemic assembler
    // reads (D3: the ledger's `CannotKnowFromHere` verdict derives from
    // exactly this signal). Ledger-less messages (gate off / legacy)
    // never fire the card.
    let abstained = original_metadata
        .as_ref()
        .and_then(|m| m.get("grounding_gate"))
        .and_then(|g| g.get("action"))
        .and_then(|a| a.as_str())
        .map(|a| a.starts_with("abstained"))
        .unwrap_or(false);
    let outcome = run_collaboration(
        inference,
        approval,
        inference_config,
        conversation_id,
        question,
        original_content,
        abstained,
        routing_events,
        session_id,
        lesson_prompt,
        preempt,
        route_ctx,
    )
    .await;

    match outcome {
        // Refinement never reached the inference call (auto-collab off,
        // no gap, user dismissed via Skip). The frontend never set
        // `m.refining = true` on this path, so no emit is needed.
        RefinementOutcome::NotAttempted => None,

        // Inference ran and produced new content. Persist the rewrite
        // and emit `message-refined` so the desktop swaps the bubble.
        RefinementOutcome::Refined {
            text: refined,
            user_content,
        } => {
            // The verification universe for a REFINED answer is the corpus
            // evidence PLUS the user-provided source (paste or web-search
            // results): the rewrite exists precisely to integrate that
            // source, so verifying against corpus-only evidence rejects
            // every rescue that adds genuinely new information — the
            // affordance's entire purpose.
            let evidence_with_source = if user_content.is_empty() {
                evidence.to_string()
            } else {
                format!("{evidence}\n---\n{user_content}")
            };
            // Post-synthesis guardrail (refinement path): the gap-check
            // rewrite is a fresh generation grounded in the same
            // evidence, so re-verify its quotes before it overwrites the
            // already-verified streamed answer. Without this, the guard
            // would be silently defeated on exactly the turns the gap
            // check fires. Empty evidence is a no-op.
            let refined = {
                let v = crate::quote_verification::verify_answer_against_evidence(
                    &refined,
                    &evidence_with_source,
                );
                if v.demoted_count > 0 {
                    tracing::warn!(
                        demoted = v.demoted_count,
                        verified = v.verified_count,
                        message_id = %message_id,
                        "post-stream refinement: guardrail demoted unverified quotations"
                    );
                }
                v.rewritten
            };
            // Refinement re-gate (GateSurface::Refinement, verify-only
            // — the refinement itself was the rewrite). On failure:
            // KEEP the verified original, but still emit
            // `message-refined` with the original content — the UI's
            // `refining` flag must clear either way.
            if let Some(guard) = grounding_guard {
                let profile = crate::runtime::grounding::GateSurface::Refinement.profile();
                // The gate's sealed universe must include the user's source,
                // or web/paste-derived facts in the rewrite can never verify.
                // chunk_labels stays PARALLEL to chunks (the alignment check
                // indexes them together) — when labels were unavailable
                // (empty, alignment skipped) we leave them empty rather than
                // desync the vectors.
                let mut gate_evidence = guard.evidence;
                if !user_content.is_empty() {
                    let labels_parallel =
                        gate_evidence.chunk_labels.len() == gate_evidence.chunks.len();
                    // PREPEND, not append: the per-claim support check walks
                    // chunks in order under a top-12 cap (judge.rs:292) and
                    // the specifics scan truncates similarly — appended at
                    // position 20-40 the user's source is never checked and
                    // the rewrite still rejects (measured: 5/5 reverts with
                    // the appended variant, user_content_chars in receipts).
                    //
                    // The chunk also carries the SAME date anchor the
                    // refinement system prompt injects: that prompt mandates
                    // date reasoning ("Current date: … compare source dates"),
                    // so refined answers legitimately say "since today is
                    // <date>" — and the gate then prosecuted exactly that as
                    // an ungrounded claim ("the evidence does not state the
                    // current date"; measured 2026-07-10, both validation
                    // rejects). Ambient truth the system itself asserted in
                    // the prompt belongs in the verification universe.
                    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                    // Judge-sized pieces: claim_violation_joint truncates
                    // EACH passage at 1,500 chars (judge.rs:755), so one
                    // multi-result block loses its tail — split on paragraph
                    // boundaries so every part of the source stays visible
                    // to every per-claim check. Each piece carries the date
                    // anchor so any piece alone grounds "today is <date>".
                    let mut pieces: Vec<String> = vec![String::new()];
                    for para in user_content.split("\n\n") {
                        let cur = pieces.last_mut().expect("pieces starts non-empty");
                        if !cur.is_empty() && cur.len() + para.len() > 1_300 {
                            pieces.push(para.to_string());
                        } else {
                            if !cur.is_empty() {
                                cur.push_str("\n\n");
                            }
                            cur.push_str(para);
                        }
                    }
                    for piece in pieces.iter().rev().filter(|p| !p.is_empty()) {
                        gate_evidence.chunks.insert(
                            0,
                            format!("Current date: {today}.\nUser-provided source:\n{piece}"),
                        );
                        if labels_parallel {
                            gate_evidence
                                .chunk_labels
                                .insert(0, vec!["user-provided source".to_string()]);
                        }
                    }
                    gate_evidence
                        .source_labels
                        .push("user-provided source".to_string());
                }
                let outcome = crate::runtime::grounding::gate_answer(
                    &guard.inference,
                    question,
                    refined.clone(),
                    &gate_evidence,
                    &crate::types::CompletionRequest::default(),
                    &profile,
                )
                .await;
                let action = outcome
                    .meta
                    .get("action")
                    .and_then(|a| a.as_str())
                    .unwrap_or("released");
                if matches!(action, "abstained_no_retry" | "annotated_no_retry") {
                    tracing::warn!(
                        target: "grounding_gate",
                        message_id = %message_id,
                        action,
                        user_content_chars = user_content.len(),
                        "refinement_rejected: refined text failed the grounding \
                         gate — keeping the verified original"
                    );
                    approval.emit_message_refined(MessageRefinedPayload {
                        conversation_id: conversation_id.to_string(),
                        message_id: message_id.to_string(),
                        new_content: original_content.to_string(),
                    });
                    return None;
                }
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

        // Inference ran but produced no change OR errored. In both
        // cases the user clicked Submit / Search on the
        // InformationRequestCard, so the desktop has the bubble in
        // `refining = true` state — we MUST emit `message-refined`
        // (with the original content) to clear that flag. Without
        // this emit the UI sticks on "Refining your answer" forever.
        // Repro: web-search affordance with primary slot in a bad
        // KV state — see Decode Error -3 trace 2026-05-25.
        RefinementOutcome::NoChange | RefinementOutcome::Failed { .. } => {
            if let RefinementOutcome::Failed { ref error } = outcome {
                tracing::warn!(
                    error = %error,
                    message_id = %message_id,
                    "post-stream refinement: emitting fallback message-refined to clear UI"
                );
            }
            approval.emit_message_refined(MessageRefinedPayload {
                conversation_id: conversation_id.to_string(),
                message_id: message_id.to_string(),
                new_content: original_content.to_string(),
            });
            None
        }
    }
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

/// Preemption registry for post-stream housekeeping (gap check /
/// refinement). A NEW user turn cancels the tokens of ALL outstanding
/// post-stream spawns — every conversation, not just its own — so
/// housekeeping never occupies a slot a fresh turn is waiting on
/// beyond the one inference call that may already be in flight
/// (cancellation is observed at step boundaries — see
/// `run_collaboration`).
///
/// Global scope is deliberate: this daemon serves one human, and
/// housekeeping is strictly lower priority than any user-facing turn.
/// The 2026-07-12 prefill A/B run showed every live dispatch collision
/// was CROSS-conversation (the desktop "new chat per question"
/// pattern); per-conversation scoping missed all of them. The map
/// stays keyed by conversation so `current()` hands each spawn the
/// token of its own turn.
///
/// Bounded like `Runtime::assembly_memo`: past
/// [`Self::MAX_CONVERSATIONS`] tracked conversations the map is
/// cleared wholesale (tokens dropped un-cancelled — their spawns just
/// run to completion once; a fresh process re-learns within a turn).
#[derive(Default)]
pub(crate) struct PostStreamPreemption {
    tokens: std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
}

impl PostStreamPreemption {
    const MAX_CONVERSATIONS: usize = 512;

    /// Called at the top of every streaming turn: cancels ALL
    /// outstanding post-stream tokens (any conversation — see the
    /// struct doc for why global) and mints the token the NEW turn's
    /// post-stream spawns will carry.
    pub(crate) fn begin_turn(&self, conversation_id: &str) -> CancellationToken {
        let mut map = self.tokens.lock().expect("post-stream preemption lock");
        let preempted = map.values().filter(|t| !t.is_cancelled()).count();
        for token in map.values() {
            token.cancel();
        }
        if preempted > 0 {
            tracing::info!(
                target: "post_stream",
                conversation_id,
                preempted,
                "post_stream: new turn — preempting all outstanding housekeeping"
            );
        }
        if map.len() >= Self::MAX_CONVERSATIONS && !map.contains_key(conversation_id) {
            map.clear();
        }
        let fresh = CancellationToken::new();
        map.insert(conversation_id.to_string(), fresh.clone());
        fresh
    }

    /// The live token for this conversation's current turn — fetched
    /// at post-stream spawn setup (same turn as the `begin_turn` that
    /// minted it). `None` when the map was cleared by the bound; the
    /// spawn then runs unpreemptable once, which is the pre-feature
    /// behavior.
    pub(crate) fn current(&self, conversation_id: &str) -> Option<CancellationToken> {
        self.tokens
            .lock()
            .expect("post-stream preemption lock")
            .get(conversation_id)
            .cloned()
    }
}

#[cfg(test)]
mod preemption_tests {
    use super::PostStreamPreemption;

    #[test]
    fn new_turn_cancels_all_outstanding_tokens_globally() {
        let reg = PostStreamPreemption::default();
        let t1 = reg.begin_turn("c1");
        assert!(!t1.is_cancelled(), "first turn's token starts live");

        // A turn on a DIFFERENT conversation preempts c1's housekeeping
        // too — the cross-conversation collision class from the
        // 2026-07-12 prefill A/B run (new chat per question).
        let t2 = reg.begin_turn("c2");
        assert!(
            t1.is_cancelled(),
            "cross-conversation token must be preempted"
        );
        assert!(!t2.is_cancelled(), "the new turn's token starts live");

        let t3 = reg.begin_turn("c2");
        assert!(
            t2.is_cancelled(),
            "same-conversation preemption still holds"
        );
        assert!(!t3.is_cancelled());
    }

    #[test]
    fn map_is_bounded() {
        let reg = PostStreamPreemption::default();
        for i in 0..(PostStreamPreemption::MAX_CONVERSATIONS + 8) {
            let _ = reg.begin_turn(&format!("c{i}"));
        }
        let len = reg.tokens.lock().unwrap().len();
        assert!(len <= PostStreamPreemption::MAX_CONVERSATIONS + 1);
    }
}
