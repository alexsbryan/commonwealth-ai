// SPDX-License-Identifier: AGPL-3.0-or-later
//! SimpleQuery / DeepQuery dispatch — the witness path's primary
//! entry. Builds the (relational or factual) system message, splices
//! recalled memories, and emits a single non-streaming synthesis call.

use crate::error::Result;

use super::super::*;

impl Runtime {
    /// Handle SimpleQuery, DeepQuery, and other non-plan intents.
    /// Searches all knowledge sources before generating a response.
    pub(crate) async fn handle_simple(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        intent: &Intent,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
        routing_trigger: Option<String>,
        scope: Option<&str>,
    ) -> Result<Response> {
        // Search knowledge + build prompt (shared with handle_message_stream).
        let mut kc = self
            .prepare_knowledge_context(message, context, intent, scope)
            .await;

        // Witness override for relational + reasoning-shaped intents.
        // `prepare_knowledge_context` builds `kc.system` via
        // `build_primary_system_message`, which prepends the FULL
        // `RELATIONAL_BASE_SYSTEM_PROMPT` (4.5KB / ~1100 tokens) when
        // the active skill register is Relational. On 9B fast-slot
        // fine-tunes that prompt is too heavy to converge through —
        // see voice-eval scenario 07 (DeepQuery + inner-work) where
        // the model burns its planning budget on contract recitation.
        // Tighten the system message here to the compact contract
        // plus memories + tensions, and mirror the multi-shot
        // contradiction-detection that's already wired into
        // handle_expressive_query so the disagreement-as-inquiry
        // move is deterministic when the evidence supports it.
        //
        // Scope: only the reasoning-shaped intents that share a
        // synthesis pattern with Expressive (DeepQuery, the
        // generic-fallback `_` intents that landed in handle_simple
        // because there's no specialized handler). Trivial Q&A
        // (`SimpleQuery`) and continuations are deliberately
        // untouched — their existing prompt is already brief and
        // doesn't need the witness scaffolding.
        let register = context.turn_register();
        let want_witness_path =
            register == SkillRegister::Relational && matches!(intent, Intent::DeepQuery);
        let mut metrics = RuntimeMetrics::default();
        let (final_max_tokens, final_enable_thinking) = if want_witness_path {
            // Glassbox: name the memory and tension count entering the
            // witness path. Scenario 07-style contradiction failures
            // most commonly reduce to "FTS retrieval missed the seed
            // memory", which leaves the witness with nothing to surface.
            tracing::info!(
                memories = context.memories.len(),
                tensions = context.temporal_tensions.len(),
                first_memory_chars = context
                    .memories
                    .first()
                    .map(|m| m.content.len())
                    .unwrap_or(0),
                "witness:handle_simple deep_query relational entry"
            );
            let pass_a_start = std::time::Instant::now();
            let contradiction = self.detect_contradiction(message, &context.memories).await;
            metrics.pass_a_ms = Some(pass_a_start.elapsed().as_millis() as u64);
            tracing::info!(
                contradiction_present = contradiction.is_some(),
                "witness:handle_simple contradiction-check result"
            );
            let mut s = self.build_compact_relational_system_message(context, message);
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
            // Witness path uses a fixed 2048 budget; the compact
            // relational system message it just built doesn't carry
            // the budget directive that `prepare_knowledge_context`
            // splices for the non-witness path. Splice it here so the
            // model knows the budget it actually has, not the larger
            // configured one.
            let witness_budget = 2048usize;
            let budget_note = crate::runtime::build_response_length_directive(witness_budget);
            s.push_str(&format!("\n\n{budget_note}"));
            kc.system = s;
            // Mirror the Expressive relational budget: 2048 tokens
            // covers the planning-trace-then-close shape on the 9B,
            // and `enable_thinking: false` is the empirical setting
            // that triggers the auto-`</think>` close on Qwen3.5-vOP.
            (witness_budget, Some(false))
        } else {
            (self.inference_config.max_tokens, None)
        };

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(intent)
        };

        // Tier 2: same evidence_id_allowlist gather as the
        // streaming KQ path — see the comment block at the
        // streaming dispatch site for the rationale.
        let evidence_id_allowlist = self.gather_evidence_id_allowlist(conversation_id).await;
        // Shared synthesis-request core (`synthesis_common`); this
        // surface overrides retrieval-derived speed, the witness
        // budget/thinking pair, and the Tier-2 allowlist.
        let request = CompletionRequest {
            system_message: Some(kc.system),
            preferred_speed: kc.speed,
            max_tokens: Some(final_max_tokens),
            enable_thinking: final_enable_thinking,
            evidence_id_allowlist,
            ..self.synthesis_request(kc.prompt, oicp)
        };

