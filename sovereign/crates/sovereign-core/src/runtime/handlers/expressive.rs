// SPDX-License-Identifier: AGPL-3.0-or-later
//! ExpressiveQuery dispatch — witness handler. Non-streaming
//! (`handle_expressive_query`) and streaming (`handle_expressive_query_stream`)
//! variants. Builds the compact relational system message, splices
//! recalled memories + temporal-tension cues, captures TurnProvenance
//! for the desktop inner-work surface.

use std::sync::Arc;

use crate::error::Result;

use super::super::*;

impl Runtime {
    /// Handle ExpressiveQuery: situated acknowledgment + targeted
    /// help-offer.
    ///
    /// Two prompt paths, branched on the active skill's register:
    ///
    /// * **Factual** — legacy ad-hoc prompt ("The user expressed how
    ///   they're feeling... SITUATED CONTEXT: ..."). Anchored to
    ///   `working_memory.current_goal` + last assistant turn so the
    ///   reply lands on the actual current work, not a generic pep
    ///   talk. No memory recall on this branch — the situated
    ///   handler never had it historically.
    ///
    /// * **Relational** — the witness contract
    ///   (`RELATIONAL_BASE_SYSTEM_PROMPT`) plus the FTS-retrieved
    ///   memories from the upstream `build_context` pass and any
    ///   temporal tensions surfaced by
    ///   `maybe_splice_temporal_tensions`. This is the wire that
    ///   makes RIGHT_DISAGREEMENT and contradiction-across-time
    ///   moves possible — without the memory section the model has
    ///   nothing to reach back into and falls into uncritical
    ///   validation. Voice-eval contradiction scenarios (06, 07,
    ///   10) exercise this path.
    pub(crate) async fn handle_expressive_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        let current_goal = context
            .working_memory
            .as_ref()
            .and_then(|wm| wm.current_goal.clone());
        let recent_topic = context
            .topic_context
            .as_ref()
            .and_then(|tc| tc.topic.clone());
        let last_assistant: Option<String> = context
            .conversation
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            // char-safe: byte slice panics mid-multibyte-char (CJK/emoji/RTL).
            .map(|m| crate::runtime::truncate_chars(&m.content, 300));

        let goal_str = current_goal
            .as_deref()
            .or(recent_topic.as_deref())
            .unwrap_or("unspecified");
        let tried_str = last_assistant
            .as_deref()
            .unwrap_or("no prior turn in this conversation");

        // System-prompt selection. The Expressive synthesis path
        // historically built its OWN ad-hoc prompt (third-person
        // framing + `SITUATED CONTEXT:` block + `If current_goal is
        // 'unspecified'…` conditional), which competes with — and
        // wins against — the witness contract on smaller models.
        // Voice-eval scenario 10 reproduces the exact failure: 9B
        // and 35B both echo the conditional rule back as their
        // response. When the active skill is Relational, route this
        // branch through `RELATIONAL_BASE_SYSTEM_PROMPT` so the
        // witness contract is the only voice in play; situated
        // context goes in as a brief observation block, not a rule
        // set the model is asked to evaluate.
        let register = context.turn_register();
        let mut metrics = RuntimeMetrics::default();
        let system = if register == SkillRegister::Relational {
            // Multi-shot Pass A: structured contradiction check. Soft-
            // fails to None — Pass B then proceeds without an explicit
            // "what may be missing" cue.
            let pass_a_start = std::time::Instant::now();
            let contradiction = self.detect_contradiction(message, &context.memories).await;
            metrics.pass_a_ms = Some(pass_a_start.elapsed().as_millis() as u64);

            let mut s = self.build_compact_relational_system_message(context, message);

            // Pass A → Pass B handoff. When the detector found a
            // concrete factual tension, name it explicitly in the
            // synthesis prompt so the model doesn't have to re-derive
            // it during its own planning. This is the lever that
            // turns RIGHT_DISAGREEMENT from hit-and-miss into
            // deterministic on the 9B fast slot.
            if let Some(c) = &contradiction {
                // When there IS a contradiction or pattern shift, the
                // reply has a real dialectic to develop: what they
                // said + what memory shows + an off-ramp question.
                // Surfacing the structure here lets the model carry
                // it cleanly. Gate it on `contradiction.is_some()` so
                // pure-uncertainty turns (no antithesis to surface)
                // stay brief — empirically (iter18 vs iter17 large
                // 2026-05-01) imposing dialectical structure on
                // every reply lifts substance axes but pushes simple
                // "I don't have enough" replies past the length cap.
                s.push_str(&format!(
                    "\n\nWhat may be missing from how they're framing this \
                     (offer once, kindly, as inquiry — easily dismissable):\n\
                     \u{2022} Prior: {prior}\n\
                     \u{2022} Now: {now}\n\
                     \n\
                     Three small moves, in order — not a template to recite:\n\
                       1. Speak to the specific thing they said — \
                          without opening by quoting it back.\n\
                       2. Surface the prior — name it, don't smooth it \
                          into agreement.\n\
                       3. Hand the decision back with one real question.",
                    prior = c.prior_evidence,
                    now = c.current_claim,
                ));
            }

            // Observation block — phrasing matches `render_temporal_tensions`
            // ("offer these as observations, easily dismissable") so the
            // model treats it as background, not a directive to recite.
            s.push_str("\n\nWhat may be in play (observation, easily dismissable):\n");
            s.push_str(&format!("  Current goal: {goal_str}\n"));
            s.push_str(&format!("  Recently tried: {tried_str}"));
            s
        } else {
            format!(
                "The user expressed how they're feeling about the current work.\n\
                 \n\
                 SITUATED CONTEXT:\n\
                 Current goal: {goal_str}\n\
                 Recently tried: {tried_str}\n\
                 \n\
                 Acknowledge briefly (one short sentence). Then offer ONE specific way to help, \
                 anchored to the current goal and what was just tried. End with ONE targeted \
                 question that would unblock you. Do not give a generic pep talk; do not minimize.\n\
                 \n\
                 If current_goal is 'unspecified' AND there is no prior turn, do not invent an \
                 offer. Say plainly that you don't have context loaded for what they're working on, \
                 and ask what they'd like to focus on. Epistemic honesty over confident-sounding \
                 improvisation."
            )
        };

