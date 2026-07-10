// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streaming turn dispatch — `handle_message_stream` and its
//! classification-bearing body, plus the session-continuation
//! (`resume_session_stream`) and cancel-and-redirect
//! (`redirect_turn_stream`) entry points. The KnowledgeQuery and
//! Deep/Simple streaming synthesis loops are still INLINE here,
//! including their two near-duplicate refusal-retry state machines —
//! a known duplication whose unification is deliberately deferred
//! (the blocks differ in error-frame and finish-reason handling, so
//! merging them is a measured behavior change, not a move).
//! Extracted verbatim from `runtime.rs` in the 2026-06-10
//! decomposition; same `impl Runtime`-across-files pattern as
//! `handlers/`.

use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};

use crate::context::{build_context, format_history_as_prompt};
use crate::error::{Error, Result};
use crate::memory;
use crate::skills::SkillRegister;
use crate::slot_policy::Workload;

use super::*;

/// Terminal state observed by [`run_synthesis_stream`]: the caller threads
/// these into provenance + the post-stream gate. `model_id` may differ from
/// the value passed in because the refusal-retry re-opens a fresh stream.
struct SynthStreamOutcome {
    model_id: String,
    observed_finish: Option<crate::types::FinishReason>,
    observed_completion_tokens: Option<u32>,
}

/// The `'synth:` streaming-forward loop shared by the KnowledgeQuery and
/// DeepQuery streams. It was duplicated verbatim in both `tokio::spawn` arms
/// of `handle_message_stream_with_classification`, differing only in the gate
/// flag and the log tag — this is the single source of truth for both.
///
/// Behaviour:
/// - **gate mode** (`gate_on`) holds every token in `full_text`; the gate
///   block after the loop owns the release.
/// - **non-gate** forwards tokens to `tx`, buffering the head so the one-shot
///   refusal-retry can fire (re-synthesize once with the answer-prefill when
///   the head opens with the model's own refusal AND evidence was retrieved).
/// - cancellation (`biased`, so it wins over buffered tokens), terminal
///   `Finish`, and mid-stream `Finish::Error` / `Error` frames are resolved.
///
/// Returns `None` when the spawned turn must abort — the caller should
/// `return` immediately: `tx` was dropped (receiver gone) or the slot emitted
/// `Finish::Error`/`Error` (the error frame is already forwarded). Otherwise
/// returns the final model id + observed finish/usage.
async fn run_synthesis_stream(
    inference: &Arc<dyn InferenceProvider>,
    mut s: Pin<Box<dyn Stream<Item = crate::types::StreamFrame> + Send>>,
    mut model_id: String,
    request: &CompletionRequest,
    tx: &tokio::sync::mpsc::Sender<Result<String>>,
    cancel_for_stream: &tokio_util::sync::CancellationToken,
    full_text: &mut String,
    had_retrieved_chunks: bool,
    gate_on: bool,
    // In gate mode, a throttled sink for the running held-token COUNT — the
    // caller pumps it into a `SynthesisProgress` heartbeat so the user sees
    // the answer forming behind the gate. `None` on ungated/naked paths (they
    // stream tokens live, so there's nothing to bridge).
    progress_tx: Option<&tokio::sync::mpsc::Sender<u32>>,
    log_tag: &'static str,
) -> Option<SynthStreamOutcome> {
    let mut observed_finish: Option<crate::types::FinishReason> = None;
    let mut observed_completion_tokens: Option<u32> = None;
    let mut head = String::new();
    let mut head_flushed = false;
    let mut retried = false;
    // Heartbeat throttle: emit at most ~4×/sec so a fast slot doesn't flood
    // the narration channel, and count tokens (frames) held so far.
    let mut held_tokens: u32 = 0;
    let mut last_heartbeat = std::time::Instant::now();
    const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

    'synth: loop {
        loop {
            // Cancellation races the next frame. `biased` so a pending
            // cancel wins over buffered tokens — the user asked us to stop
            // NOW. Dropping `s` (when the spawn unwinds past 'synth) closes
            // the provider channel; the embedded engine breaks on the
            // failed send.
            let frame = tokio::select! {
                biased;
                _ = cancel_for_stream.cancelled() => {
                    tracing::info!(
                        chars_streamed = full_text.chars().count(),
                        "{}: cancelled by session token — terminating with FinishReason::Cancelled",
                        log_tag
                    );
                    observed_finish = Some(crate::types::FinishReason::Cancelled);
                    break 'synth;
                }
                f = s.next() => match f {
                    Some(fr) => fr,
                    None => break,
                },
            };
            use crate::types::StreamFrame;
            match frame {
                StreamFrame::Token(chunk) => {
                    if gate_on {
                        // Hold mode: accumulate everything; the gate block
                        // after 'synth owns the release (or the retry, or
                        // the abstention). Refusal-retry is skipped here —
                        // a refusal extracts as NO_CLAIM and releases
                        // ungated.
                        full_text.push_str(&chunk);
                        // Heartbeat: the token stays HELD, but its count goes
                        // out so the desktop can show the answer growing.
                        // Fire the FIRST frame immediately — feedback should
                        // appear the moment synthesis starts holding, not
                        // 250ms in — then throttle subsequent frames.
                        if let Some(ptx) = progress_tx {
                            held_tokens += 1;
                            if held_tokens == 1
                                || last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL
                            {
                                let _ = ptx.try_send(held_tokens);
                                last_heartbeat = std::time::Instant::now();
                            }
                        }
                    } else if head_flushed {
                        full_text.push_str(&chunk);
                        if tx.send(Ok(chunk)).await.is_err() {
                            return None;
                        }
                    } else {
                        head.push_str(&chunk);
                        full_text.push_str(&chunk);
                        if head.chars().count() >= REFUSAL_HEAD_CHARS {
                            if !retried && had_retrieved_chunks && looks_like_refusal_opener(&head)
                            {
                                retried = true;
                                tracing::info!(
                                    target: "synth.refusal_retry",
                                    head = %head.chars().take(80).collect::<String>(),
                                    "{}: refusal opener detected with evidence present — retrying with answer prefill",
                                    log_tag
                                );
                                full_text.clear();
                                full_text.push_str(REFUSAL_RETRY_PREFIX);
                                if tx.send(Ok(REFUSAL_RETRY_PREFIX.to_string())).await.is_err() {
                                    return None;
                                }
                                head_flushed = true;
                                let mut retry_req = request.clone();
                                retry_req.assistant_prefix = Some(REFUSAL_RETRY_PREFIX.to_string());
                                retry_req.system_message = Some(REFUSAL_RETRY_SYSTEM.to_string());
                                match inference
                                    .complete_stream_with_id_and_finish(&retry_req)
                                    .await
                                {
                                    Ok((s2, mid2)) => {
                                        s = s2;
                                        model_id = mid2;
                                        observed_finish = None;
                                        observed_completion_tokens = None;
                                        continue 'synth;
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Err(e)).await;
                                        return None;
                                    }
                                }
                            } else if tx.send(Ok(std::mem::take(&mut head))).await.is_err() {
                                return None;
                            } else {
                                head_flushed = true;
                            }
                        }
                    }
                }
                StreamFrame::Finish { reason, usage } => {
                    observed_completion_tokens = usage.as_ref().map(|u| u.completion_tokens);
                    // FinishReason::Error means the slot bailed mid-stream
                    // (context overflow, decode failure, tokenizer
                    // rejection). Surface it so the post-stream path doesn't
                    // save a 0-char message + fire a misleading
                    // InformationRequest.
                    if let crate::types::FinishReason::Error(ref msg) = reason {
                        tracing::warn!(
                            finish_reason = "error",
                            error = %msg,
                            chars_streamed = full_text.len(),
                            "{}: slot terminated with Finish::Error — propagating as error frame",
                            log_tag
                        );
                        let _ = tx
                            .send(Err(crate::error::Error::Inference(msg.clone())))
                            .await;
                        return None;
                    }
                    observed_finish = Some(reason);
                }
                StreamFrame::Error(msg) => {
                    let _ = tx.send(Err(crate::error::Error::Inference(msg))).await;
                    return None;
                }
            }
        }

        // Stream ended while still buffering the head (a short answer below
        // the threshold): decide on what we have. Gate mode never flushes
        // here — release happens after the verdict below.
        if !head_flushed && !gate_on {
            if !retried && had_retrieved_chunks && looks_like_refusal_opener(&head) {
                retried = true;
                tracing::info!(
                    target: "synth.refusal_retry",
                    head = %head.chars().take(80).collect::<String>(),
                    "{}: short refusal detected with evidence present — retrying with answer prefill",
                    log_tag
                );
                full_text.clear();
                full_text.push_str(REFUSAL_RETRY_PREFIX);
                if tx.send(Ok(REFUSAL_RETRY_PREFIX.to_string())).await.is_err() {
                    return None;
                }
                head_flushed = true;
                let mut retry_req = request.clone();
                retry_req.assistant_prefix = Some(REFUSAL_RETRY_PREFIX.to_string());
                retry_req.system_message = Some(REFUSAL_RETRY_SYSTEM.to_string());
                match inference
                    .complete_stream_with_id_and_finish(&retry_req)
                    .await
                {
                    Ok((s2, mid2)) => {
                        s = s2;
                        model_id = mid2;
                        observed_finish = None;
                        observed_completion_tokens = None;
                        continue 'synth;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return None;
                    }
                }
            } else {
                let _ = tx.send(Ok(std::mem::take(&mut head))).await;
                head_flushed = true;
            }
        }
        break 'synth;
    }

    // Glassbox (truncation trace 2026-06-30): emit the draft lifecycle for EVERY
    // grounded turn, not only Length finishes. The truncation is intermittent and
    // is NOT a Length cut (0 Length events across a 120-chat run), so the open
    // question is the finish reason + produced tokens vs the EFFECTIVE cap (after
    // prompt_budget::enforce) + the answer tail. Join to the chaos journal by
    // answer_chars + answer_tail; a mid-`[Source:` tail here = draft-side cut.
    tracing::info!(
        target: "synth.lifecycle",
        finish = ?observed_finish,
        completion_tokens = ?observed_completion_tokens,
        max_tokens = ?request.max_tokens,
        think_budget = ?request.think_budget,
        answer_chars = full_text.chars().count(),
        answer_tail = %tail_chars(&full_text, 48),
        "{}: synth draft complete",
        log_tag
    );
    if matches!(observed_finish, Some(crate::types::FinishReason::Length)) {
        tracing::warn!(
            target: "synth.truncation",
            max_tokens = ?request.max_tokens,
            completion_tokens = ?observed_completion_tokens,
            "{}: answer TRUNCATED at the token cap (finish_reason=Length)",
            log_tag
        );
    }

    // Soft landing (truncation fix 2026-06-30): if the draft ended mid-thought,
    // continue it to a natural boundary before returning. Detection is by
    // CONTENT, not finish_reason — this model reports Stop even at the cap, so
    // the Length branch above never fires for it. Skipped for cancelled turns.
    if !matches!(observed_finish, Some(crate::types::FinishReason::Cancelled)) {
        continue_truncated_synthesis(
            inference,
            request,
            tx,
            cancel_for_stream,
            full_text,
            gate_on,
            log_tag,
        )
        .await;
    }

    Some(SynthStreamOutcome {
        model_id,
        observed_finish,
        observed_completion_tokens,
    })
}

/// Last `n` chars of `s`, char-safe — for glassbox answer tails (truncation trace).
fn tail_chars(s: &str, n: usize) -> String {
    let mut v: Vec<char> = s.chars().rev().take(n).collect();
    v.reverse();
    v.into_iter().collect()
}

/// Soft-landing continuation for a draft that ended mid-thought.
///
/// This model family reports `finish=Stop` even when it stops at the token
/// cap, so a truncation is detected by CONTENT (`ends_mid_thought`), never by
/// `finish_reason`. We continue the assistant turn from the draft so far —
/// carried as `assistant_prefix`, which the inference layer commits before the
/// next generation token — in bounded rounds, until the answer lands on a
/// boundary or the round budget is spent. In gate mode the appended text is
/// held in `full_text` for the gate to re-verify the stitched whole; in
/// non-gate mode it is streamed as it arrives. The round cap + a min-length
/// guard keep a genuinely open-ended (or degenerate, e.g. a stray "search")
/// draft from looping.
async fn continue_truncated_synthesis(
    inference: &Arc<dyn InferenceProvider>,
    request: &CompletionRequest,
    tx: &tokio::sync::mpsc::Sender<Result<String>>,
    cancel: &tokio_util::sync::CancellationToken,
    full_text: &mut String,
    gate_on: bool,
    log_tag: &'static str,
) {
    use crate::runtime::evidence::ends_mid_thought;
    const MAX_ROUNDS: usize = 3;
    const PER_ROUND_TOKENS: usize = 512;
    const MIN_CHARS_TO_CONTINUE: usize = 40;

    for round in 0..MAX_ROUNDS {
        if cancel.is_cancelled()
            || full_text.trim().chars().count() < MIN_CHARS_TO_CONTINUE
            || !ends_mid_thought(full_text)
        {
            break;
        }
        let mut req = request.clone();
        // Commit the answer-so-far as the assistant prefill and let the model
        // continue it; `complete` returns only the NEW tokens after the prefix.
        req.assistant_prefix = Some(full_text.clone());
        req.max_tokens = Some(PER_ROUND_TOKENS);
        req.think_budget = Some(0);
        let added = match inference.complete(&req).await {
            Ok(resp) if !resp.text.is_empty() => resp.text,
            Ok(_) => break, // model had nothing to add
            Err(e) => {
                tracing::warn!(
                    target: "synth.continue",
                    round,
                    error = %e,
                    "{}: continuation call failed; releasing draft as-is",
                    log_tag
                );
                break;
            }
        };
        full_text.push_str(&added);
        // Non-gate mode already streamed the draft, so stream the continuation
        // too; gate mode holds everything until the verdict.
        if !gate_on && tx.send(Ok(added.clone())).await.is_err() {
            break; // receiver gone
        }
        tracing::info!(
            target: "synth.continue",
            round,
            added_chars = added.chars().count(),
            now_complete = !ends_mid_thought(full_text),
            total_chars = full_text.chars().count(),
            "{}: soft-landing continuation",
            log_tag
        );
    }
}

