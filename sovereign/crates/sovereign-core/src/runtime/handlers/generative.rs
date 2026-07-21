// SPDX-License-Identifier: AGPL-3.0-or-later
//! GenerativeQuery dispatch — creative/generative content handler.
//!
//! Creative requests ("tell me a story", "write a poem", "compose a letter",
//! "brainstorm names") need NO corpus retrieval, NO grounding gate, NO tools,
//! and NO situated/relational framing — just a capable model STREAMING the
//! requested piece behind a neutral creative system prompt. The router
//! classifies these to `GenerativeQuery` semantically — the embed router
//! (`router_embed.rs`, k-NN over `generative_query` exemplars) owns the
//! high-confidence cases, the LLM coarse `GENERATIVE` category catches the
//! rest — keeping them OFF the DeepQuery path, which would otherwise retrieve
//! over every installed corpus and buffer every token behind the grounding
//! gate: a 1.5–3.5 min blank screen then a dump grounded in irrelevant corpora
//! (2026-06-26 breaker finding).
//!
//! This is the sibling of `handle_expressive_query_stream` MINUS the
//! working-memory / emotive scaffolding: no Pass-A contradiction detection, no
//! recalled-memory splice, no situated/relational framing — a creative ask
//! carries no context to ground in. Streaming + persistence are identical.
//!
//! It DOES record a minimal `ResponseProvenance` on persist: the intent, the
//! backend, and — load-bearing — the `finish_reason`, so the desktop can tell
//! a natural finish from a user cancel. The pump select!s on the session's
//! cancellation token, so a "Stop" click terminates the generation promptly
//! (dropping the provider stream stops the engine on receiver-drop) and the
//! persisted turn honestly reads `finish_reason: "cancelled"` rather than
//! running to EOS and reporting `"stop"`.

use std::sync::Arc;

use crate::error::Result;

use super::super::*;

/// Neutral creative system prompt. Deliberately free of the
/// situated/relational framing the witness path uses: a creative request wants
/// the work itself, not an offer to help or a question back.
const GENERATIVE_SYSTEM_PROMPT: &str = "You are a skilled, versatile writer and creative thinker. \
     Fulfil the user's creative or generative request directly and vividly — write the requested \
     piece itself (a story, poem, letter, dialogue, list of ideas, …) in full. No meta-commentary, \
     no preamble about what you're about to do, no offers to help, and no questions back unless the \
     request is genuinely impossible to attempt.";