        // Token + thinking-mode policy.
        //
        // Empirical finding (manual `/v1/chat/completions` probes
        // against Qwen3.5-9B-vOP, captured 2026-05-01): the
        // counter-intuitive setting on this fine-tune is
        // `enable_thinking: false`. With `false`, the model still
        // produces a planning trace (its training is dominated by
        // thinking-style data), but it reliably auto-emits
        // `</think>` to close the trace and then writes the actual
        // reply. With `true`, the chat template prepends `<think>`
        // to the assistant turn and the fine-tune *fails to close*
        // — it just keeps planning until `max_tokens`. The closer
        // is what `strip_thinking_response` keys on, so `false`
        // is the setting that lets the reply surface.
        //
        // Pinning `Some(false)` here documents the per-call
        // intent (rather than relying on the embedded default) so
        // that flipping the daemon-wide default later won't
        // silently break the witness path. Budget 1024 tokens to
        // cover the planning trace + reply on this 9B; the 35B
        // closes faster but uses the same envelope.
        //
        // Factual branch: keeps the legacy 256-token budget and
        // `None` (defers to embedded default of false), since the
        // legacy ad-hoc Expressive prompt was calibrated tight.
        let (max_tokens, enable_thinking) = if register == SkillRegister::Relational {
            // 2048-token budget: empirically a 9B fine-tune like
            // Qwen3.5-vOP spends ~800-1500 tokens of planning on the
            // full relational stack (witness contract + memories +
            // tensions) before it auto-closes `</think>` and writes
            // the reply. 1024 truncates the planning mid-sentence
            // and the close never fires. If this still truncates on
            // even larger prompts, simplifying the prompt itself is
            // the right move (the planning length is a signal that
            // the prompt is asking the model to juggle too many
            // things at once).
            (Some(2048), Some(false))
        } else {
            (Some(256), None)
        };
        // Witness work needs the bigger model. Relational register
        // routes to `Speed::Slow` (the primary slot — typically the
        // 35B that the iter3 prompt campaign was tuned against). The
        // Factual branch keeps the legacy `Speed::Fast` since the
        // ad-hoc Expressive prompt was calibrated tight on the Fast
        // slot. Until 2026-05-05 this was hardcoded `Speed::Fast`
        // unconditionally and silently served witness turns from the
        // 9B fast slot — see `handle_expressive_query_stream` for
        // the parallel fix and the bug report that surfaced it.
        let preferred_speed = if register == SkillRegister::Relational {
            Speed::Slow
        } else {
            Speed::Fast
        };
        let request = CompletionRequest {
            prompt: message.to_string(),
            system_message: Some(system),
            preferred_speed,
            max_tokens,
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(0),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
            ..Default::default()
        };
        let synth_start = std::time::Instant::now();
        let completion = self.inference.complete(&request).await?;
        metrics.synthesis_ms = Some(synth_start.elapsed().as_millis() as u64);
        // Strip the thinking trace before persisting the assistant
        // turn. With `enable_thinking: true` flipped on for the
        // relational branch, the chat template prepends `<think>` to
        // the assistant turn — so the model's output ends up shaped
        // `<planning text></think>\n\n<reply>`. Surfacing the planning
        // text in chat history would (a) leak the model's internal
        // reasoning to the user, (b) bias the next turn's context
        // toward "respond like a planner", and (c) bloat memory
        // recall hits with content that isn't a real reply. The
        // `strip_thinking_response` helper drops everything up to
        // and including the last `</think>` (and falls through to
        // `strip_think_blocks` for the no-tags case so the factual
        // branch is unaffected). Then `strip_source_citations`
        // removes any hallucinated `[Source: ...]` markers — see
        // its docs for why this code path needs that despite having
        // no corpus to cite from.
        let mut response_text = {
            let no_thinking = crate::title::strip_thinking_response(&completion.text);
            crate::title::strip_source_citations(&no_thinking)
        };

