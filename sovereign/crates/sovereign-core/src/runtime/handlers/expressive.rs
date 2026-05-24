//! ExpressiveQuery dispatch — witness handler. Non-streaming
//! (`handle_expressive_query`) and streaming (`handle_expressive_query_stream`)
//! variants. Builds the compact relational system message, splices
//! recalled memories + temporal-tension cues, captures TurnProvenance
//! for the desktop inner-work surface.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;

use crate::error::Result;
use crate::traits::*;
use crate::types::*;

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
            .map(|m| m.content[..m.content.len().min(300)].to_string());

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
            let contradiction = self
                .detect_contradiction(message, &context.memories)
                .await;
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
                       1. Name what they said, specifically.\n\
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
        let response_text = {
            let no_thinking = crate::title::strip_thinking_response(&completion.text);
            crate::title::strip_source_citations(&no_thinking)
        };
        let response_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: response_text,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "intent": "ExpressiveQuery",
                "current_goal": current_goal,
                "had_prior_assistant": last_assistant.is_some(),
            })),
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
            .map(|m| m.content[..m.content.len().min(300)].to_string());

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
            let c = self
                .detect_contradiction(message, &context.memories)
                .await;
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
                       1. Name what they said, specifically.\n\
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
        };

        let _synth_start = std::time::Instant::now();
        let (inner_stream, model_id) = self
            .inference
            .complete_stream_with_id(&request)
            .await?;
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
                temporal_tensions,
                contradiction: contradiction_prov,
                current_goal: current_goal.clone(),
                recent_topic: recent_topic.clone(),
                last_assistant_excerpt: last_assistant.clone(),
                model_id: Some(model_id.clone()),
                max_tokens: request.max_tokens,
                enable_thinking: request.enable_thinking,
                pass_a_ms,
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
                        tracing::warn!(
                            error = err_msg,
                            "expressive_stream: inner stream errored"
                        );
                        return;
                    }
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