/// Run the production grounding gate on the HELD answer (shared by the
/// KnowledgeQuery and DeepQuery spawns). Skipped for cancelled turns; on judge
/// failure `gate_answer` itself fails open. Mutates `full_text` to the gated
/// text and returns the glassbox meta when the gate ran, else `None`.
async fn gate_held_answer(
    inference: &Arc<dyn InferenceProvider>,
    gate_on: bool,
    observed_finish: &Option<crate::types::FinishReason>,
    question: &str,
    full_text: &mut String,
    evidence: &crate::runtime::grounding::EvidenceContext,
    request: &CompletionRequest,
    profile: &crate::runtime::grounding::GroundingProfile,
) -> Option<serde_json::Value> {
    if gate_on && !matches!(observed_finish, Some(crate::types::FinishReason::Cancelled)) {
        // Pre-gate citation pass (2026-07-01): snap/strip the DRAFT's `[Source:]`
        // garbles before the audit sees them — otherwise the specifics scan flags
        // each garbled label as a fabricated specific and burns a rewrite cycle on
        // what the deterministic snap fixes for free. The post-gate pass below
        // stays: the longform rewrite can re-garble labels.
        let pre = crate::runtime::grounding::attribute_citations(
            full_text,
            &evidence.chunks,
            &evidence.source_labels,
        );
        if pre.changed() {
            tracing::info!(
                target: "synth.citation",
                stage = "pre_gate",
                citations_total = pre.citations_total,
                citations_stripped = pre.citations_stripped(),
                citations_snapped = pre.citations_snapped(),
                stripped = ?pre.stripped_titles,
                snapped = ?pre.snapped_titles,
                "corrected draft [Source:] citations before the gate"
            );
            *full_text = pre.cleaned;
        }
        // Truncation trace (2026-06-30): capture the draft BEFORE the gate takes it
        // so we can localize a mid-`[Source:` cut to the draft vs the gate.
        let draft_len = full_text.chars().count();
        let draft_tail = tail_chars(full_text, 48);
        let outcome = crate::runtime::grounding::gate_answer(
            inference,
            question,
            std::mem::take(full_text),
            evidence,
            request,
            profile,
        )
        .await;
        let gate_action = outcome
            .meta
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("?")
            .to_string();
        let gate_out_len = outcome.text.chars().count();
        let gate_out_tail = tail_chars(&outcome.text, 48);
        // Present the gated answer: strip phantom tool-call envelopes the model
        // reflexes (chat wires no tools) so the persisted record + non-desktop
        // surfaces don't carry a raw `<tool_call>` / `:code_search(...)` leak.
        *full_text = crate::pipeline::presenter::present_answer(&outcome.text);
        // Citation-attribution (faithfulness audit 2026-06-30): the value gate
        // passed the answer's TOP-LINE value, but synthesis may have propped it up
        // with FABRICATED supporting citations — `[Source: Re: Advertising Campaign
        // - NASCAR]` over an Enron corpus that never mentions NASCAR (audit turn
        // #4). Strip `[Source: …]` markers whose title is absent from the very
        // chunks the draft saw. Strip, don't gate: a false strip drops one marker,
        // never a good answer; the fabrication rate rides the gate meta so a
        // confabulation-heavy answer is visible to telemetry and the glassbox.
        let cites = crate::runtime::grounding::attribute_citations(
            full_text,
            &evidence.chunks,
            &evidence.source_labels,
        );
        if cites.changed() {
            tracing::info!(
                target: "synth.citation",
                stage = "post_gate",
                citations_total = cites.citations_total,
                citations_stripped = cites.citations_stripped(),
                citations_snapped = cites.citations_snapped(),
                fabrication_rate = cites.fabrication_rate(),
                stripped = ?cites.stripped_titles,
                snapped = ?cites.snapped_titles,
                "corrected [Source:] citations: snapped garbled labels, stripped unverifiable ones"
            );
            *full_text = cites.cleaned;
        }
        // Citation-value ALIGNMENT (gen75 NARA misattribution): a citation can
        // name a REAL label while its value came from a DIFFERENT chunk. When
        // the true holder is unambiguous, re-point the citation (provenance
        // becomes verifiable); when it isn't, drop the citation rather than
        // release one that disconfirms itself. Runs after the label-fidelity
        // pass so titles are already exact.
        if !evidence.chunk_labels.is_empty() {
            tracing::debug!(
                target: "synth.citation",
                stage = "align_input",
                n_chunks = evidence.chunks.len(),
                n_label_sets = evidence.chunk_labels.len(),
                labeled = evidence.chunk_labels.iter().filter(|l| !l.is_empty()).count(),
                distinct_titles = evidence
                    .chunk_labels
                    .iter()
                    .filter_map(|l| l.first())
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                first_labels = ?evidence.chunk_labels.first(),
                "alignment input"
            );
            let align = crate::runtime::grounding::align_citation_values(
                full_text,
                &evidence.chunks,
                &evidence.chunk_labels,
            );
            if align.changed() {
                tracing::info!(
                    target: "synth.citation",
                    stage = "align",
                    realigned = ?align.realigned,
                    stripped = ?align.stripped,
                    "re-pointed/stripped citations whose value lives in a different chunk"
                );
                *full_text = align.cleaned;
            }
        }
        // Glassbox: the full gate lifecycle. draft_tail complete + final_tail cut
        // ⇒ gate truncated; both cut ⇒ draft truncated; gate_out vs final differ
        // ⇒ present_answer. Join to the chaos journal by final_len + final_tail.
        tracing::info!(
            target: "gate.lifecycle",
            gate_action = %gate_action,
            draft_len,
            draft_tail = %draft_tail,
            gate_out_len,
            gate_out_tail = %gate_out_tail,
            final_len = full_text.chars().count(),
            final_tail = %tail_chars(full_text, 48),
            "gate lifecycle"
        );
        Some(outcome.meta)
    } else {
        None
    }
}