        // Memory-recall grounding gate (Relational only), borrowed from
        // the knowledge grounding gate and made BINDING: verify the
        // draft doesn't confabulate the user's past against the entries
        // the witness saw; on a flagged confabulation escalate through
        //   (1) a correction retry that names the unsupported detail and
        //       points back to the matching entry, re-verified; then
        //   (2) if it STILL confabulates, a claim-free reflective
        //       fallback that forbids any past-detail assertion — which
        //       is structurally incapable of misremembering.
        // Fails open on judge error (quality lever, never availability).
        // Verifies against the SAME entries the witness saw (rendered
        // top-K). Measured: the single correction pass cut confab
        // ~50%→~30%; the binding re-verify + claim-free floor is the
        // lever for the residual (2026-07-08 recall bench).
        let mut recall_verification: Option<crate::runtime::types::RecallVerificationProv> = None;
        if register == SkillRegister::Relational && !context.memories.is_empty() {
            use super::super::memory_grounding as mg;
            const VERIFY_CAP: usize = 3; // matches PROMPT_RENDER_CAP
            let seen = &context.memories[..context.memories.len().min(VERIFY_CAP)];
            let base_system = request.system_message.clone().unwrap_or_default();

            let strip = |raw: &str| -> String {
                let nt = crate::title::strip_thinking_response(raw);
                crate::title::strip_source_citations(&nt)
            };
            let regen = |system: String| {
                let mut retry = request.clone();
                retry.system_message = Some(system);
                retry
            };

            let v1 =
                mg::verify_recall_grounding(self.inference.as_ref(), message, &response_text, seen)
                    .await;
            let mut final_referenced = None;
            // Tracks the verdict that describes the FINAL response_text
            // (v1, or v2 after a correction retry) for the retained
            // provenance record below.
            let mut recall_verdict_state = (v1.grounded, v1.fail_open);
            if v1.grounded {
                final_referenced = v1.referenced;
                // False-denial correction: the draft is grounded but
                // told the user "I don't have that memory" while a
                // retrieved entry plausibly IS it — the dominant
                // surviving trust-breaker in the 2026-07-09 hand-read.
                // One retry that offers the entry as a question; the
                // retry is kept only if it re-verifies grounded, else
                // the original (honest-toned) denial stands.
                if let Some(idx) = v1.denied_match {
                    if let Some(m) = seen.get(idx.saturating_sub(1)) {
                        let sysd = format!("{base_system}{}", mg::denial_note(&m.content));
                        if let Ok(r) = self.inference.complete(&regen(sysd)).await {
                            let fixed = strip(&r.text);
                            if !fixed.trim().is_empty() {
                                let vd = mg::verify_recall_grounding(
                                    self.inference.as_ref(),
                                    message,
                                    &fixed,
                                    seen,
                                )
                                .await;
                                if vd.grounded {
                                    response_text = fixed;
                                    final_referenced = vd.referenced.or(Some(idx));
                                }
                            }
                        }
                    }
                }
            } else {
                // Stage 1 — correction retry.
                let sys1 = format!("{base_system}{}", mg::correction_note(&v1.unsupported));
                if let Ok(r) = self.inference.complete(&regen(sys1)).await {
                    let fixed = strip(&r.text);
                    if !fixed.trim().is_empty() {
                        response_text = fixed;
                    }
                }
                // Re-verify: the correction is not trusted blindly.
                let v2 = mg::verify_recall_grounding(
                    self.inference.as_ref(),
                    message,
                    &response_text,
                    seen,
                )
                .await;
                recall_verdict_state = (v2.grounded, v2.fail_open);
                if v2.grounded {
                    final_referenced = v2.referenced;
                } else {
                    // Stage 2 — claim-free reflective floor. Structurally
                    // cannot confabulate; the safe default when grounding
                    // fails twice.
                    let sys2 = format!("{base_system}{}", mg::no_recall_note());
                    if let Ok(r) = self.inference.complete(&regen(sys2)).await {
                        let fixed = strip(&r.text);
                        if !fixed.trim().is_empty() {
                            response_text = fixed;
                        }
                    }
                }
            }
            // Sticky pin: the entry the (grounded) reply actually spoke
            // about stays in view on later turns. Reference-driven, so
            // warmup chatter never pins noise (see `merge_recall_pins`).
            tracing::info!(
                target: "memory_grounding",
                grounded = v1.grounded,
                referenced = ?final_referenced,
                denied = ?v1.denied_match,
                seen = seen.len(),
                "recall gate verdict"
            );
            if let Some(idx) = final_referenced {
                if let Some(m) = seen.get(idx.saturating_sub(1)) {
                    tracing::info!(
                        target: "memory_grounding",
                        memory_id = %m.id,
                        "recall pin set"
                    );
                    self.pin_referenced_memory(conversation_id, &m.id);
                }
            }
            recall_verification = Some(crate::runtime::types::RecallVerificationProv {
                grounded: recall_verdict_state.0,
                fail_open: recall_verdict_state.1,
                referenced: final_referenced,
            });
        }