        let synth_start = std::time::Instant::now();
        let completion = self.inference.complete(&request).await?;
        metrics.synthesis_ms = Some(synth_start.elapsed().as_millis() as u64);

        // When the witness path was active, strip the planning trace
        // before anything downstream (gap-check, response assembly,
        // persistence) sees it. Same convention as
        // handle_expressive_query — see `strip_thinking_response`
        // doc for the three response shapes it handles. No-op when
        // the response carries no `<think>` markers.
        //
        // Then drop any hallucinated `[Source: ...]` citation markers.
        // The witness has no corpus to cite from, but modern fine-
        // tunes sometimes emit RAG-formatted citations from their
        // training distribution when asked to ground in "the record."
        // We strip post-hoc rather than instructing the prompt — see
        // `strip_source_citations` for why prompting against the
        // behavior is itself counterproductive.
        let response_text = if want_witness_path {
            let no_thinking = crate::title::strip_thinking_response(&completion.text);
            crate::title::strip_source_citations(&no_thinking)
        } else {
            completion.text.clone()
        };

        // Production grounding gate (GateSurface::SimpleQuery,
        // env-gated default-off): fires only when retrieval actually
        // matched (kc.chunks non-empty) and the witness/relational
        // path is NOT active — witness moves are about memories and
        // dialectic, not corpus facts, and are explicitly out of gate
        // scope. Claim search is sealed to the conversation's corpora.
        let gate_surface = crate::runtime::grounding::GateSurface::SimpleQuery;
        let mut grounding_gate_meta: Option<serde_json::Value> = None;
        let response_text = if gate_surface.enabled() && !want_witness_path && !kc.chunks.is_empty()
        {
            let gate_evidence = crate::runtime::grounding::EvidenceContext {
                chunks: crate::runtime::grounding::gate_evidence_chunks(&kc.chunks),
                source_labels: crate::runtime::grounding::gate_evidence_source_labels(&kc.chunks),
                chunk_labels: crate::runtime::grounding::gate_evidence_chunk_labels(&kc.chunks),
                searcher: Some(std::sync::Arc::new(
                    self.claim_searcher(
                        context.conversation.enabled_corpora.as_deref(),
                        &kc.chunks,
                    ),
                ) as _),
                entity_anchored: crate::runtime::evidence_loop::question_is_corpus_deictic(message),
                top_similarity: None,
            };
            let outcome = crate::runtime::grounding::gate_answer(
                &self.inference,
                message,
                response_text.clone(),
                &gate_evidence,
                &request,
                &gate_surface.profile(),
            )
            .await;
            grounding_gate_meta = Some(outcome.meta);
            outcome.text
        } else {
            response_text
        };

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // No-ops when disabled. Evidence is the same formatted-chunks text
        // that was injected into the synthesis prompt (or empty if no
        // corpus material was retrieved).
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let final_content = self
            .maybe_collaborate(conversation_id, message, &response_text, &evidence)
            .await;

        // Completion-telemetry tail comes from the shared helper
        // (`synthesis_common`); only surface-varying fields here.
        let provenance = ResponseProvenance {
            search_method: kc.search_method,
            sources: kc.sources,
            coarse_intent,
            self_assessment,
            routing_trigger,
            coverage: kc.coverage,
            ..self.synthesis_provenance(format!("{intent:?}"), &completion)
        };

        // Phase 3b: include recalled memories on the relational
        // witness path so the desktop's inner-work surface can render
        // gutter echo dots. `want_witness_path` already gates this on
        // (Relational register + DeepQuery); reuse that signal so the
        // metadata stays tight elsewhere.
        let recalled_memories_metadata: Option<serde_json::Value> =
            if want_witness_path && !context.memories.is_empty() {
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

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some({
                let mut m = serde_json::json!({
                    "model": completion.model_id,
                    "tokens": completion.tokens_used,
                    "latency_ms": completion.latency_ms,
                    "provenance": provenance,
                    "retrieved_chunks": kc.retrieved_chunks,
                    "recalled_memories": recalled_memories_metadata,
                });
                if let Some(g) = grounding_gate_meta {
                    m["grounding_gate"] = g;
                }
                m
            }),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: None,
            metrics: if want_witness_path {
                Some(metrics)
            } else {
                None
            },
        })
    }
}