impl Runtime {
    // NOTE (pre-existing, preserved verbatim by the 2026-06-10 move):
    // the doc block + instrument attribute below describe
    // `handle_message_stream` but are attached to
    // `resume_session_stream` — the next item — so resume turns are
    // traced under the span name "runtime.handle_message_stream".
    // Left untouched because changing span names is observable
    // behavior, not a move.
    /// Stream a chat response token-by-token.
    ///
    /// Builds context, saves the user message, routes the intent, and starts
    /// streaming inference for SimpleQuery / DeepQuery / KnowledgeQuery. The
    /// returned [`StreamHandle`] yields response chunks; once the stream
    /// completes, the assistant message is persisted under `message_id`.
    ///
    /// Returns [`Error::NotImplemented`] for ComplexTask intents — callers
    /// should fall back to [`Self::handle_message`] in that case.
    #[tracing::instrument(
        name = "runtime.handle_message_stream",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    /// PR2 session-continuation entry point. Called when the user
    /// clicks a ClarificationCard option or a NextStepOffer. Takes
    /// the `ResumeSession` hint, synthesises a fresh
    /// `RouterClassification` from it (primary = hinted intent,
    /// confidence = 1.0, MoveKind::Commit by construction), and
    /// dispatches through the regular `handle_message_stream` body —
    /// just with classification pre-decided so no router call is
    /// made. PR2c will additionally reuse the retrieval cache keyed
    /// by `resume.session_id`.
    pub async fn resume_session_stream(
        &self,
        message: &str,
        conversation_id: &str,
        resume: ResumeSession,
    ) -> Result<StreamHandle> {
        tracing::info!(
            session_id = %resume.session_id,
            intent_hint = %resume.intent_hint,
            "runtime: resume session (continuation)"
        );
        let hinted = parse_intent_hint(&resume.intent_hint);
        let synthetic = RouterClassification {
            primary: IntentCandidate {
                intent: hinted,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: Some(format!("session continuation from {}", &resume.session_id)),
            coarse_intent: Some("CONTINUATION".to_string()),
            self_assessment: None,
            timing: None,
            scope: None,
        };
        self.handle_message_stream_with_classification(message, conversation_id, Some(synthetic))
            .await
    }

    /// PR2c redirect handler — cancel an in-flight Propose-mode
    /// sampler AND restart synthesis against the alternative intent
    /// the user picked. Reads `session.input` + `session.conversation_id`
    /// from the earlier `SessionStore.begin(...)` call, so the caller
    /// only needs to pass the session id + intent hint. The old
    /// assistant message stays in history (cancelled, possibly
    /// partial) — the new one appears below as a fresh stream.
    ///
    /// PR2c scope: cancel + new stream, no retrieval reuse yet (the
    /// new stream re-runs `prepare_knowledge_query_plan`). Caching is
    /// PR2d — noted in the plan file.
    pub async fn redirect_turn_stream(
        &self,
        session_id: &str,
        intent_hint: &str,
    ) -> Result<StreamHandle> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| Error::NotImplemented(format!("session {session_id} not found")))?;
        tracing::info!(
            session_id,
            intent_hint,
            from_intent = ?session.classification.primary.intent,
            "routing:redirected — cancelling current sampler and re-dispatching"
        );
        // PR4 — structural-signal capture. Update the `routing_log`
        // row for this session's message with
        // `was_redirected = true` + `redirect_to = <intent_hint>`.
        // The hash must match the one `Router::classify` wrote via
        // `log_routing` — both sides use `router::message_hash`.
        // Best-effort; a db failure here doesn't block the redirect.
        let signal_hash = crate::router::message_hash(&session.input);
        let signal_hint = intent_hint.to_string();
        let signal_store = Arc::clone(&self.store);
        tokio::spawn(async move {
            if let Err(e) = signal_store
                .mark_routing_redirected(&signal_hash, &signal_hint)
                .await
            {
                tracing::warn!(error = %e, "routing:redirect_signal write failed");
            } else {
                tracing::info!(
                    hash = %signal_hash,
                    redirect_to = %signal_hint,
                    "routing:redirect_signal captured"
                );
            }
        });
        // Cancel the in-flight sampler so it drains and releases the
        // slot lock before we spawn the replacement stream. Receiver
        // drop (existing semantics) would also work, but the explicit
        // token cancel is observable in `inference:cancelled` logs.
        session.cancel.cancel();
        // Hand off to the same continuation path the Clarification
        // card uses. Same synthetic-classification shape, just
        // tagged so the trace differentiates the two kinds of
        // continuations.
        let hinted = parse_intent_hint(intent_hint);
        let synthetic = RouterClassification {
            primary: IntentCandidate {
                intent: hinted,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: Some(format!("redirect from session {session_id}")),
            coarse_intent: Some("REDIRECT".to_string()),
            self_assessment: None,
            timing: None,
            scope: None,
        };
        let message = session.input.clone();
        let conversation_id = session.conversation_id.clone();
        drop(session);
        self.handle_message_stream_with_classification(&message, &conversation_id, Some(synthetic))
            .await
    }

    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        self.handle_message_stream_with_classification(message, conversation_id, None)
            .await
    }

    /// Run a streaming turn with the intent PINNED, skipping the router's
    /// classification. A general seam (same synthetic-classification path
    /// as session resume/redirect): a caller that already knows the intent
    /// — e.g. a single-purpose sealed corpus whose questions are always
    /// factual lookups — uses this to bypass router misclassification. The
    /// caller owns the choice of intent; this method has no domain
    /// knowledge of why.
    pub async fn handle_message_stream_as(
        &self,
        message: &str,
        conversation_id: &str,
        intent: Intent,
    ) -> Result<StreamHandle> {
        let synthetic = RouterClassification {
            primary: IntentCandidate {
                intent,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: Some("caller-pinned intent".to_string()),
            coarse_intent: None,
            self_assessment: None,
            timing: None,
            scope: None,
        };
        self.handle_message_stream_with_classification(message, conversation_id, Some(synthetic))
            .await
    }

    /// Naked chat turn — raw model, none of the Sovereign affordances.
    ///
    /// Desktop "naked mode" (a user setting) routes here instead of
    /// `handle_message_stream`: the conversation history is rendered
    /// straight into the prompt and streamed from the loaded model with
    /// NO retrieval, router, grounding gate, tools, atlas, or gap-check.
    /// The only system context is a minimal assistant preamble plus the
    /// user's custom instructions (persona) when set — the desktop
    /// equivalent of the benches' `--naked` mode. Returns the same
    /// `StreamHandle` so the caller's streaming UI is unchanged.
    ///
    /// v1 cancellation is a standalone token: the stop button doesn't
    /// reach naked turns yet (naked is affordance-free by construction
    /// and never calls `sessions.begin`). Wiring stop is a follow-up.
    pub async fn handle_message_stream_naked(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        if message.len() > MAX_TURN_MESSAGE_CHARS {
            return Err(Error::InvalidInput(OVERSIZE_MESSAGE_HINT.to_string()));
        }
        if is_degenerate_message(message) {
            return Err(Error::InvalidInput(DEGENERATE_MESSAGE_HINT.to_string()));
        }

        // Prior history only — no working-memory / topic shaping.
        let principal = self
            .corpus_principal
            .as_ref()
            .and_then(|r| r.principal_for(conversation_id));
        let mut context = build_context(
            self.store.as_ref(),
            conversation_id,
            message,
            principal.as_deref(),
        )
        .await?;

        // Persist the user turn (same as the situated path).
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;
        context.conversation.messages.push(user_msg);

        // System = minimal assistant preamble + the user's persona
        // (custom instructions) when set. Nothing else.
        let mut system = "You are a helpful assistant.".to_string();
        if let Some(ci) = self.inference_config.custom_instructions.as_deref() {
            let ci = ci.trim();
            if !ci.is_empty() {
                system.push_str("\n\n");
                system.push_str(ci);
            }
        }

        // Raw request: the rendered transcript is the prompt; no tools,
        // evidence, or grammar. SLOT_POLICY §3 Passthrough — the model
        // the user chose to run naked; latency=Normal → shadow Speed::Slow
        // (Primary), unchanged from the prior explicit Slow.
        let mut request =
            CompletionRequest::for_workload(Workload::Passthrough, format_history_as_prompt(&context, 24));
        request.system_message = Some(system);

        let message_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let cancel = tokio_util::sync::CancellationToken::new();
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();

        tokio::spawn(async move {
            let (s, model_id) = match inference.complete_stream_with_id_and_finish(&request).await {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
            let mut full_text = String::new();
            let _ = run_synthesis_stream(
                &inference,
                s,
                model_id,
                &request,
                &tx,
                &cancel,
                &mut full_text,
                false, // had_retrieved_chunks — naked has no evidence
                false, // gate_on — no grounding gate
                None,  // no heartbeat — naked streams tokens live
                "naked",
            )
            .await;

            let assistant_msg = Message {
                id: message_id_owned,
                conversation_id: conversation_id_owned.clone(),
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: None,
                version: now(),
            };
            if let Err(e) = store.save_message(&assistant_msg).await {
                tracing::warn!(
                    conversation_id = %conversation_id_owned,
                    error = %e,
                    "naked stream: failed to save assistant message"
                );
            }
        });

        Ok(StreamHandle {
            message_id,
            stream: Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)),
        })
    }

    /// KnowledgeQuery / ComparisonQuery streaming turn. Lifted verbatim
    /// from `handle_message_stream_with_classification` so the dispatcher
    /// reads as a table of contents; behaviour unchanged.
    #[allow(clippy::too_many_arguments)]
    async fn stream_knowledge_query_turn(
        &self,
        message: &str,
        conversation_id: &str,
        context: ConversationContext,
        classification: RouterClassification,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
        scope: Option<String>,
        intent: Intent,
        _session_id: String,
        cancel_token: tokio_util::sync::CancellationToken,
        tool_descriptors: Vec<ToolDescriptor>,
    ) -> Result<StreamHandle> {
        tracing::info!(
            intent = ?intent,
            "runtime: stream path — KnowledgeQuery/ComparisonQuery with token streaming"
        );

        // RetrievalStart — fire immediately so the desktop chip
        // appears before the corpus search begins. Bypasses
        // `try_emit_narration` (which suppresses below 1.5s
        // elapsed) because the user is staring at typing-dots
        // and needs to see activity within 200ms. RetrievalComplete
        // below remains gated by the suppression rules.
        let retrieval_start_at = std::time::Instant::now();
        self.routing_events
            .emit_turn_narration(TurnNarration {
                session_id: _session_id.clone(),
                conversation_id: conversation_id.to_string(),
                event: NarrationEvent {
                    phase: NarrationPhase::RetrievalStart,
                    text: "Searching your knowledge…".to_string(),
                    elapsed_ms: 0,
                },
            })
            .await;

        let plan = self
            .prepare_knowledge_query_plan(message, &context, &intent, scope.as_deref())
            .await;
        tracing::debug!(
            retrieval_ms = retrieval_start_at.elapsed().as_millis() as u64,
            chunks = plan.chunks.len(),
            "runtime:retrieval_start_to_complete"
        );

        // PR5 — post-retrieval retrieval-miss diversion. Off-
        // target evidence shape (dispersed across ≥3 sources,
        // no source concentration, no title match) was
        // historically the exact input that produced confident
        // parametric fabrication. Suppress synthesis and emit a
        // clarification card instead.
        if plan.shape.is_off_target() {
            tracing::info!(
                session_id = %_session_id,
                retrieval_count = plan.shape.count,
                distinct_sources = plan.shape.distinct_sources,
                title_match = plan.shape.title_match,
                top_source_repeat = plan.shape.top_source_repeat_count,
                top_source = %plan.shape.top_source_label,
                top1_score = plan.shape.top1_score,
                median_ratio = plan.shape.median_ratio,
                "routing:retrieval_miss — diverting to Ask clarification"
            );
            return self
                .handle_retrieval_miss_stream(
                    message,
                    conversation_id,
                    &_session_id,
                    &plan.shape,
                    &tool_descriptors,
                )
                .await;
        }

        // Narration: report retrieval shape on long turns.
        // Suppressed internally when total elapsed is below the
        // `NARRATION_MIN_ELAPSED` window or the per-turn cap is
        // hit. The session store guards both; this call is safe
        // on short turns — it just returns `None`.
        //
        // Emit on every non-empty retrieval (not just on
        // `top_source_repeat_count >= 2`). The user is staring at
        // the typing-dots spinner and the most useful thing we
        // can tell them after retrieval finishes is "we read N
        // chunks across these sources." When the top source
        // dominates we say so; otherwise we report the spread.
        if !plan.chunks.is_empty() {
            let txt = if plan.shape.top_source_repeat_count >= 2 {
                format!(
                    "Read {} chunks — {} from one source, so I'll keep the answer focused.",
                    plan.chunks.len(),
                    plan.shape.top_source_repeat_count,
                )
            } else {
                format!(
                    "Read {} chunks across {} sources — drafting the response.",
                    plan.chunks.len(),
                    plan.shape.distinct_sources.max(1),
                )
            };
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::RetrievalComplete {
                    chunks_in: plan.chunks.len(),
                    corpora: plan.source_map.keys().cloned().collect(),
                },
                txt,
            ) {
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: _session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event,
                    })
                    .await;
            }
        }

        let message_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

        // Everything the spawned task needs — no borrows of `self`.
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let approval = Arc::clone(&self.approval);
        let inference_config = self.inference_config.clone();
        // Tool-Mastery Layer 3 — cloned so the nested
        // post-stream gap-check spawn can write a
        // `tool_decision` outcome note after refinement
        // resolves. Soft-fail when no NoteStore is wired
        // (test harnesses): `record_tool_outcome` no-ops.
        let notes_for_outcome: Option<Arc<corpus_engine_notes::NoteStore>> =
            self.note_store.clone();
        // Cloned into the outer spawn so the post-stream gap-
        // check can emit narration chips that reach the desktop
        // UI alongside the INFORMATION REQUEST card. Without
        // these the chip-then-card glassbox UX silently drops
        // for the streaming path. See `run_collaboration` for
        // how they're consumed.
        let collab_routing_events: Option<Arc<dyn RoutingEventSink>> =
            Some(Arc::clone(&self.routing_events));
        let collab_session_id: Option<String> = Some(_session_id.clone());
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();
        let question = message.to_string();

        let KnowledgeQueryPlan {
            request,
            chunks,
            gate_entity_anchored,
            doc_context,
            shape,
            route,
            gap_check_enabled,
            search_ms,
            retrieved_chunks,
            source_map,
            result_quality,
            prompt_budget_note,
            folder_meta,
            meta_atlas_hits,
        } = plan;
        let documents_found = chunks.len();
        // Answerable-context gate for the refusal-retry (KQ path), mirroring
        // the DeepQuery spawn: only retry a refusal when evidence WAS
        // retrieved — a genuine "no sources" stays an honest abstention.
        let had_retrieved_chunks = documents_found > 0;
        let top_source_label = shape.top_source_label.clone();
        let coarse_intent_for_prov = coarse_intent.clone();
        let self_assessment_for_prov = self_assessment.clone();
        let routing_trigger_for_prov = classification.rationale.clone();

        // PR3: compute next-step offers against the same
        // retrieval the answer was built from. We do this on the
        // main task (not the spawn) so we can capture the
        // user's message by reference without cloning into the
        // async move. The result is serialised into message
        // metadata inside the spawn.
        let had_dominant_source = shape.top_source_repeat_count >= 2;
        let retrieval_missed = shape.is_off_target();
        let top_source_title_owned = if shape.top_source_key.1.is_empty() {
            None
        } else {
            Some(shape.top_source_key.1.clone())
        };
        let offers = build_next_step_offers(&OfferContext {
            user_message: message,
            top_source_title: top_source_title_owned.as_deref(),
            had_dominant_source,
            retrieved_chunks: &retrieved_chunks,
            session_id: &_session_id,
            retrieval_missed,
        });
        let offers_json = serde_json::to_value(&offers).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "next_steps: serialize failed");
            serde_json::Value::Array(Vec::new())
        });
        let route_for_log = route;

        // Production grounding gate (see runtime/grounding.rs).
        // Short answers: hold → single-claim verify → retry →
        // abstain. Long-form (PrimarySynthesis): hold → per-claim
        // audit → rewrite → annotate. Zero-chunk turns skip — the
        // structural GK caveat owns that path.
        // Governance corpora take the FR-9 governance surface (its own
        // bank/override); everything else is the general KnowledgeQuery
        // gate. Both run the identical cite-or-abstain ladder.
        let gate_surface = self.kq_gate_surface(context.conversation.enabled_corpora.as_deref());
        let gate_on = gate_surface.enabled() && documents_found > 0;
        // The turn's sealed evidence universe — built here because
        // the spawned task holds no `&self`. Claim search is
        // sealed to the conversation's corpora.
        let gate_evidence = crate::runtime::grounding::EvidenceContext {
            chunks: if gate_on {
                crate::runtime::grounding::gate_evidence_chunks(&chunks)
            } else {
                Vec::new()
            },
            chunk_labels: if gate_on {
                crate::runtime::grounding::gate_evidence_chunk_labels(&chunks)
            } else {
                Vec::new()
            },
            source_labels: if gate_on {
                crate::runtime::grounding::gate_evidence_source_labels(&chunks)
            } else {
                Vec::new()
            },
            searcher: if gate_on {
                Some(std::sync::Arc::new(
                    self.claim_searcher(context.conversation.enabled_corpora.as_deref(), &chunks),
                ) as _)
            } else {
                None
            },
            entity_anchored: gate_entity_anchored,
            // Best retrieval similarity over the draft's chunks → the env-gated
            // retry-floor signal (EvidenceContext::top_similarity). Only the gate
            // path (chunks present) carries it.
            top_similarity: if gate_on {
                let best = chunks
                    .iter()
                    .filter_map(|c| c.vector_distance.map(|d| 1.0 - d))
                    .fold(f32::NEG_INFINITY, f32::max);
                best.is_finite().then_some(best)
            } else {
                None
            },
        };
        let gate_profile = gate_surface.profile();
        let gate_question: String = message.to_string();
        if crate::runtime::grounding::grounding_gate_enabled() {
            crate::runtime::grounding::dbg(&format!(
                "gate_on={gate_on} route={route:?} docs={documents_found} gate_entity_anchored={gate_entity_anchored}"
            ));
        }

        // Hold-phase narration: the gate withholds every token
        // until verification completes, which on a gated turn
        // reads as a stall without this chip. Emitted on the main
        // task (we still hold `&self`); try_emit_narration's
        // elapsed/cap suppression applies as usual.
        if gate_on {
            let txt = "Drafting an answer, then verifying it against your \
                       sources before showing it."
                .to_string();
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::GroundingVerifyStart,
                txt,
            ) {
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: _session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event,
                    })
                    .await;
            }
        }

        // Narration: synthesis-start chip. Bridges the silent
        // gap between retrieval-complete and the first streamed
        // token — which on a cold primary slot can be 90+
        // seconds (model load) plus another minute or two of
        // CPU decode for a 35B Q6. Without this the user sees
        // the same "Working on it…" placeholder for the entire
        // wait. Emitted on the main task (we still hold `&self`)
        // immediately before the spawn that calls
        // `complete_stream_with_id`. The 1.5s narration gate
        // suppresses this on short DeepQuery turns where
        // synthesis is fast enough that no chip is needed.
        {
            let txt = "Generating a deep answer with the primary model — \
                       first use after a restart can take a minute."
                .to_string();
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::PrimarySynthesisStart,
                txt,
            ) {
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: _session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event,
                    })
                    .await;
            }
        }

        let cancel_for_stream = cancel_token.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();

            let (s, model_id) = match inference.complete_stream_with_id_and_finish(&request).await {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };

            let mut full_text = String::new();

            // A request-carried assistant_prefix is part of the
            // ANSWER (the model decodes as its continuation) but
            // not part of the completion stream — emit it visibly
            // first or the user/judges never see the committed
            // text. Today the only initial-request setter on this
            // path is the structural GK caveat (knowledge_query's
            // foreign-topic insufficiency); the retry paths below
            // set their prefix on retry_req and emit it manually.
            if let Some(pfx) = request.assistant_prefix.clone() {
                full_text.push_str(&pfx);
                // In gate mode nothing is sent until the verdict.
                if !gate_on && tx.send(Ok(pfx)).await.is_err() {
                    return;
                }
            }

            // Delightful waiting UX: the gate holds every token, so bridge the
            // silent stretch with a live token-count heartbeat. A throttled
            // channel carries the running count out of `run_synthesis_stream`;
            // this reader turns each into a `SynthesisProgress` chip the
            // desktop shows ticking up. Gate mode only — an ungated turn
            // already streams tokens the user can watch. The reader ends when
            // the channel closes (`hb_tx` dropped after synthesis).
            let hb_tx = if gate_on {
                let (tx_hb, mut rx_hb) = tokio::sync::mpsc::channel::<u32>(4);
                let hb_events = collab_routing_events.clone();
                let hb_sid = collab_session_id.clone();
                let hb_cid = conversation_id_owned.clone();
                tokio::spawn(async move {
                    let (Some(events), Some(sid)) = (hb_events, hb_sid) else {
                        return;
                    };
                    while let Some(tokens) = rx_hb.recv().await {
                        events
                            .emit_turn_narration(TurnNarration {
                                session_id: sid.clone(),
                                conversation_id: hb_cid.clone(),
                                event: NarrationEvent {
                                    phase: NarrationPhase::SynthesisProgress { tokens },
                                    text: String::new(),
                                    elapsed_ms: started.elapsed().as_millis() as u64,
                                },
                            })
                            .await;
                    }
                });
                Some(tx_hb)
            } else {
                None
            };

            // Refusal-retry + token forwarding live in the shared
            // `run_synthesis_stream` (mirrored by the DeepQuery spawn).
            // `None` => the turn must abort (tx dropped or Finish::Error
            // already forwarded).
            let Some(synth) = run_synthesis_stream(
                &inference,
                s,
                model_id,
                &request,
                &tx,
                &cancel_for_stream,
                &mut full_text,
                had_retrieved_chunks,
                gate_on,
                hb_tx.as_ref(),
                "kq-stream",
            )
            .await
            else {
                return;
            };
            drop(hb_tx); // close the heartbeat channel → the reader task ends
            let model_id = synth.model_id;
            let observed_finish = synth.observed_finish;
            let observed_completion_tokens = synth.observed_completion_tokens;

            // Production grounding gate: the full ladder lives in
            // grounding::gate_answer (shared with the non-streaming
            // path). Runs on the HELD answer before anything
            // reaches the user; fail-open on judge failure.
            let grounding_gate_meta = gate_held_answer(
                &inference,
                gate_on,
                &observed_finish,
                &gate_question,
                &mut full_text,
                &gate_evidence,
                &request,
                &gate_profile,
            )
            .await;

            // Post-synthesis guardrail: demote any quoted span that
            // isn't verbatim-present in the evidence shown to the
            // model before it's persisted, so the stored record (and
            // any reload of this bubble) can't present a composite /
            // fabricated quotation as verbatim. The live token stream
            // already went out unmodified — this hardens the durable
            // copy; in gate mode the verified rewrite IS what gets
            // released to the user (held streams send post-rewrite).
            // The refinement path (collaboration.rs) re-verifies
            // any gap-check rewrite. Empty doc_context (parametric
            // path) is a no-op.
            let full_text = {
                let v = crate::quote_verification::verify_answer_against_evidence(
                    &full_text,
                    &doc_context,
                );
                if v.demoted_count > 0 {
                    tracing::warn!(
                        demoted = v.demoted_count,
                        verified = v.verified_count,
                        "kq-stream: post-synthesis guardrail demoted unverified quotations"
                    );
                }
                v.rewritten
            };

            // Gate mode held every token — release the final
            // (gated, quote-verified) text as one frame now.
            if gate_on && !cancel_for_stream.is_cancelled() {
                if tx.send(Ok(full_text.clone())).await.is_err() {
                    return;
                }
            }

            // Persist final assistant message with full KQ metadata
            // so the UI citation expander and provenance header
            // have everything they had on the non-streaming path.
            let (sources_for_prov, coverage_for_prov) = build_provenance_components(
                &source_map,
                &std::collections::HashMap::new(),
                &folder_meta,
                // KnowledgeQueryPlan doesn't carry the
                // display-category lookup; the chip-label rename
                // for conversation corpora only fires on the
                // DeepQuery path (see `prepare_knowledge_context`).
                // Threading the lookup through the plan is a
                // follow-up if we want the KQ streaming surface
                // to render "Your conversations" as well.
                None,
            );
            // Phase 5 — typed Finish frame from the provider is
            // now the source of truth for length truncation, no
            // more chars-per-token heuristic. Falls back to
            // `Stop` when the provider closed the stream without
            // a terminal frame (older test stubs); the trait
            // `complete_stream_with_finish` default guarantees a
            // terminal frame on every provider that ships today.
            let finish_reason_typed = observed_finish.unwrap_or(crate::types::FinishReason::Stop);
            let max_budget = inference_config.max_tokens;
            // Provider-reported count when present; otherwise fall
            // back to a chars-per-token estimate so the UI's
            // "(N generated)" line stays useful even on providers
            // that don't emit usage. The estimate is signposted
            // — tracing makes the source legible to the operator
            // post-hoc.
            let completion_tokens_val = observed_completion_tokens
                .unwrap_or_else(|| (full_text.chars().count() / 4) as u32);
            if observed_completion_tokens.is_none() {
                tracing::debug!(
                    chars = full_text.chars().count(),
                    est_completion_tokens = completion_tokens_val,
                    "runtime: kq-stream - usage absent, completion_tokens estimated from chars"
                );
            }
            let provenance = ResponseProvenance {
                intent: "KnowledgeQuery".to_string(),
                search_method: Some("CorpusEngine".to_string()),
                sources: sources_for_prov,
                inference_backend: model_id,
                oicp_match: None,
                total_latency_ms: started.elapsed().as_millis() as u64,
                tokens_used: completion_tokens_val as usize,
                coarse_intent: coarse_intent_for_prov,
                self_assessment: self_assessment_for_prov,
                routing_trigger: routing_trigger_for_prov,
                coverage: coverage_for_prov,
                finish_reason: Some(finish_reason_typed),
                max_tokens_budget: Some(max_budget),
                completion_tokens: Some(completion_tokens_val),
                context_window: inference.effective_context_size(),
            };
            let metadata_json = serde_json::json!({
                "streamed": true,
                "intent": "knowledge_query",
                "documents_found": documents_found,
                "search_ms": search_ms,
                "result_quality": result_quality,
                "provenance": provenance,
                "retrieved_chunks": retrieved_chunks,
                // Glassbox for the production grounding gate:
                // null when the gate is off / out of scope;
                // otherwise {action, retried, violation_prob,
                // threshold} so the UI can render verified /
                // regenerated / abstained provenance.
                "grounding_gate": grounding_gate_meta,
                // Glassbox for the prompt-budget guard: non-null
                // when assembly exceeded the context window and
                // the prompt was trimmed (runtime::prompt_budget).
                "prompt_budget": prompt_budget_note,
                // Move 4 — canonical-entity-boost echo for the
                // bench's fourth legibility lens. Empty when the
                // registry was unset or matched no entities.
                "meta_atlas_hits": meta_atlas_hits,
                // PR3 — grounded follow-ups rendered as clickable
                // NextStepButtons under the bubble. Empty array
                // when retrieval produced nothing to ground an
                // offer against; the UI hides the row.
                "next_steps": offers_json,
            });
            let assistant_msg = Message {
                id: message_id_owned.clone(),
                conversation_id: conversation_id_owned.clone(),
                role: Role::Assistant,
                content: full_text.clone(),
                created_at: now(),
                metadata: Some(metadata_json.clone()),
                version: now(),
            };
            if let Err(e) = store.save_message(&assistant_msg).await {
                tracing::warn!(
                    conversation_id = %conversation_id_owned,
                    error = %e,
                    "KnowledgeQuery stream: failed to save assistant message"
                );
            }

            if gap_check_enabled {
                // Per the humility principle (see
                // `prepare_knowledge_query_plan` for the long
                // form): always run the gap check on KQ paths.
                // The retrieval-shape route (FastFocused vs
                // PrimarySynthesis) decides synthesis style; it
                // does NOT decide whether the answer is actually
                // grounded. The gap check is the LLM-based
                // judge of "did the model answer the question?"
                // and has to fire regardless of how concentrated
                // the retrieval looked. Top-source label is
                // included in the log so a grep on
                // `gap_check_scheduled` reconstructs which
                // retrieval-shape paths reach the check.
                tracing::debug!(
                    route = ?route_for_log,
                    top_source = %top_source_label,
                    "KnowledgeQuery stream: scheduling post-stream gap check"
                );
                let collab_inference = Arc::clone(&inference);
                let collab_store = Arc::clone(&store);
                let collab_approval = Arc::clone(&approval);
                let collab_config = inference_config.clone();
                let collab_cid = conversation_id_owned.clone();
                let collab_mid = message_id_owned.clone();
                let collab_question = question.clone();
                let collab_original = full_text.clone();
                let collab_evidence = doc_context.clone();
                let collab_metadata = metadata_json;
                // Clone the routing-events sink + session id
                // into the spawn so the gap-check chips ("now
                // auditing the answer", "found something to
                // ask about") reach the desktop UI alongside
                // the in-flight INFORMATION REQUEST card.
                let collab_events = collab_routing_events.clone();
                let collab_sid = collab_session_id.clone();
                let collab_sid_for_outcome = collab_sid.clone();
                let collab_notes_for_outcome = notes_for_outcome.clone();
                // Tool-Mastery Layer 3 — record what happened
                // on this KQ turn so the next turn's dossier
                // can read it. Outcome resolves from the
                // post-stream refinement result (Stale =
                // gap-check fired and rewrote the answer),
                // plus the evidence-presence signal captured
                // before the spawn (NoResults = retrieval was
                // empty; Useful = chunks landed and the
                // original answer stood). All writes are
                // best-effort — see `dossier::record_tool_outcome`.
                // Tool-Mastery Layer 3 — synchronous baseline
                // write BEFORE the gap-check spawn fires. Writing
                // here (not inside the spawn) guarantees the
                // tool_decision lands even when the bench / CLI
                // exits before the gap-check spawn completes —
                // run_post_stream_refinement can take 10-30s and
                // the next turn's dossier read would otherwise
                // see nothing. The spawn below MAY overwrite with
                // `Stale` when refinement actually rewrites the
                // answer; the dossier reader returns
                // most-recent-first so the later write supersedes
                // when it lands in time.
                // Decide outcome from three orthogonal signals so a
                // turn whose retrieval LANDED but whose answer
                // landed in "I don't know" territory still records
                // `no-results` (the snapshot-freshness shape: the
                // hybrid retriever happily returns 30+ historical
                // Tour de France articles for a "2027 Tour" query
                // even though none of them are about 2027). The
                // answer-content check uses general English
                // negation + absence patterns, not bank vocabulary,
                // so it transfers across questions.
                let answer_is_honest_negation = {
                    let lower = full_text.to_lowercase();
                    let has_negation = [
                        "don't",
                        "do not",
                        "cannot",
                        "can't",
                        "doesn't have",
                        "no information",
                        "no data",
                        "no record",
                        "outside",
                        "unable to",
                    ]
                    .iter()
                    .any(|w| lower.contains(w));
                    let has_scope_token = [
                        "information",
                        "data",
                        "record",
                        "snapshot",
                        "knowledge base",
                        "details",
                        "results",
                    ]
                    .iter()
                    .any(|w| lower.contains(w));
                    has_negation && has_scope_token
                };
                let retrieval_missed = documents_found == 0
                    || answer_is_honest_negation
                    || (!shape.title_match
                        && shape.query_token_coverage < EVIDENCE_MIN_TOKEN_COVERAGE);
                let baseline_outcome = if retrieval_missed {
                    crate::memory::ToolDecisionOutcome::NoResults
                } else {
                    crate::memory::ToolDecisionOutcome::Useful
                };
                let baseline_reasoning = if documents_found == 0 {
                    "knowledge retrieval returned 0 chunks".to_string()
                } else if answer_is_honest_negation {
                    format!(
                        "retrieval returned {documents_found} chunks but \
                         the assistant's answer acknowledged a gap \
                         (snapshot-freshness or scope mismatch)"
                    )
                } else if retrieval_missed {
                    format!(
                        "retrieval returned {documents_found} chunks but \
                         title_match=false and query_token_coverage={:.2} \
                         (corpus does not cover this topic)",
                        shape.query_token_coverage
                    )
                } else {
                    format!("synthesised over {documents_found} chunks")
                };
                // Tier 1 (result memory): populate summary +
                // turn_index so the next turn's dossier can
                // render addressable references. `evidence_ids`
                // stays empty here because the legacy KQ path
                // doesn't route through knowledge_lookup tool —
                // when it does (follow-up PR), this site will
                // also pass the per-call ev-Tn-NNNN handles.
                let summary = if shape.top_source_label.is_empty() {
                    None
                } else {
                    Some(shape.top_source_label.clone())
                };
                // Turn index: count of prior user messages
                // (zero-based). The current in-flight user
                // message is already pushed onto
                // conversation.messages by the time we reach
                // this site, so subtract 1.
                let turn_index_for_outcome = context
                    .conversation
                    .messages
                    .iter()
                    .filter(|m| matches!(m.role, Role::User))
                    .count()
                    .saturating_sub(1);
                let baseline_extras = crate::memory::ToolDecisionExtras {
                    summary: summary.clone(),
                    evidence_ids: Vec::new(),
                    turn_index: turn_index_for_outcome,
                };
                crate::dossier::record_tool_outcome(
                    notes_for_outcome.as_deref(),
                    collab_sid_for_outcome.as_deref().unwrap_or(""),
                    Some(&conversation_id_owned),
                    "knowledge_lookup",
                    baseline_outcome,
                    &baseline_reasoning,
                    baseline_extras,
                )
                .await;

                let outcome_notes = collab_notes_for_outcome.clone();
                let outcome_notes_present = outcome_notes.is_some();
                // Capture Tier-1 extras for the stale-write
                // path inside the spawn (closure can't reach
                // back to the dispatch-frame locals).
                let stale_summary_for_capture = summary.clone();
                let turn_index_for_capture = turn_index_for_outcome;
                // Refinement re-gate: only armed when this turn's
                // answer was itself gate-released.
                let collab_guard = if gate_on {
                    Some(crate::runtime::collaboration::RefinementGuard {
                        inference: std::sync::Arc::clone(&collab_inference),
                        evidence: gate_evidence,
                    })
                } else {
                    None
                };
                tokio::spawn(async move {
                    tracing::info!(
                        conversation_id = %collab_cid,
                        has_notes = outcome_notes_present,
                        documents_found,
                        "dossier:streaming_kq_outcome_spawn_fired"
                    );
                    let refined = run_post_stream_refinement(
                        collab_inference.as_ref(),
                        collab_approval.as_ref(),
                        collab_store.as_ref(),
                        &collab_config,
                        &collab_cid,
                        &collab_mid,
                        &collab_question,
                        &collab_original,
                        &collab_evidence,
                        Some(collab_metadata),
                        collab_events,
                        collab_sid,
                        collab_guard,
                    )
                    .await;
                    if refined.is_some() {
                        // Stale write supersedes the baseline
                        // entry from above. Preserve summary +
                        // turn_index so the dossier history
                        // stays addressable; flag the outcome
                        // change via the new reasoning.
                        let stale_extras = crate::memory::ToolDecisionExtras {
                            summary: stale_summary_for_capture.clone(),
                            evidence_ids: Vec::new(),
                            turn_index: turn_index_for_capture,
                        };
                        crate::dossier::record_tool_outcome(
                            outcome_notes.as_deref(),
                            collab_sid_for_outcome.as_deref().unwrap_or(""),
                            Some(&collab_cid),
                            "knowledge_lookup",
                            crate::memory::ToolDecisionOutcome::Stale,
                            "gap-check refined the post-stream answer",
                            stale_extras,
                        )
                        .await;
                    }
                });
            }

            // Auto-title after first exchange — same post-stream
            // hook the non-KQ streaming path uses. Non-blocking.
            let title_inference = Arc::clone(&inference);
            let title_store = Arc::clone(&store);
            let title_cid = conversation_id_owned.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::title::try_auto_title(
                    title_inference.as_ref(),
                    title_store.as_ref(),
                    &title_cid,
                )
                .await
                {
                    tracing::warn!(
                        conversation_id = %title_cid,
                        error = %e,
                        "auto-title: generation failed (KQ stream path)"
                    );
                }
            });
        });

        let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
        return Ok(StreamHandle { message_id, stream });
    }

    /// DeepQuery / SimpleQuery streaming turn. Lifted verbatim from
    /// `handle_message_stream_with_classification` so the dispatcher reads
    /// as a table of contents; behaviour unchanged.
    #[allow(clippy::too_many_arguments)]
    async fn stream_deep_query_turn(
        &self,
        message: &str,
        conversation_id: &str,
        context: ConversationContext,
        classification: RouterClassification,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
        scope: Option<String>,
        intent: Intent,
        _session_id: String,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<StreamHandle> {
        // RetrievalStart — DeepQuery streaming path. Skipped for
        // SimpleQuery because that intent is a quick factual answer
        // and the existing RetrievalComplete narration is also gated
        // off for it (chunks typically empty). Fires immediately so
        // the chip is on screen before `prepare_knowledge_context`
        // returns.
        if !matches!(intent, Intent::SimpleQuery) {
            self.routing_events
                .emit_turn_narration(TurnNarration {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    event: NarrationEvent {
                        phase: NarrationPhase::RetrievalStart,
                        text: "Searching your knowledge…".to_string(),
                        elapsed_ms: 0,
                    },
                })
                .await;
        }

        // 4. Search knowledge + build prompt (shared with handle_simple).
        let kc = self
            .prepare_knowledge_context(message, &context, &intent, scope.as_deref())
            .await;

        // Narration — DeepQuery / SimpleQuery streaming path. Mirrors
        // the KnowledgeQuery/ComparisonQuery branch above, but keyed
        // off `KnowledgeContext` (no `plan.shape` available here).
        // Suppressed by the session store when total elapsed < 1.5s
        // or the per-turn cap is hit, so this is safe on fast paths.
        if !matches!(intent, Intent::SimpleQuery) && !kc.chunks.is_empty() {
            let txt = format!(
                "Read {} chunks across {} sources — drafting the response.",
                kc.chunks.len(),
                kc.sources.len().max(1),
            );
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::RetrievalComplete {
                    chunks_in: kc.chunks.len(),
                    corpora: kc.sources.iter().map(|s| s.origin.clone()).collect(),
                },
                txt,
            ) {
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: _session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event,
                    })
                    .await;
            }
        }

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(&intent)
        };

        // Model ID is captured from `complete_stream_with_id` once
        // the provider has committed to a routing decision — see
        // the trait docs on that method. Using the pre-stream sync
        // `model_id_for` here would miss peer attribution (the
        // mesh wrapper can only report "I routed to peer X" after
        // its async `select_peer` pass has run).
        //
        // Tier 2: populate evidence_id_allowlist from the
        // conversation's prior tool_decision payloads so the
        // sampler's EvidenceIdAllowlistConstraint can block
        // fabrications of `[ev-Tn-NNNN]` ids the model hasn't
        // actually been given. Soft-fails to None when no prior
        // ids exist (Tier 1 prompt discipline is then the only
        // safety net — same posture as today).
        let evidence_id_allowlist = self.gather_evidence_id_allowlist(conversation_id).await;
        // Generous synthesis output budget so a thorough DEEP answer completes
        // instead of truncating mid-sentence at the general cap (mirror of the
        // KnowledgeQuery PrimarySynthesis fix; the enforce() ladder protects this
        // reservation by trimming evidence first). Deep answers are the long-form
        // path, so this is where truncation bites hardest. Env-tunable
        // (SOVEREIGN_SYNTHESIS_OUTPUT_FLOOR, default 4096); see synth.truncation.
        let synth_max = self.inference_config.max_tokens.max(
            std::env::var("SOVEREIGN_SYNTHESIS_OUTPUT_FLOOR")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(4096),
        );
        let mut request = CompletionRequest {
            prompt: kc.prompt,
            system_message: Some(kc.system),
            preferred_speed: kc.speed,
            max_tokens: Some(synth_max),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist,
            lark_grammar: None,
        };
        // Phase-1 prompt-budget guard: assembled input + response
        // reservation must fit the context window, or the engine's
        // "Prompt too long" rejection becomes a terminal user-facing
        // error loop (note 2cd9227e). See `prompt_budget` for the
        // degradation ladder; the note lands in message metadata.
        let budget_note = match self.inference.effective_context_size() {
            Some(ctx) => {
                let (outcome, measured) =
                    prompt_budget::enforce(&mut request, &|s| self.inference.count_tokens(s), ctx);
                let budget_trimmed =
                    matches!(outcome, prompt_budget::BudgetOutcome::Trimmed { .. });
                // Glassbox (truncation trace 2026-06-30): the EFFECTIVE response cap
                // after the budget guard. If a big prompt (the truncated case had
                // ~16k chars of evidence) made enforce shrink max_tokens, this is
                // where a non-Length-looking short answer would originate.
                tracing::info!(
                    target: "synth.budget",
                    ctx,
                    budget_trimmed,
                    effective_max_tokens = ?request.max_tokens,
                    "prompt budget enforced"
                );
                // Phase 2: the memo records pre-trim DEMAND so the
                // compaction sensor and next-turn allocator see what
                // assembly actually wanted.
                self.record_assembly(conversation_id, measured);
                match outcome {
                    prompt_budget::BudgetOutcome::Trimmed { note } => Some(note),
                    _ => None,
                }
            }
            None => None,
        };

        let search_method = kc.search_method;
        let sources = kc.sources;
        let coverage = kc.coverage;
        let retrieved_chunks = kc.retrieved_chunks;
        // Answerable-context gate for the refusal-retry: only retry a refusal
        // when evidence WAS retrieved (a genuine "no sources" must still be an
        // honest abstention, never force-answered).
        let had_retrieved_chunks = !retrieved_chunks.is_empty();

        // Format the corpus evidence now so the post-stream epistemic-
        // humility hook can feed it to the gap checker. Moved into the
        // streaming spawn; not used before the synthesis completes.
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let question = message.to_string();

        let intent_label = format!("{intent:?}");
        let message_id = uuid::Uuid::new_v4().to_string();

        // Narration — synthesis-start chip on the DeepQuery /
        // SimpleQuery streaming path. Bridges the silence between
        // retrieval-complete and the first streamed token. With
        // primary-slot prewarm in place this is typically a no-op
        // wait, but it's still the right time to acknowledge the
        // long phase to the user.
        if matches!(request.preferred_speed, Speed::Slow) {
            let txt = "Generating a deep answer with the primary model.".to_string();
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::PrimarySynthesisStart,
                txt,
            ) {
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: _session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event,
                    })
                    .await;
            }
        }

        // 5. Spawn streaming task.
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let approval = Arc::clone(&self.approval);
        let inference_config = self.inference_config.clone();
        // Cloned into the spawn so the post-stream gap-check chips
        // can reach the desktop UI. See the matching block in the
        // KnowledgeQuery streaming branch above for the rationale.
        let routing_events_for_spawn: Option<Arc<dyn RoutingEventSink>> =
            Some(Arc::clone(&self.routing_events));
        let session_id_for_spawn: Option<String> = Some(_session_id.clone());
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();
        // Capture recalled memories on the relational/witness path so
        // the desktop's inner-work surface can render echo dots in the
        // gutter beside the just-committed paragraph. Gated to the
        // relational register so non-relational turns don't leak
        // memory contents into UI metadata they don't need. Thin
        // shape — id + content + created_at is what the echo overlay
        // displays; the rest of the Memory record stays internal.
        let recalled_memories_for_metadata: Option<serde_json::Value> =
            if context.turn_register() == SkillRegister::Relational && !context.memories.is_empty()
            {
                Some(serde_json::Value::Array(
                    context
                        .memories
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "id": m.id,
                                "content": m.content,
                                "created_at": m.created_at,
                            })
                        })
                        .collect(),
                ))
            } else {
                None
            };

        // Production grounding gate, DeepQuery side: long-form answers
        // take the per-claim audit → rewrite → annotate ladder in
        // grounding::gate_answer (short deep answers fall through to
        // the single-claim ladder). entity_anchored=false here — the
        // agentic loop (and its atlas gazetteer verdict) is KQ-only
        // today, and the long-form ladder doesn't consume it.
        let deep_gate_surface = crate::runtime::grounding::GateSurface::DeepQuery;
        let deep_gate_on = deep_gate_surface.enabled() && !kc.chunks.is_empty();
        // The turn's sealed evidence universe (deep answers are
        // usually long-form). Built pre-spawn; claim search sealed to
        // the conversation's corpora. entity_anchored=false — the
        // agentic loop (and its atlas gazetteer verdict) is KQ-only.
        let deep_gate_evidence =
            crate::runtime::grounding::EvidenceContext {
                chunks: if deep_gate_on {
                    crate::runtime::grounding::gate_evidence_chunks(&kc.chunks)
                } else {
                    Vec::new()
                },
                chunk_labels: if deep_gate_on {
                    crate::runtime::grounding::gate_evidence_chunk_labels(&kc.chunks)
                } else {
                    Vec::new()
                },
                source_labels: if deep_gate_on {
                    crate::runtime::grounding::gate_evidence_source_labels(&kc.chunks)
                } else {
                    Vec::new()
                },
                searcher: if deep_gate_on {
                    Some(std::sync::Arc::new(self.claim_searcher(
                        context.conversation.enabled_corpora.as_deref(),
                        &kc.chunks,
                    )) as _)
                } else {
                    None
                },
                entity_anchored: false,
                // Best retrieval similarity over the draft's chunks → the env-gated
                // retry-floor signal. The floor engages on the short single-claim
                // path (a short deep answer can reach it); long-form deep answers
                // take the per-claim audit path that ignores it.
                top_similarity: if deep_gate_on {
                    let best = kc
                        .chunks
                        .iter()
                        .filter_map(|c| c.vector_distance.map(|d| 1.0 - d))
                        .fold(f32::NEG_INFINITY, f32::max);
                    best.is_finite().then_some(best)
                } else {
                    None
                },
            };
        let deep_gate_profile = deep_gate_surface.profile();
        let deep_gate_question: String = message.to_string();
        if deep_gate_on {
            let txt = "Drafting an answer, then verifying it against your                        sources before showing it."
                .to_string();
            if let Some(event) = self.sessions.try_emit_narration(
                &_session_id,
                NarrationPhase::GroundingVerifyStart,
                txt,
            ) {
                self.routing_events
                    .emit_turn_narration(TurnNarration {
                        session_id: _session_id.clone(),
                        conversation_id: conversation_id.to_string(),
                        event,
                    })
                    .await;
            }
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

        let cancel_for_stream = cancel_token.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut full_text = String::new();

            let (s, model_id) = match inference.complete_stream_with_id_and_finish(&request).await {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };

            // Refusal-retry + token forwarding live in the shared
            // Token-count heartbeat during the gated hold (mirrors the
            // KnowledgeQuery spawn). Reader ends when `hb_tx` drops.
            let hb_tx = if deep_gate_on {
                let (tx_hb, mut rx_hb) = tokio::sync::mpsc::channel::<u32>(4);
                let hb_events = routing_events_for_spawn.clone();
                let hb_sid = session_id_for_spawn.clone();
                let hb_cid = conversation_id_owned.clone();
                tokio::spawn(async move {
                    let (Some(events), Some(sid)) = (hb_events, hb_sid) else {
                        return;
                    };
                    while let Some(tokens) = rx_hb.recv().await {
                        events
                            .emit_turn_narration(TurnNarration {
                                session_id: sid.clone(),
                                conversation_id: hb_cid.clone(),
                                event: NarrationEvent {
                                    phase: NarrationPhase::SynthesisProgress { tokens },
                                    text: String::new(),
                                    elapsed_ms: started.elapsed().as_millis() as u64,
                                },
                            })
                            .await;
                    }
                });
                Some(tx_hb)
            } else {
                None
            };

            // Refusal-retry + token forwarding live in the shared
            // `run_synthesis_stream` (mirrored by the KnowledgeQuery spawn).
            // `None` => the turn must abort (tx dropped or Finish::Error
            // already forwarded).
            let Some(synth) = run_synthesis_stream(
                &inference,
                s,
                model_id,
                &request,
                &tx,
                &cancel_for_stream,
                &mut full_text,
                had_retrieved_chunks,
                deep_gate_on,
                hb_tx.as_ref(),
                "deep-stream",
            )
            .await
            else {
                return;
            };
            drop(hb_tx); // close the heartbeat channel → the reader task ends
            let model_id = synth.model_id;
            let observed_finish = synth.observed_finish;
            let observed_completion_tokens = synth.observed_completion_tokens;

            // Production grounding gate (deep): held answer → shared
            // ladder. Long-form deep answers take the per-claim audit.
            let grounding_gate_meta = gate_held_answer(
                &inference,
                deep_gate_on,
                &observed_finish,
                &deep_gate_question,
                &mut full_text,
                &deep_gate_evidence,
                &request,
                &deep_gate_profile,
            )
            .await;

            // Phase 5 — typed Finish frame from the provider is the
            // source of truth for length truncation. Falls back to
            // `Stop` when the provider closed without a terminal
            // frame (older test stubs); the trait
            // `complete_stream_with_finish` default guarantees a
            // terminal frame on every provider that ships today.
            let finish_reason_typed = observed_finish.unwrap_or(crate::types::FinishReason::Stop);
            let max_budget = inference_config.max_tokens;
            let completion_tokens_val = observed_completion_tokens
                .unwrap_or_else(|| (full_text.chars().count() / 4) as u32);
            if observed_completion_tokens.is_none() {
                tracing::debug!(
                    chars = full_text.chars().count(),
                    est_completion_tokens = completion_tokens_val,
                    "runtime: deep-stream - usage absent, completion_tokens estimated from chars"
                );
            }
            let provenance = ResponseProvenance {
                intent: intent_label,
                search_method,
                sources,
                inference_backend: model_id,
                oicp_match: None,
                total_latency_ms: started.elapsed().as_millis() as u64,
                tokens_used: completion_tokens_val as usize,
                coarse_intent,
                self_assessment,
                routing_trigger: classification.rationale.clone(),
                coverage,
                finish_reason: Some(finish_reason_typed),
                max_tokens_budget: Some(max_budget),
                completion_tokens: Some(completion_tokens_val),
                context_window: inference.effective_context_size(),
            };
            let metadata_json = serde_json::json!({
                "streamed": true,
                "provenance": provenance,
                "retrieved_chunks": retrieved_chunks,
                // Phase 3b: present only on the relational/witness
                // path; absent or null elsewhere. The desktop's
                // inner-work surface renders these as gutter echo
                // dots; chat ignores the field.
                "recalled_memories": recalled_memories_for_metadata,
                // Glassbox for the prompt-budget guard: non-null when
                // assembly exceeded the context window and the prompt
                // was trimmed to fit (see runtime::prompt_budget).
                "prompt_budget": budget_note,
                "grounding_gate": grounding_gate_meta,
            });
            // Post-synthesis guardrail (DeepQuery / reasoning stream):
            // same contract as the KnowledgeQuery stream — demote any
            // quoted span not verbatim-present in the evidence before
            // it's persisted. Empty evidence (pure-reasoning, no
            // retrieval) is a no-op. The refinement path
            // (collaboration.rs) re-verifies any gap-check rewrite.
            let full_text = {
                let v = crate::quote_verification::verify_answer_against_evidence(
                    &full_text, &evidence,
                );
                if v.demoted_count > 0 {
                    tracing::warn!(
                        demoted = v.demoted_count,
                        verified = v.verified_count,
                        "deep-stream: post-synthesis guardrail demoted unverified quotations"
                    );
                }
                v.rewritten
            };
            // Gate mode held every token — release the final text now.
            if deep_gate_on && !cancel_for_stream.is_cancelled() {
                if tx.send(Ok(full_text.clone())).await.is_err() {
                    return;
                }
            }
            let assistant_msg = Message {
                id: message_id_owned.clone(),
                conversation_id: conversation_id_owned.clone(),
                role: Role::Assistant,
                content: full_text.clone(),
                created_at: now(),
                metadata: Some(metadata_json.clone()),
                version: now(),
            };
            let _ = store.save_message(&assistant_msg).await;

            // Epistemic-humility hook (post-stream): audit the streamed
            // answer and, if the user provides additional content, rewrite
            // the persisted message and emit a `message-refined` event so
            // the UI can update the bubble in place. Runs concurrently
            // with auto-title so neither blocks the other.
            let collab_inference = Arc::clone(&inference);
            let collab_store = Arc::clone(&store);
            let collab_approval = Arc::clone(&approval);
            let collab_config = inference_config.clone();
            let collab_cid = conversation_id_owned.clone();
            let collab_mid = message_id_owned.clone();
            let collab_question = question.clone();
            let collab_evidence = evidence.clone();
            let collab_original = full_text.clone();
            let collab_metadata = metadata_json;
            // Routing-events sink + session id for gap-check
            // narration chips. Same rationale as the KnowledgeQuery
            // spawn above — without these the chip-then-card UX
            // silently drops on the streaming path. The clones
            // were already lifted above the outer spawn so this
            // is a cheap inner re-clone.
            let collab_events = routing_events_for_spawn.clone();
            let collab_sid = session_id_for_spawn.clone();
            // Post-stream tasks (epistemic-humility audit + auto-title)
            // share the fast-slot inflight semaphore with user-facing
            // requests. Under sequential load — eval bench, atlas
            // pipeline, anyone calling the daemon back-to-back — the
            // next request's routing classify queues behind these,
            // adding 30–60s of latency per turn for ~zero observable
            // benefit on the bench (the streamed answer is already
            // delivered; the refinement is a server-side rewrite).
            // Set `SOVEREIGN_SKIP_POST_STREAM=1` to disable both tasks.
            // The right architectural fix is a priority queue or
            // separate slot for background work; this env knob is the
            // diagnostic + bench-iteration lever.
            let skip_post_stream = std::env::var("SOVEREIGN_SKIP_POST_STREAM")
                .map(|v| v == "1")
                .unwrap_or(false);
            if !skip_post_stream {
                // Refinement re-gate: armed only when this turn's
                // answer was itself gate-released.
                let collab_guard = if deep_gate_on {
                    Some(crate::runtime::collaboration::RefinementGuard {
                        inference: std::sync::Arc::clone(&collab_inference),
                        evidence: deep_gate_evidence,
                    })
                } else {
                    None
                };
                tokio::spawn(async move {
                    run_post_stream_refinement(
                        collab_inference.as_ref(),
                        collab_approval.as_ref(),
                        collab_store.as_ref(),
                        &collab_config,
                        &collab_cid,
                        &collab_mid,
                        &collab_question,
                        &collab_original,
                        &collab_evidence,
                        Some(collab_metadata),
                        collab_events,
                        collab_sid,
                        collab_guard,
                    )
                    .await;
                });

                // Auto-title after first exchange. Non-blocking; the stream has
                // already delivered the response to the user.
                let title_inference = Arc::clone(&inference);
                let title_store = Arc::clone(&store);
                let title_cid = conversation_id_owned.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::title::try_auto_title(
                        title_inference.as_ref(),
                        title_store.as_ref(),
                        &title_cid,
                    )
                    .await
                    {
                        tracing::warn!(
                            conversation_id = %title_cid,
                            error = %e,
                            "auto-title: generation failed (stream path)"
                        );
                    }
                });
            }
        });

        let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));

        Ok(StreamHandle { message_id, stream })
    }

    /// Private inner entry point for [`handle_message_stream`] and
    /// [`resume_session_stream`]. When `preset` is `Some`, the
    /// classifier call is skipped; when `None`, classification runs
    /// as normal.
    async fn handle_message_stream_with_classification(
        &self,
        message: &str,
        conversation_id: &str,
        preset: Option<RouterClassification>,
    ) -> Result<StreamHandle> {
        tracing::info!("runtime: stream turn begin");
        // PR2e — reject oversized turn messages before any Fast-slot
        // work runs. Document-sized inputs belong in the attached-
        // file path; dropping 20 pages into the chat body used to
        // hang `compress_working_memory` for minutes.
        if message.len() > MAX_TURN_MESSAGE_CHARS {
            tracing::warn!(
                message_chars = message.len(),
                limit = MAX_TURN_MESSAGE_CHARS,
                "runtime:oversize_message rejected"
            );
            return Err(Error::InvalidInput(OVERSIZE_MESSAGE_HINT.to_string()));
        }
        if is_degenerate_message(message) {
            tracing::info!("runtime: degenerate (contentless) message — returning clarification");
            return Err(Error::InvalidInput(DEGENERATE_MESSAGE_HINT.to_string()));
        }
        // Reserve the turn's cancel token NOW, before the multi-second
        // build-context + classification + retrieval preparing window. A
        // `cancel_stream` that arrives during that window (Stop clicked the
        // instant the button appears, common on a slow model) trips this
        // token via `cancel_preparing`; `sessions.begin` below ADOPTS it, so
        // the cancel carries through to synthesis. Without this the cancel
        // hit only the previous (stale) session and this turn ran to
        // completion (2026-07-07 slow-model race).
        let _ = self.sessions.reserve_cancel(conversation_id);
        // 1. Build context.
        let principal = self
            .corpus_principal
            .as_ref()
            .and_then(|r| r.principal_for(conversation_id));
        let mut context = build_context(
            self.store.as_ref(),
            conversation_id,
            message,
            principal.as_deref(),
        )
        .await?;
        tracing::debug!(
            messages = context.conversation.messages.len(),
            memories = context.memories.len(),
            installed_corpora = context.installed_corpora.len(),
            "runtime: stream context built"
        );

        // 1a. Embedding-based memory recall on relational/witness paths.
        // Mirrors the non-streaming path (see `handle_turn`). FTS
        // keyword recall misses concrete-event seed memories on
        // abstract self-referential queries (hard-mode H05).
        //
        // Scope-aware: the recall is walled by the conversation's
        // skill_id so an inner-work conversation only surfaces
        // inner-work memories, and a general conversation never sees
        // them. See `MemoryScope` for the invariant.
        //
        // Mode-derived guard, NOT `turn_register()` — the intent
        // policy binds after routing (line ~2581), so at this point
        // `turn_register()` always returns the Factual fallback and a
        // register check would silently disable embed recall + sticky
        // pins (found 2026-07-09 on the non-streaming twin).
        let relational_turn = {
            let early_mode = self.resolve_active_mode(conversation_id).await;
            let declared_register = early_mode
                .as_deref()
                .and_then(|id| self.skills.skill_by_id(id))
                .map(|s| s.inference.register)
                .unwrap_or_default();
            declared_register == SkillRegister::Relational
                || early_mode.as_deref() == Some(crate::intent_policy::MODE_INNER_WORK)
        };
        if relational_turn {
            let scope = crate::traits::MemoryScope::from_conversation_skill(
                context.conversation.skill_id.as_deref(),
            );
            // Optional cross-encoder rerank — same seam as the
            // non-streaming path; inert when no `rerank_fn` is
            // configured.
            match memory::recall_relevant_memories_embed_reranked(
                self.inference.as_ref(),
                self.store.as_ref(),
                &scope,
                message,
                5,
                self.rerank_fn.as_ref(),
            )
            .await
            {
                Ok(top) if !top.is_empty() => {
                    tracing::debug!(
                        before = context.memories.len(),
                        after = top.len(),
                        "runtime: stream memories overridden via embedding recall"
                    );
                    // Sticky pins — same contract as the non-streaming
                    // path (see `merge_recall_pins`).
                    context.memories = self
                        .merge_recall_pins(&context.conversation.id, &scope, top)
                        .await;
                }
                _ => {}
            }
        }

        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 1b. Update topic context for turn-aware routing. The
        //     incoming user `message` is passed in so the extractor
        //     can detect a pivot off the prior arc — otherwise the
        //     topic stays anchored to the last assistant turn and a
        //     learner question that shifts subject ("Why didn't
        //     relativity win the Nobel?" after a photoelectric chain)
        //     keeps the stale topic, dragging retrieval off course.
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
            Some(message),
        )
        .await
        .ok();
        context.topic_context = topic_context;

        // 2. Save user message.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;
        context.conversation.messages.push(user_msg);

        // 2a. Compact dropped history. Once the conversation exceeds
        //     the visible window OR crosses the budget-pressure
        //     threshold (added 2026-05-25), the synthesis prompt
        //     would drop the oldest turns silently — coreference and
        //     topic anchors established in T0/T1 would vanish from
        //     view at T10+. Fast-slot summary preserves them as a
        //     compact preamble. Surfaced by
        //     sovereign/bench/wikipedia_learn 2026-05-17 marathon
        //     thread + the upcoming marathon_graceful bench.
        //
        //     session_id is None on this code path because
        //     `self.sessions.begin` doesn't run until further down —
        //     the narration chip is gated behind Some, so compaction
        //     still fires (and traces) but the user sees no chip on
        //     this entry point. The chip surface fires from the
        //     handler-level paths that have a session in scope.
        self.maybe_compact_dropped_history(&mut context, conversation_id, None)
            .await;

        // 2a.5. Retrieval-over-history spike (2026-05-26). Gated on
        //       SOVEREIGN_HISTORY_RETRIEVAL=1. Embeds prior turn pairs
        //       OUTSIDE the visible window, picks top-K cosine-near
        //       the current user message, stashes hits on the context
        //       for the renderer. Mechanism A/B vs the lossy-summary
        //       compaction arm — see `maybe_retrieve_relevant_history`.
        self.maybe_retrieve_relevant_history(&mut context, message)
            .await;

        // 2b. Tag the conversation with the skill that was active
        // when it started. The store upsert is idempotent — only
        // the first call with a non-NULL skill wins, later calls
        // are no-ops. The KnowledgeView conversational acquirer
        // reads this column to exclude `privacy = local_only`
        // skills (e.g. `inner-work`) from the shared corpus.
        if let Some(skill_id) = self.skills.primary_skill_id_for_conversation() {
            if let Err(e) = self
                .store
                .set_conversation_skill_if_unset(conversation_id, &skill_id)
                .await
            {
                tracing::debug!(
                    conversation_id,
                    error = %e,
                    "failed to tag conversation with skill_id; continuing"
                );
            }
        }

        // ── Deterministic wellbeing gate (streaming surface) ────
        //
        // Mirror of the `handle_turn` gate — see `runtime/wellbeing.rs`
        // for the contract and the inner-chaos receipts behind it. On
        // a Relational-register turn with a crisis signal, persist the
        // guaranteed caring + crisis-resource response and emit it as
        // a single stream frame; routing and synthesis never run.
        {
            let gate_mode = self.resolve_active_mode(conversation_id).await;
            let declared_register = gate_mode
                .as_deref()
                .and_then(|id| self.skills.skill_by_id(id))
                .map(|s| s.inference.register)
                .unwrap_or_default();
            let relational = declared_register == SkillRegister::Relational
                || gate_mode.as_deref() == Some(crate::intent_policy::MODE_INNER_WORK);
            if relational {
                if let Some(signal) = super::wellbeing::maybe_wellbeing_signal(
                    self.inference.as_ref(),
                    &context,
                    message,
                )
                .await
                {
                    tracing::info!(
                        trigger = signal.trigger,
                        first_fire = signal.first_fire,
                        "wellbeing gate: crisis signal — crisis-constrained synthesis + guaranteed resource floor (stream)"
                    );
                    let (content, mode) = super::wellbeing::crisis_response(
                        self.inference.as_ref(),
                        &context,
                        message,
                        &signal,
                    )
                    .await;
                    let assistant_msg = Message {
                        id: uuid::Uuid::new_v4().to_string(),
                        conversation_id: conversation_id.to_string(),
                        role: Role::Assistant,
                        content,
                        created_at: now(),
                        metadata: Some(super::wellbeing::wellbeing_metadata(&signal, mode)),
                        version: now(),
                    };
                    self.store.save_message(&assistant_msg).await?;
                    let message_id = assistant_msg.id.clone();
                    let content = assistant_msg.content.clone();
                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
                    let _ = tx.send(Ok(content)).await;
                    drop(tx);
                    let stream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
                    return Ok(StreamHandle { message_id, stream });
                }
            }
        }

        // 3. Route (or honour a preset classification from a
        // session-continuation call). When `preset` is `Some`, the
        // classifier call is skipped — the UI has already picked the
        // intent via `ClarificationCard` or `NextStepButtons`, and
        // re-classifying the same message would waste a Fast-slot
        // call and risk drifting from the user's explicit choice.
        //
        // Pre-classification narrowing: mode-only. The router sees
        // the broadest catalog the surface admits so classification
        // isn't artificially constrained by an as-yet-unknown
        // intent. Handlers downstream can re-narrow via
        // `narrow_tools_for_intent` once the intent is in hand.
        //
        // Resolve `active_mode` from the conversation tag BEFORE the
        // narrow so workspace-tagged conversations (recipe-author
        // being the load-bearing case) see their narrowed catalog
        // at classification time. The registry-side lookup inside
        // the plain `narrow_tools_pre_classification` misses the
        // conv-tag path; calling `_for_mode` with the resolved tag
        // is what prevents the router from picking generic tools
        // (e.g. `shell`) on a recipe-author turn. See decision note
        // 2026-05-23 for the silent-misroute history.
        let early_active_mode = self.resolve_active_mode(conversation_id).await;
        let tool_descriptors =
            self.narrow_tools_pre_classification_for_mode(early_active_mode.as_deref());
        let classification = if let Some(preset) = preset {
            preset
        } else {
            self.router
                .classify(message, &context, &tool_descriptors)
                .await?
        };

        // Apply routing policy. PR1 only reaches MoveKind::Commit in
        // the dispatcher; Propose/Ask are scaffolded by `decide_policy`
        // but the Runtime treats anything non-Commit as Commit until
        // PR2 wires the UI. We still log the policy so glassbox
        // observers (ARCH §0.1, §9.1) see which tier we'd be in.
        let policy = decide_policy(&classification, &self.confidence_thresholds);
        tracing::debug!(
            tier = ?policy.tier,
            move_kind = ?policy.move_kind,
            primary_intent = ?classification.primary.intent,
            confidence = classification.primary.confidence,
            thresholds_high = policy.thresholds_used.high,
            thresholds_moderate = policy.thresholds_used.moderate,
            "router:policy_applied"
        );

        // Begin an in-memory QuerySession covering this turn. Holds
        // the classification + policy + cancellation token. PR2 will
        // also cache retrieval and partial response here so a
        // `redirect_turn` can reuse work without re-searching.
        self.sessions.sweep_expired();
        let skill_id = self.skills.primary_skill_id_for_conversation();
        // The cancel token is LIVE plumbing, not bookkeeping: the
        // desktop's `cancel_stream` (and `redirect_turn`) cancels it,
        // and the streaming forward loops below select! on it —
        // terminating the turn with FinishReason::Cancelled and
        // dropping the provider stream (the embedded engine stops
        // decoding on receiver-drop). Before 2026-06-10 this binding
        // was discarded (`_cancel_token`) and cancel was a no-op:
        // "cancelled" turns ran to natural completion (harness note
        // df66cb8d).
        let (_session_id, cancel_token) = self.sessions.begin(
            conversation_id.to_string(),
            skill_id,
            message.to_string(),
            classification.clone(),
            policy.clone(),
        );

        // Destructure the classification fields we still thread as
        // diagnostics into downstream handlers. Preserving these
        // names keeps the handle_knowledge_query / handle_simple call
        // sites untouched so PR1 stays behaviour-preserving.
        //
        // Build the per-turn IntentPolicy and stash it on context
        // so downstream consumers read register/effective_intent
        // from one source of truth rather than re-querying
        // `SkillRegistry::primary_skill_register()` at ~16 sites.
        // The witness-intent override is now folded into
        // `intent_policy::policy_for`; the effective intent we
        // dispatch on is `policy.effective_intent`.
        let raw_intent = classification.primary.intent.clone();
        // Conversation-driven active mode was resolved early (above)
        // so the pre-classification narrow could consult it. Re-use
        // that resolution here rather than paying the
        // store.get_conversation round-trip a second time.
        let active_mode = early_active_mode.clone();
        let declared_register = active_mode
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .map(|s| s.inference.register)
            .unwrap_or_default();
        let intent_policy = crate::intent_policy::policy_for(
            &raw_intent,
            declared_register,
            active_mode.as_deref(),
        );
        let mut intent = intent_policy
            .effective_intent
            .clone()
            .unwrap_or_else(|| raw_intent.clone());
        context.intent_policy = Some(intent_policy);

        // Evidence escalation. A query routed to a non-retrieval type but that
        // is actually ABOUT the corpus's own facts — entity-anchored (names a
        // corpus entity) or corpus-deictic ("the story", "your sources") — needs
        // the evidence + agentic-formulation path, which lives only in
        // KnowledgeQuery. Escalate it so it gets retrieval, the "what do I need
        // to know to answer this" formulation round, and grounded synthesis (or
        // an honest abstention) — instead of a metalingual deflection with zero
        // retrieval. The router being imperfect is expected; this self-corrects
        // when the query plainly needs grounding. Measured 2026-06-18:
        // "According to Mr Vladimir, what 'sacrosanct fetish'…" routed
        // Metalingual on the quoted phrase and returned 0 chunks, though it is a
        // plain factual lookup. The deterministic checks are cheap (no
        // retrieval); KnowledgeQuery's own ground-or-abstain handles the rest, so
        // an over-escalation degrades to a normal grounded answer, never a leak.
        if matches!(intent, Intent::MetalingualQuery)
            && (crate::runtime::evidence_loop::compute_entity_anchored(
                message,
                context.conversation.enabled_corpora.as_deref(),
                &[],
            ) || crate::runtime::evidence_loop::question_is_corpus_deictic(message))
        {
            tracing::info!(
                from = ?intent,
                "router: escalating Metalingual → KnowledgeQuery (corpus-anchored query needs the evidence/formulation path)"
            );
            intent = Intent::KnowledgeQuery;
        }

        let coarse_intent = classification.coarse_intent.clone();
        let self_assessment = classification.self_assessment.clone();
        let scope = classification.scope.clone();

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            active_mode = ?active_mode,
            tier = ?policy.tier,
            "runtime: stream routed"
        );

        // Recipe-author workspace dispatch. When the conversation is
        // tagged with `recipe-author`, every meaningful turn is a
        // long-lived tool-using loop (draft → validate → test →
        // checkpoint) — wrong shape for the generic ComplexTask
        // planner that follows below, and the streaming ComplexTask
        // path was returning NotImplemented anyway (desktop sat in
        // loading state forever). Route to the agent-loop handler
        // here, BEFORE the ComplexTask bailout. The narrowed
        // `tool_descriptors` (from the pre-classification narrow
        // above with `early_active_mode`) already carries the
        // recipe-author tool catalog. See handlers/recipe_author.rs
        // for the loop shape and the 2026-05-23 history note.
        if matches!(
            active_mode.as_deref(),
            Some(crate::intent_policy::MODE_RECIPE_AUTHOR)
                | Some(crate::intent_policy::MODE_WORKFLOW_AUTHOR)
        ) {
            let skill_id = active_mode
                .as_deref()
                .unwrap_or(crate::intent_policy::MODE_RECIPE_AUTHOR);
            tracing::info!(
                intent = ?intent,
                skill_id,
                "runtime: dispatching authoring workspace turn to agent loop"
            );
            return self
                .handle_recipe_author_turn_stream(
                    skill_id,
                    message,
                    conversation_id,
                    &context,
                    &tool_descriptors,
                )
                .await;
        }

        // PR2 — Ask move. Suppress synthesis entirely, emit a
        // `clarification-request` event, save a placeholder assistant
        // message with the clarification metadata so the UI's
        // existing message-metadata listener can render the
        // ClarificationCard (same delivery path as retrieved_chunks).
        // Return an already-closed stream so the desktop relay exits
        // its token loop and promptly fires `message-complete`.
        if matches!(policy.move_kind, MoveKind::Ask) {
            return self
                .handle_ask_move_stream(message, conversation_id, &_session_id, &classification)
                .await;
        }

        // PR2 — Propose move. Emit an `interpretation-proposed` event
        // BEFORE any tokens flow, then fall through to the Commit
        // path so the Fast slot begins streaming immediately. The UI
        // renders the banner on the in-flight message; a subsequent
        // `redirect_turn` cancels the sampler via
        // `session.cancel.cancel()` and re-dispatches with an
        // alternative intent.
        if matches!(policy.move_kind, MoveKind::Propose) {
            let interpretation = format_interpretation(
                message,
                &classification.primary.intent,
                classification.rationale.as_deref(),
            );
            let alternatives = classification
                .alternatives
                .iter()
                .map(|a| ProposedAlternative {
                    label: label_for_intent(&a.intent),
                    intent_hint: intent_hint(&a.intent),
                })
                .collect();
            self.routing_events
                .emit_interpretation_proposed(InterpretationProposed {
                    session_id: _session_id.clone(),
                    conversation_id: conversation_id.to_string(),
                    interpretation,
                    alternatives,
                    confidence: classification.primary.confidence,
                })
                .await;
            tracing::info!(
                session_id = %_session_id,
                "routing:propose — banner emitted, continuing to Commit path"
            );
            // Fall through to Commit path — streaming begins below.
        }

        // ── Team-pipeline gate (Phase 4 of the situated-team plan) ──
        //
        // When `SOVEREIGN_TEAM_PIPELINE` is on AND the intent is one
        // the orchestrator handles end-to-end, route this turn through
        // `pipeline::run_team_pipeline` instead of the legacy
        // intent-specific dispatch below. Read the env var per-turn
        // (not at boot) so flipping it on a running daemon takes
        // effect immediately. Default-off until T2 bench validates
        // a default-on flip — see `pipeline::runner` for the
        // rationale and the constant to flip.
        //
        // Conation / Commissive / Expressive / ComplexTask /
        // MetalingualQuery keep the legacy path even when the
        // gate is on; their handlers depend on situated-skill
        // wiring that v1 of the orchestrator doesn't replicate.
        // Tool-calls and OICP/mesh peer routing reach `Runtime`
        // through different entry points and never hit this
        // branch (per plan §4.3).
        if crate::pipeline::is_team_pipeline_enabled()
            && matches!(
                intent,
                Intent::SimpleQuery
                    | Intent::DeepQuery
                    | Intent::KnowledgeQuery
                    | Intent::ComparisonQuery
                    | Intent::ExpressiveQuery
            )
        {
            tracing::info!(
                intent = ?intent,
                "team-pipeline: kill-switch enabled — routing turn through orchestrator"
            );
            let candidates = self.retrieve_candidates(message, &context, &intent).await;
            let register = context.turn_register();
            let witness_grounding = build_witness_grounding(&context, register);
            let inputs = crate::pipeline::TeamPipelineInputs {
                provider: Arc::clone(&self.inference),
                message,
                classification: &classification,
                register,
                candidates,
                max_tokens: crate::pipeline::DEFAULT_TEAM_PIPELINE_MAX_TOKENS,
                judge_enabled: true,
                witness_grounding,
            };
            let sink: Arc<dyn crate::pipeline::NarrationSink> =
                Arc::new(crate::pipeline::RoutingEventNarrationSink {
                    inner: Arc::clone(&self.routing_events),
                });
            let output = crate::pipeline::run_team_pipeline(
                inputs,
                sink,
                _session_id.clone(),
                conversation_id.to_string(),
            )
            .await?;
            let message_id = uuid::Uuid::new_v4().to_string();
            return Ok(StreamHandle {
                message_id,
                stream: output.stream,
            });
        }

        // Document attached or ComplexTask → fall back to non-streaming.
        // (KnowledgeQuery used to live here too, but that triggered a desktop
        // fallback that re-ran build_context + compress_working_memory +
        // update_topic_context + classify — ~17 seconds of pure duplicated
        // work. Instead we now run KnowledgeQuery inline below and emit the
        // response as a single stream chunk.)
        // ExpressiveQuery now has a streaming variant — see
        // `handle_expressive_query_stream`. Dispatch to it directly
        // before the document-fallback gate; the witness path
        // (Pass A → strip-thinking-stream → cleaned tokens) replaces
        // the prior NotImplemented + non-streaming-fallback dance.
        if matches!(intent, Intent::ExpressiveQuery) {
            tracing::info!(
                intent = ?intent,
                register = ?context.turn_register(),
                "runtime: dispatching ExpressiveQuery to streaming witness"
            );
            return self
                .handle_expressive_query_stream(message, conversation_id, &context)
                .await;
        }

        // Creative/generative requests ("tell me a story", "write a poem") stream
        // with a neutral prompt — NO retrieval, NO grounding gate. Routed here by
        // the router's creative heuristic instead of DeepQuery, which would
        // retrieve over every corpus and buffer every token behind the gate
        // (a 1.5-3.5min blank screen then a dump — 2026-06-26 breaker finding).
        if matches!(intent, Intent::GenerativeQuery) {
            tracing::info!(intent = ?intent, "runtime: dispatching GenerativeQuery to streaming");
            return self
                .handle_generative_query_stream(
                    message,
                    conversation_id,
                    &context,
                    cancel_token,
                )
                .await;
        }

        // Document-attached turns are owned by the document-operation path and
        // never reach the streaming surface for synthesis — keep the explicit
        // bail.
        if message.starts_with("[Document attached: ") {
            tracing::info!("runtime: document-attached stream — falling back");
            return Err(Error::NotImplemented(
                "Streaming not supported for document-attached turns".into(),
            ));
        }

        // These four intents don't token-stream, but they must NOT dead-end
        // with "Not implemented" (the streaming endpoint is the ONLY one both
        // apps use, so a follow-up like "can you continue?" — classified
        // Metalingual/Conation — would error). Run the handler with the context
        // we already built (no re-classification), persist its assistant
        // message so the WS Complete frame can project the metadata, and emit
        // the full answer as a single chunk through the same StreamHandle.
        if matches!(
            intent,
            Intent::ComplexTask
                | Intent::MetalingualQuery
                | Intent::ConationQuery
                | Intent::CommissiveQuery
        ) {
            tracing::info!(
                intent = ?intent,
                "runtime: non-streaming intent — single-chunk graceful fallback"
            );
            let response = match intent {
                Intent::MetalingualQuery => {
                    self.handle_metalingual_query(message, conversation_id, &context)
                        .await?
                }
                Intent::ConationQuery => {
                    self.handle_conation_query(message, conversation_id, &context)
                        .await?
                }
                Intent::CommissiveQuery => {
                    self.handle_commissive_query(message, conversation_id, &context)
                        .await?
                }
                Intent::ComplexTask => {
                    self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                        .await?
                }
                _ => unreachable!("matched above"),
            };
            // Persist the assistant message (the non-streaming handlers return a
            // Response for the caller to save — mirror handle_turn) so
            // `tr.message_metadata` in ws.rs finds it for the terminal frame.
            self.store.save_message(&response.message).await?;
            let message_id = response.message.id.clone();
            let content = response.message.content.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(1);
            let _ = tx.send(Ok(content)).await;
            drop(tx);
            let stream = Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
            return Ok(StreamHandle { message_id, stream });
        }

        // 3b. Splice KnowledgeView landscape digests now that routing
        // has resolved. The provider (typically the sovereign-tools
        // KnowledgeViewManager) reads the enriched indexes for each
        // built-in view and writes a markdown summary into
        // `context.knowledge_view_digests` so prompt assembly can
        // surface "here's the person's terrain" before synthesis.
        //
        // IMPORTANT: this MUST run before any intent-specific dispatch
        // (including the inline KnowledgeQuery path below). The final
        // prompt-assembly site asserts `knowledge_view_digests.is_some()`
        // as an invariant — running handle_knowledge_query without
        // splicing panics in types.rs.
        //
        // Pass the resolved primary active skill so the provider can
        // suppress cross-skill context when the active skill is
        // `privacy = "local_only"` (e.g. `inner-work` should not see
        // the conversational-history digest at all).
        if let Some(provider) = &self.landscape_digests {
            // Conversation-tag-driven active skill (2026-05-24
            // redesign): the digest suppression should follow the
            // surface that owns the conversation, not registry state.
            let active_skill = self.resolve_active_mode(conversation_id).await;
            provider
                .splice_landscape_digests(&mut context, active_skill.as_deref())
                .await;
        } else {
            // No provider installed — mark the invariant satisfied with
            // an empty digest set so the assert at the prompt-assembly
            // site doesn't fire. Matches the non-streaming path which
            // also runs through a provider or explicit empty default.
            context.set_landscape_digests(Vec::new());
        }

        // 3c. Ambient field_model — append a landscape digest for any
        // `field_skeleton`-built corpus the turn is scoped to (closes the
        // "compute_digests is view-fixed" gap; shared so bench/desktop/server
        // all gain it). No-op when unscoped or the corpus has no skeleton.
        self.splice_ambient_field_digests(&mut context).await;

        // R3 — temporal tension pre-pass. Active for relational
        // skills only; zero-cost no-op for factual skills.
        self.maybe_splice_temporal_tensions(&mut context, message)
            .await;

        // Tool-Mastery Layer 2 — compute the tool dossier so
        // `build_system_message` can splice it. No-op on relational
        // skills (the helper short-circuits) and when no NoteStore
        // is wired.
        self.maybe_compute_tool_dossier(&mut context, conversation_id)
            .await;

        // KnowledgeQuery: real streaming path. Prepare the synthesis
        // plan synchronously (retrieval + evidence-shape routing +
        // source expansion + request build + retrieved_chunks
        // summaries), then spawn a tokio task that drives
        // `complete_stream_with_id` and forwards each token to the
        // caller as it arrives. This replaces the old one-shot wrapper
        // which made the desktop chat window sit inert for ~35s while
        // the full response was assembled server-side.
        if matches!(intent, Intent::KnowledgeQuery | Intent::ComparisonQuery) {
            return self
                .stream_knowledge_query_turn(
                    message,
                    conversation_id,
                    context,
                    classification,
                    coarse_intent,
                    self_assessment,
                    scope,
                    intent,
                    _session_id,
                    cancel_token,
                    tool_descriptors,
                )
                .await;
        }

        // DeepQuery / SimpleQuery streaming path.
        self.stream_deep_query_turn(
            message,
            conversation_id,
            context,
            classification,
            coarse_intent,
            self_assessment,
            scope,
            intent,
            _session_id,
            cancel_token,
        )
        .await
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal inference stub: every `complete` returns the same canned reply
    /// and counts the calls, so the continuation loop is asserted
    /// deterministically — no model, no routing (which we can't steer to the
    /// kq path from a test anyway).
    struct ContinuationMock {
        reply: String,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::traits::InferenceProvider for ContinuationMock {
        async fn complete(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<crate::types::CompletionResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::types::CompletionResponse {
                text: self.reply.clone(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "continuation-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(crate::error::Error::NotImplemented(
                "continuation mock: no streaming".into(),
            ))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> crate::types::ProviderCapabilities {
            crate::types::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: crate::types::Depth::Moderate,
            }
        }
    }

    fn stub(reply: &str) -> (Arc<dyn crate::traits::InferenceProvider>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let inf: Arc<dyn crate::traits::InferenceProvider> = Arc::new(ContinuationMock {
            reply: reply.to_string(),
            calls: calls.clone(),
        });
        (inf, calls)
    }

    async fn run(inf: &Arc<dyn crate::traits::InferenceProvider>, full: &mut String) {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = crate::types::CompletionRequest::default();
        // gate_on=true: held flow, so no token is streamed to the (dropped) rx.
        continue_truncated_synthesis(inf, &req, &tx, &cancel, full, true, "test").await;
    }

    #[tokio::test]
    async fn lands_a_truncated_draft() {
        let (inf, calls) = stub(" man, and so the only cure is to control its effects.");
        let mut full =
            "Madison argues the latent causes of faction are sown into the nature of".to_string();
        assert!(crate::runtime::evidence::ends_mid_thought(&full));
        run(&inf, &mut full).await;
        assert!(
            !crate::runtime::evidence::ends_mid_thought(&full),
            "answer landed on a boundary: {full:?}"
        );
        assert!(
            full.ends_with("effects."),
            "continuation was stitched: {full:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one continuation sufficed");
    }

    #[tokio::test]
    async fn bounded_when_never_landing() {
        // A reply that itself ends mid-thought every time must NOT loop forever.
        let (inf, calls) = stub(" and then it just keeps trailing on and on without any end so");
        // Above the min-length guard, so the loop actually engages.
        let mut full = "This particular answer was unfortunately cut off right at the".to_string();
        run(&inf, &mut full).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "stops at the round cap, never loops"
        );
    }

    #[tokio::test]
    async fn skips_complete_or_short_drafts() {
        let (inf, calls) = stub(" extra text");
        let mut done = "A complete sentence.".to_string();
        run(&inf, &mut done).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a complete draft is left alone"
        );
        // A sub-threshold degenerate stub (e.g. a stray "search") is not "continued".
        let mut stubby = "search".to_string();
        run(&inf, &mut stubby).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0, "a tiny stub is left alone");
    }
}