        // Glassbox parity with the streaming variant: capture
        // TurnProvenance on the non-streaming witness path too. The
        // desktop Cmd+? surface and the inner-chaos recall bench (which
        // must judge against the turn's ACTUAL retrieval window, not a
        // once-per-thread replica — v9 receipts, 2026-07-10) both read
        // this via `get_last_turn_provenance`.
        let message_id = uuid::Uuid::new_v4().to_string();
        // Epistemic ledger for the witness surface (EPISTEMIC_STATE §4.2/§5):
        // attached ONLY when the recall verifier attributed the reply to a
        // recalled entry — that turn genuinely asserts a memory, and the
        // footer renders the band chip ("From what you've told me", with
        // "remembered, not verified" on FailOpen). Un-attributed witness
        // turns assert no memory and stay ledger-less: deriving `Unverified`
        // there would render "used your sources" prose on a turn that used
        // none. Streaming witness runs no verifier (records None) and so
        // never attaches — extending the verifier there is the follow-up.
        let mut witness_epistemic_state: Option<crate::types::EpistemicState> = None;
        if register == SkillRegister::Relational {
            let recalled_memories: Vec<RecalledMemoryProv> = context
                .memories
                .iter()
                .map(|m| RecalledMemoryProv {
                    id: m.id.clone(),
                    content: m.content.clone(),
                    created_at: m.created_at,
                    kind: Some(
                        match m.kind {
                            crate::types::MemoryKind::Raw => "raw",
                            crate::types::MemoryKind::Summary => "summary",
                        }
                        .to_string(),
                    ),
                    source_memory_ids: m.source_memory_ids.clone(),
                    confidence: Some(m.confidence),
                })
                .collect();
            let history_summary = HistorySummaryProv {
                total_messages: context.conversation.messages.len(),
                user_count: context
                    .conversation
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::User)
                    .count(),
                assistant_count: context
                    .conversation
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::Assistant)
                    .count(),
                sent_to_model: Vec::new(),
            };
            let temporal_tensions = if context.temporal_tensions.is_empty() {
                Vec::new()
            } else {
                vec![render_temporal_tensions(&context.temporal_tensions)]
            };
            if crate::runtime::epistemic::epistemic_state_enabled()
                && recall_verification
                    .as_ref()
                    .is_some_and(|rv| rv.referenced.is_some())
            {
                witness_epistemic_state =
                    Some(crate::runtime::epistemic::assemble_epistemic_state(
                        crate::runtime::epistemic::EpistemicInputs {
                            recalled: &recalled_memories,
                            recall_verification: recall_verification.as_ref(),
                            ..Default::default()
                        },
                    ));
            }
            let prov_system = request.system_message.clone().unwrap_or_default();
            let provenance = TurnProvenance {
                conversation_id: conversation_id.to_string(),
                message_id: message_id.clone(),
                captured_at: now(),
                register: format!("{register:?}"),
                user_message: message.to_string(),
                system_prompt_chars: prov_system.chars().count(),
                system_prompt: prov_system,
                recalled_memories,
                history_summary,
                history_recall: HistoryRecallProv::from_context(context),
                temporal_tensions,
                contradiction: None,
                current_goal: current_goal.clone(),
                recent_topic: recent_topic.clone(),
                last_assistant_excerpt: last_assistant.clone(),
                model_id: None,
                max_tokens: request.max_tokens,
                enable_thinking: request.enable_thinking,
                pass_a_ms: metrics.pass_a_ms,
                recall_verification,
            };
            if let Ok(mut guard) = self.turn_provenance.write() {
                guard.insert(conversation_id.to_string(), provenance);
            } else {
                tracing::warn!("expressive: turn_provenance lock poisoned, skipping capture");
            }
        }

        let response_msg = Message {
            id: message_id,
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: response_text,
            created_at: now(),
            metadata: Some({
                let mut m = serde_json::json!({
                    "intent": "ExpressiveQuery",
                    "current_goal": current_goal,
                    "had_prior_assistant": last_assistant.is_some(),
                });
                // Memory-attributed witness turns carry the ledger so the
                // footer renders the memory chip + band (I3) — see the
                // gating note above `witness_epistemic_state`.
                if let Some(state) = &witness_epistemic_state {
                    m["epistemic_state"] =
                        serde_json::to_value(state).unwrap_or(serde_json::Value::Null);
                }
                m
            }),
            version: 0,
        };
        Ok(Response {
            message: response_msg,
            task: None,
            metrics: if register == SkillRegister::Relational {
                Some(metrics)
            } else {
                None
            },
        })
    }
    /// Streaming counterpart to [`Self::handle_expressive_query`].
    ///
    /// Same Pass A + system-prompt assembly as the non-streaming
    /// sibling, but the synthesis call is `complete_stream_with_id`
    /// instead of `complete`. The inner stream is wrapped with
    /// [`crate::title::strip_thinking_stream`] so the user sees only
    /// the post-`</think>` reply tokens — planning is buffered
    /// silently. On stream close the assistant message is persisted
    /// with the same metadata shape the non-streaming path emits
    /// (`intent`, `current_goal`, `had_prior_assistant`, plus
    /// `recalled_memories` when the relational register is active).
    ///
    /// Returns a [`StreamHandle`] whose `stream` yields cleaned
    /// reply chunks; the consumer must drain it. The persistence +
    /// metadata-emit happens in a spawned task that joins behind the
    /// stream — when the stream closes naturally the task finishes,
    /// when the consumer drops the handle the spawned task drops too.
    pub(crate) async fn handle_expressive_query_stream(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<StreamHandle> {
        let current_goal = context
            .working_memory
            .as_ref()
            .and_then(|wm| wm.current_goal.clone());
        let recent_topic = context
            .topic_context
            .as_ref()
            .and_then(|tc| tc.topic.clone());
        let last_assistant: Option<String> = context
            .conversation
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            // char-safe: byte slice panics mid-multibyte-char (CJK/emoji/RTL).
            .map(|m| crate::runtime::truncate_chars(&m.content, 300));

        let goal_str = current_goal
            .as_deref()
            .or(recent_topic.as_deref())
            .unwrap_or("unspecified");
        let tried_str = last_assistant
            .as_deref()
            .unwrap_or("no prior turn in this conversation");

        let register = context.turn_register();

        // Capture the recalled memories the witness drew on so the
        // metadata trail mirrors the non-streaming path. Inner-work's
        // desktop surface uses this for echo dots; matching the shape
        // here keeps streaming/non-streaming UX consistent.
        let recalled_memories_for_metadata: Option<serde_json::Value> =
            if register == SkillRegister::Relational {
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

        // Pass A is hoisted out of the `system` arm so the
        // contradiction result + timing are also available to the
        // provenance capture below. The detector is no-op when the
        // register isn't Relational (the branch below ignores its
        // result), so the call is gated to keep the latency cost
        // off the factual branch.
        let (contradiction, pass_a_ms) = if register == SkillRegister::Relational {
            let pass_a_start = std::time::Instant::now();
            let c = self.detect_contradiction(message, &context.memories).await;
            let elapsed = pass_a_start.elapsed().as_millis() as u64;
            tracing::info!(
                pass_a_ms = elapsed,
                contradiction_found = c.is_some(),
                "expressive_stream: pass A complete"
            );
            (c, Some(elapsed))
        } else {
            (None, None)
        };

        let system = if register == SkillRegister::Relational {
            let mut s = self.build_compact_relational_system_message(context, message);
            if let Some(c) = &contradiction {
                s.push_str(&format!(
                    "\n\nWhat may be missing from how they're framing this \
                     (offer once, kindly, as inquiry — easily dismissable):\n\
                     \u{2022} Prior: {prior}\n\
                     \u{2022} Now: {now}\n\
                     \n\
                     Three small moves, in order — not a template to recite:\n\
                       1. Speak to the specific thing they said — \
                          without opening by quoting it back.\n\
                       2. Surface the prior — name it, don't smooth it \
                          into agreement.\n\
                       3. Hand the decision back with one real question.",
                    prior = c.prior_evidence,
                    now = c.current_claim,
                ));
            }
            s.push_str("\n\nWhat may be in play (observation, easily dismissable):\n");
            s.push_str(&format!("  Current goal: {goal_str}\n"));
            s.push_str(&format!("  Recently tried: {tried_str}"));
            s
        } else {
            format!(
                "The user expressed how they're feeling about the current work.\n\
                 \n\
                 SITUATED CONTEXT:\n\
                 Current goal: {goal_str}\n\
                 Recently tried: {tried_str}\n\
                 \n\
                 Acknowledge briefly (one short sentence). Then offer ONE specific way to help, \
                 anchored to the current goal and what was just tried. End with ONE targeted \
                 question that would unblock you. Do not give a generic pep talk; do not minimize.\n\
                 \n\
                 If current_goal is 'unspecified' AND there is no prior turn, do not invent an \
                 offer. Say plainly that you don't have context loaded for what they're working on, \
                 and ask what they'd like to focus on. Epistemic honesty over confident-sounding \
                 improvisation."
            )
        };

        let (max_tokens, enable_thinking) = if register == SkillRegister::Relational {
            (Some(2048), Some(false))
        } else {
            (Some(256), None)
        };
        // See parallel comment in `handle_expressive_query`: the
        // witness contract was tuned against the 35B primary slot,
        // and the streaming path was silently serving witness turns
        // from the 9B fast slot before this routed to `Speed::Slow`
        // for Relational. Provenance from a 2026-05-05 inner-work
        // turn surfaced model_id=Qwen3.5-9B-vOP.Q5_K_S despite the
        // skill carrying `latency_class = "extended"` — the dispatch
        // layer wasn't reading the skill, so we encode the rule
        // (Relational → Slow) here directly.
        let preferred_speed = if register == SkillRegister::Relational {
            Speed::Slow
        } else {
            Speed::Fast
        };
        let request = CompletionRequest {
            prompt: message.to_string(),
            system_message: Some(system),
            preferred_speed,
            max_tokens,
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(0),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
            ..Default::default()
        };

        let _synth_start = std::time::Instant::now();
        let (inner_stream, model_id) = self.inference.complete_stream_with_id(&request).await?;
        // Compose: strip the planning trace first (drops content up
        // to `</think>`), then strip any hallucinated `[Source: ...]`
        // citation markers from the reply tokens. Both transformers
        // are streaming — see their docs for the streaming
        // composition rationale.
        let cleaned_stream = crate::title::strip_source_citations_stream(
            crate::title::strip_thinking_stream(inner_stream),
        );

        // Pre-spawn provenance capture. The message_id is minted in
        // the spawn block below; we mint it here instead so the
        // provenance can carry the same id the assistant Message will
        // be persisted under, letting the desktop correlate "the
        // response on screen" with "what the model saw to produce
        // it." See [`TurnProvenance`] for the rationale on what this
        // captures and why.
        let message_id = uuid::Uuid::new_v4().to_string();
        {
            let total = context.conversation.messages.len();
            let user_count = context
                .conversation
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count();
            let assistant_count = context
                .conversation
                .messages
                .iter()
                .filter(|m| m.role == Role::Assistant)
                .count();
            // The streaming witness path passes only `prompt: message`
            // and the assembled system. No prior turns flow to the
            // model. `sent_to_model` therefore stays empty here, by
            // design — the empty list is the diagnostic signal we
            // want surfaced. When history-injection is wired, push
            // the actual entries into this vector.
            let history_summary = HistorySummaryProv {
                total_messages: total,
                user_count,
                assistant_count,
                sent_to_model: Vec::new(),
            };

            let recalled_memories: Vec<RecalledMemoryProv> = context
                .memories
                .iter()
                .map(|m| RecalledMemoryProv {
                    id: m.id.clone(),
                    content: m.content.clone(),
                    created_at: m.created_at,
                    kind: Some(
                        match m.kind {
                            crate::types::MemoryKind::Raw => "raw",
                            crate::types::MemoryKind::Summary => "summary",
                        }
                        .to_string(),
                    ),
                    source_memory_ids: m.source_memory_ids.clone(),
                    confidence: Some(m.confidence),
                })
                .collect();

            let temporal_tensions = if context.temporal_tensions.is_empty() {
                Vec::new()
            } else {
                vec![render_temporal_tensions(&context.temporal_tensions)]
            };

            let contradiction_prov = contradiction.as_ref().map(|c| ContradictionProv {
                prior_evidence: c.prior_evidence.clone(),
                current_claim: c.current_claim.clone(),
            });

            let prov_system = request.system_message.clone().unwrap_or_default();
            let provenance = TurnProvenance {
                conversation_id: conversation_id.to_string(),
                message_id: message_id.clone(),
                captured_at: now(),
                register: format!("{register:?}"),
                user_message: message.to_string(),
                system_prompt_chars: prov_system.chars().count(),
                system_prompt: prov_system,
                recalled_memories,
                history_summary,
                history_recall: HistoryRecallProv::from_context(context),
                temporal_tensions,
                contradiction: contradiction_prov,
                current_goal: current_goal.clone(),
                recent_topic: recent_topic.clone(),
                last_assistant_excerpt: last_assistant.clone(),
                model_id: Some(model_id.clone()),
                max_tokens: request.max_tokens,
                enable_thinking: request.enable_thinking,
                pass_a_ms,
                // The recall verifier runs only on the NON-streaming
                // witness path today; `None` here is accurate, not a
                // gap — the ledger records these recalls Unverified.
                recall_verification: None,
            };
            if let Ok(mut guard) = self.turn_provenance.write() {
                guard.insert(conversation_id.to_string(), provenance);
            } else {
                tracing::warn!(
                    "expressive_stream: turn_provenance lock poisoned, skipping capture"
                );
            }
        }

        // Spawn a pump that:
        //   1. Forwards cleaned chunks to the consumer via mpsc
        //   2. Accumulates the full text for persistence
        //   3. On stream close, writes the assistant Message + emits
        //      the same metadata shape the non-streaming path uses
        //
        // The `message_id` is the one minted earlier for the
        // provenance capture, so the StreamHandle, the persisted
        // assistant Message, and the TurnProvenance entry all share a
        // single id for cross-correlation.
        let store = Arc::clone(&self.store);
        let conversation_id_owned = conversation_id.to_string();
        let message_id_for_persist = message_id.clone();
        // Post-stream recall verification inputs (F2, EPISTEMIC_STATE P2):
        // the non-streaming witness verifies BEFORE replying; here the
        // tokens have already shipped, so verification runs after stream
        // close and its outcome lands on the persisted metadata (the
        // epistemic ledger) + TurnProvenance — visible provenance, no
        // regen ladder on this path (v1): an ungrounded recall records
        // `FailedOnce` instead of silently dressing as verified.
        let verify_inference = Arc::clone(&self.inference);
        let turn_prov_for_spawn = Arc::clone(&self.turn_provenance);
        let is_relational = register == SkillRegister::Relational;
        let verify_memories: Vec<crate::types::Memory> = if is_relational {
            const VERIFY_CAP: usize = 3; // matches PROMPT_RENDER_CAP
            context.memories[..context.memories.len().min(VERIFY_CAP)].to_vec()
        } else {
            Vec::new()
        };
        let recalled_for_ledger: Vec<RecalledMemoryProv> = if is_relational {
            context
                .memories
                .iter()
                .map(|m| RecalledMemoryProv {
                    id: m.id.clone(),
                    content: m.content.clone(),
                    created_at: m.created_at,
                    kind: Some(
                        match m.kind {
                            crate::types::MemoryKind::Raw => "raw",
                            crate::types::MemoryKind::Summary => "summary",
                        }
                        .to_string(),
                    ),
                    source_memory_ids: m.source_memory_ids.clone(),
                    confidence: Some(m.confidence),
                })
                .collect()
        } else {
            Vec::new()
        };
        let user_message_owned = message.to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<String>>();

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut s = cleaned_stream;
            let mut full_text = String::new();
            while let Some(item) = s.next().await {
                match item {
                    Ok(chunk) => {
                        full_text.push_str(&chunk);
                        if tx.send(Ok(chunk)).is_err() {
                            // Consumer dropped — abandon persistence
                            // since the user won't see the result anyway.
                            tracing::debug!(
                                "expressive_stream: consumer dropped, skipping persist"
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("{e}");
                        let _ = tx.send(Err(e));
                        tracing::warn!(error = err_msg, "expressive_stream: inner stream errored");
                        return;
                    }
                }
            }

            // Post-stream recall verification + ledger (see the input
            // block above the spawn). Same verifier, same VERIFY_CAP
            // window as the non-streaming witness.
            let mut witness_epistemic_state: Option<crate::types::EpistemicState> = None;
            if is_relational
                && !verify_memories.is_empty()
                && crate::runtime::epistemic::epistemic_state_enabled()
            {
                use crate::runtime::memory_grounding as mg;
                let vd = mg::verify_recall_grounding(
                    verify_inference.as_ref(),
                    &user_message_owned,
                    &full_text,
                    &verify_memories,
                )
                .await;
                let rv = crate::runtime::types::RecallVerificationProv {
                    grounded: vd.grounded,
                    fail_open: vd.fail_open,
                    referenced: vd.referenced,
                };
                tracing::info!(
                    target: "epistemic.ledger",
                    grounded = rv.grounded,
                    fail_open = rv.fail_open,
                    referenced = ?rv.referenced,
                    "streaming witness: post-stream recall verification"
                );
                // Update the turn's provenance capture (guarded on the
                // message id — a newer turn may have overwritten the
                // conversation slot while we verified).
                if let Ok(mut guard) = turn_prov_for_spawn.write() {
                    if let Some(p) = guard.get_mut(&conversation_id_owned) {
                        if p.message_id == message_id_for_persist {
                            p.recall_verification = Some(rv.clone());
                        }
                    }
                }
                // Memory-attributed turns carry the ledger (same gating
                // as the non-streaming witness: un-attributed turns stay
                // ledger-less — deriving `Unverified` there would render
                // "used your sources" prose on a turn that used none).
                if rv.referenced.is_some() {
                    witness_epistemic_state =
                        Some(crate::runtime::epistemic::assemble_epistemic_state(
                            crate::runtime::epistemic::EpistemicInputs {
                                recalled: &recalled_for_ledger,
                                recall_verification: Some(&rv),
                                ..Default::default()
                            },
                        ));
                }
            }

            let mut metadata = serde_json::json!({
                "intent": "ExpressiveQuery",
                "current_goal": current_goal,
                "had_prior_assistant": last_assistant.is_some(),
            });
            if let Some(mem) = recalled_memories_for_metadata {
                if let serde_json::Value::Object(ref mut map) = metadata {
                    map.insert("recalled_memories".to_string(), mem);
                }
            }
            if let Some(state) = &witness_epistemic_state {
                if let serde_json::Value::Object(ref mut map) = metadata {
                    map.insert(
                        "epistemic_state".to_string(),
                        serde_json::to_value(state).unwrap_or(serde_json::Value::Null),
                    );
                }
            }

            let assistant_msg = Message {
                id: message_id_for_persist,
                conversation_id: conversation_id_owned,
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: Some(metadata),
                version: 0,
            };
            if let Err(e) = store.save_message(&assistant_msg).await {
                tracing::warn!(error = %e, "expressive_stream: persist failed");
            }
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(StreamHandle {
            message_id,
            stream: Box::pin(stream),
        })
    }
}
