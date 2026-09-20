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

/// The assistant-message metadata for a GenerativeQuery turn — ONE builder, so
/// the `Response` the non-streaming door returns carries exactly what the pump
/// persisted under the same `message_id`. Until this was single-sourced the
/// door minted `{"intent"}` of its own and the caller saw no provenance at all,
/// for a row that had one. `None` is the pump-published-nothing case (the
/// persist below failed): the key is absent rather than fabricated.
fn generative_metadata(provenance: Option<&ResponseProvenance>) -> serde_json::Value {
    let mut metadata = serde_json::json!({ "intent": "GenerativeQuery" });
    if let Some(provenance) = provenance {
        metadata["provenance"] = serde_json::json!(provenance);
    }
    metadata
}

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
        context: &ConversationContext,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<StreamHandle> {
        // A streaming consumer reads the persisted row by `message_id`, so it
        // has no use for the metadata receiver — dropping it closes the
        // channel and the pump's send is a no-op.
        let (handle, _metadata) = self
            .generative_stream(message, conversation_id, context, cancel_token)
            .await?;
        Ok(handle)
    }

    /// The generative stream itself, plus a one-shot that resolves to the
    /// metadata the pump PERSISTED for `handle.message_id`. Exists so the
    /// non-streaming door below can return what was stored instead of
    /// minting a second, thinner metadata object for the same row.
    async fn generative_stream(
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
    ) -> Result<(
        StreamHandle,
        tokio::sync::oneshot::Receiver<serde_json::Value>,
    )> {
        let request = CompletionRequest {
            admission: None,
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
            stable_prefix_len: None,
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
        let (metadata_tx, metadata_rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

        // Copied out before the spawn — `self` does not outlive the task, and a
        // `RouterStamp` is `Copy`, so the turn carries the router that routed it.
        let router_stamp = self.router.stamp();
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
                // Which classifiers were live behind this route; `None` from a router
                // that does not report. `routed_by_none()` marks a DEGRADED host.
                router: router_stamp,
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
            let metadata = generative_metadata(Some(&provenance));
            let assistant_msg = Message {
                id: message_id_for_persist,
                conversation_id: conversation_id_owned,
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: Some(metadata.clone()),
                version: 0,
            };
            match store.save_message(&assistant_msg).await {
                // Published only once the row is actually in the store, so a
                // caller that awaits this is told what WAS stored rather than
                // what we meant to store. The streaming door drops the
                // receiver, so a closed channel here is expected, not a fault.
                Ok(()) => {
                    let _ = metadata_tx.send(metadata);
                }
                Err(e) => tracing::warn!(error = %e, "generative_stream: persist failed"),
            }
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok((
            StreamHandle {
                message_id,
                stream: Box::pin(stream),
            },
            metadata_rx,
        ))
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
        let (handle, metadata_rx) = self
            .generative_stream(
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
        // The stream closes only when the pump's task has finished, so the
        // publish above has already happened or never will. `Err` is the
        // persist-failed path (which warns there): the reply then carries the
        // intent alone rather than a record of a row that is not in the store.
        let metadata = match metadata_rx.await {
            Ok(metadata) => metadata,
            Err(_) => {
                tracing::warn!(
                    message_id = %message_id,
                    "generative: nothing was persisted for this turn — replying without provenance"
                );
                generative_metadata(None)
            }
        };
        Ok(Response {
            message: Message {
                id: message_id,
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: Some(metadata),
                version: 0,
            },
            task: None,
            metrics: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;

    use crate::error::{Error, Result};
    use crate::registry::ToolRegistry;
    use crate::runtime::Runtime;
    use crate::skills::SkillRegistry;
    use crate::traits::{ConversationStore, InferenceProvider, StateStore};
    use crate::types::{
        CompletionRequest, CompletionResponse, Conversation, ConversationContext, Depth,
        ProviderCapabilities, ResponseProvenance, Speed,
    };

    /// Streams two fixed chunks and closes. The handler is what is under
    /// test; the model only has to end cleanly so the pump reaches its
    /// persist.
    struct StreamingStub;

    #[async_trait]
    impl InferenceProvider for StreamingStub {
        async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::NotImplemented(
                "the generative handler streams; `complete` is unused".into(),
            ))
        }

        async fn complete_stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok("Once upon ".to_string()),
                Ok("a time.".to_string()),
            ])))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.0; 8])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn empty_context() -> ConversationContext {
        ConversationContext {
            conversation: Conversation {
                id: "conv-generative".to_string(),
                title: None,
                messages: Vec::new(),
                created_at: 0,
                updated_at: 0,
                version: 0,
                deleted_at: None,
                skill_id: None,
                enabled_corpora: None,
                searched_sources: None,
            },
            memories: Vec::new(),
            working_memory: None,
            installed_corpora: vec![],
            corpus_ceiling: None,
            document_session: None,
            topic_context: None,
            knowledge_view_digests: None,
            temporal_tensions: Vec::new(),
            compacted_history: None,
            history_retrieval_hits: None,
            tool_dossier: None,
            intent_policy: None,
        }
    }

    /// The non-streaming door drives the streaming one and shares its
    /// `message_id`, so the `Response` it hands back and the row the pump
    /// persisted are two views of ONE turn. Until
    /// rb-generative-returns-what-it-stores the door minted
    /// `{"intent": "GenerativeQuery"}` of its own while the pump wrote the
    /// same id with a full `ResponseProvenance`: a caller reading the reply
    /// saw no backend, no finish reason and no latency for a turn that had
    /// recorded all three. The two metadata objects are now one object.
    #[tokio::test]
    async fn generative_response_metadata_matches_the_stored_row() {
        let store = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        let runtime = Runtime::new(crate::runtime::RuntimeParts::new(
            Arc::new(StreamingStub),
            Box::new(crate::stubs::PassthroughRouter),
            Box::new(crate::stubs::NoOpPlanner),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&store) as Arc<dyn StateStore>,
            Arc::new(SkillRegistry::new()),
            Arc::new(crate::executor::AutoApprovalChannel),
            crate::types::InferenceConfig::default(),
            crate::runtime::lane::LaneSources::none(),
        ));

        let response = runtime
            .handle_generative_query("tell me a story", "conv-generative", &empty_context())
            .await
            .expect("the generative door collects its own stream");

        let stored = store
            .get_conversation("conv-generative")
            .await
            .expect("the pump persisted the turn");
        let row = stored
            .messages
            .iter()
            .find(|m| m.id == response.message.id)
            .expect("the reply and the stored row share one message_id");

        assert_eq!(
            response.message.metadata, row.metadata,
            "the reply must return what the pump stored, not a thinner object \
             built beside it"
        );

        let provenance: ResponseProvenance = serde_json::from_value(
            response.message.metadata.as_ref().expect("metadata")["provenance"].clone(),
        )
        .expect("the returned metadata carries the record the row carries");
        assert_eq!(provenance.intent, "GenerativeQuery");
        assert!(
            provenance.finish_reason.is_some(),
            "the finish reason is the field a Stop click depends on: {:?}",
            provenance.finish_reason
        );
    }
}