impl Runtime {
    /// Handle GenerativeQuery (streaming): stream the requested creative piece
    /// with a neutral prompt. No retrieval, no gate, no tools — tokens flow to
    /// the consumer as the model generates them, so a long piece shows progress
    /// immediately instead of buffering. Mirrors the Expressive streaming
    /// persistence: a spawned pump forwards chunks + writes the assistant
    /// message on stream close, sharing the minted `message_id`.
    pub(crate) async fn handle_generative_query_stream(
        &self,
        message: &str,
        conversation_id: &str,
        _context: &ConversationContext,
        // The session's cancellation token (from `sessions.begin`). The
        // desktop's `cancel_stream` cancels it; the pump below select!s on
        // it so a "Stop" click terminates the creative generation NOW
        // instead of running to natural EOS. Before this was threaded, the
        // generative path was the ONE streaming surface that ignored cancel
        // — the pump looped on `s.next()` with no cancel arm, so a cancelled
        // long story ran to completion (finish_reason came back "stop", not
        // "cancelled"; real-cancel-stream regression, 2026-07-07).
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<StreamHandle> {
        let request = CompletionRequest {
            prompt: message.to_string(),
            system_message: Some(GENERATIVE_SYSTEM_PROMPT.to_string()),
            // Primary slot for creative quality (the witness path's rationale
            // for Slow on extended turns applies equally to a long story).
            preferred_speed: Speed::Slow,
            max_tokens: Some(2048),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(0),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: Some(false),
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
        };

        let (inner_stream, model_id) = self.inference.complete_stream_with_id(&request).await?;
        // Strip the planning trace, then any hallucinated `[Source: ...]`
        // markers — same streaming composition as the witness path.
        let cleaned_stream = crate::title::strip_source_citations_stream(
            crate::title::strip_thinking_stream(inner_stream),
        );

        let message_id = uuid::Uuid::new_v4().to_string();
        let store = Arc::clone(&self.store);
        let conversation_id_owned = conversation_id.to_string();
        let message_id_for_persist = message_id.clone();
        let context_window = self.inference.effective_context_size();
        let max_tokens_budget = request.max_tokens.map(|t| t);
        let started = std::time::Instant::now();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<String>>();

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut s = cleaned_stream;
            let mut full_text = String::new();
            // Race the token stream against cancellation, biased so a cancel
            // that arrives with buffered tokens still wins — the user asked
            // us to stop NOW. Breaking the loop drops `s`, which closes the
            // provider channel; the embedded engine stops decoding on the
            // failed send (same mechanism as run_synthesis_stream).
            let finish_reason = loop {
                tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        tracing::info!(
                            chars = full_text.chars().count(),
                            "generative_stream: cancelled by session token — \
                             terminating with FinishReason::Cancelled"
                        );
                        break crate::types::FinishReason::Cancelled;
                    }
                    item = s.next() => match item {
                        Some(Ok(chunk)) => {
                            full_text.push_str(&chunk);
                            if tx.send(Ok(chunk)).is_err() {
                                // Consumer dropped — abandon persistence.
                                tracing::debug!(
                                    "generative_stream: consumer dropped, skipping persist"
                                );
                                return;
                            }
                        }
                        Some(Err(e)) => {
                            let err_msg = format!("{e}");
                            let _ = tx.send(Err(e));
                            tracing::warn!(
                                error = err_msg,
                                "generative_stream: inner stream errored"
                            );
                            return;
                        }
                        None => break crate::types::FinishReason::Stop,
                    },
                }
            };

            // Glassbox: the creative path carries no situated context to
            // ground in (see the module header), but it MUST still report
            // *how the turn ended* — a natural finish (Stop) versus a user
            // cancel (Cancelled) is the one piece of provenance a "Stop"
            // click depends on. Retrieval/grounding fields stay empty.
            let completion_tokens = (full_text.chars().count() / 4) as u32;
            let provenance = ResponseProvenance {
                intent: "GenerativeQuery".to_string(),
                search_method: None,
                sources: Vec::new(),
                inference_backend: model_id,
                oicp_match: None,
                total_latency_ms: started.elapsed().as_millis() as u64,
                tokens_used: completion_tokens as usize,
                coarse_intent: None,
                self_assessment: None,
                routing_trigger: None,
                coverage: None,
                finish_reason: Some(finish_reason),
                max_tokens_budget,
                completion_tokens: Some(completion_tokens),
                context_window,
            };
            let assistant_msg = Message {
                id: message_id_for_persist,
                conversation_id: conversation_id_owned,
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "GenerativeQuery",
                    "provenance": provenance,
                })),
                version: 0,
            };
            if let Err(e) = store.save_message(&assistant_msg).await {
                tracing::warn!(error = %e, "generative_stream: persist failed");
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

    /// Non-streaming fallback (the `handle_message` / `handle_turn` path). Drives
    /// the streaming handler and collects — so the persistence + neutral prompt
    /// stay single-sourced.
    pub(crate) async fn handle_generative_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        // Non-streaming callers have no session cancel token — pass a fresh
        // one that is never cancelled (the collect-to-completion loop below
        // owns the lifetime).
        let handle = self
            .handle_generative_query_stream(
                message,
                conversation_id,
                context,
                tokio_util::sync::CancellationToken::new(),
            )
            .await?;
        let message_id = handle.message_id.clone();
        let mut stream = handle.stream;
        let mut full_text = String::new();
        {
            use futures::StreamExt;
            while let Some(item) = stream.next().await {
                full_text.push_str(&item?);
            }
        }
        Ok(Response {
            message: Message {
                id: message_id,
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: Some(serde_json::json!({ "intent": "GenerativeQuery" })),
                version: 0,
            },
            task: None,
            metrics: None,
        })
    }
}
