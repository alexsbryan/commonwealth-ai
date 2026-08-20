// SPDX-License-Identifier: AGPL-3.0-or-later
//! DocumentOperation dispatch — surface for create/append/list/lookup
//! against the local DocumentStore. Synthesis-free; the operation
//! itself is the response.

use crate::error::Result;
use crate::slot_policy::Workload;

use super::super::*;

impl Runtime {
    /// Handle ComplexTask: plan → execute → (replan on failure) → synthesize.
    /// Handle document analysis: bypass planner, call document_operation directly.
    ///
    /// 1. Resolve the source path from the store
    /// 2. Generate map/reduce prompts with a single inference call
    /// 3. Call document_operation tool directly with deterministic params
    /// 4. Synthesize the result into a response
    pub(crate) async fn handle_document_operation(
        &self,
        source_hint: &str,
        user_query: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        tracing::info!(source_hint = %source_hint, "runtime: document_operation — resolving source");

        // 1. Resolve actual source path from the store.
        let sources = self.store.list_sources().await.unwrap_or_default();
        let source_lower = source_hint.to_lowercase();
        let resolved_source = sources
            .iter()
            .find(|s| s.to_lowercase().contains(&source_lower))
            .cloned()
            .unwrap_or_else(|| source_hint.to_string());

        tracing::debug!(
            resolved_source = %resolved_source,
            available_sources = sources.len(),
            "runtime: document_operation — source resolved"
        );

        // Get chunk count for the prompt.
        let chunks = self
            .store
            .get_chunks_by_source(&resolved_source)
            .await
            .unwrap_or_default();
        let chunk_count = chunks.len();
        let word_count: usize = chunks
            .iter()
            .map(|c| c.content.split_whitespace().count())
            .sum();
        drop(chunks);

        if chunk_count == 0 {
            tracing::warn!(
                source = %resolved_source,
                "runtime: document_operation — no chunks found for source"
            );
            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: format!(
                    "No document chunks found for '{}'. The document may not have been ingested correctly.",
                    source_hint
                ),
                created_at: now(),
                metadata: None,
                version: now(),
            };
            self.store.save_message(&assistant_msg).await?;
            self.spawn_auto_title(conversation_id);
            return Ok(Response {
                message: assistant_msg,
                task: None,
                metrics: None,
            });
        }

        tracing::info!(
            source = %resolved_source,
            chunks = chunk_count,
            words = word_count,
            user_query_chars = user_query.len(),
            "runtime: document_operation — generating map/reduce prompts"
        );

        // 2. Generate map/reduce prompts with a single focused inference call.
        let prompt = format!(
            "The user uploaded a document ({chunk_count} chunks, ~{word_count} words) and asked:\n\
                 \"{user_query}\"\n\n\
                 Write two prompts for a map-reduce analysis of this document.\n\n\
                 MAP PROMPT — applied to each chunk of the document:\n\
                 - Extract only what's present in that chunk\n\
                 - Produce structured notes relevant to the user's request\n\
                 - Do NOT invent or assume content not in the chunk\n\n\
                 REDUCE PROMPT — merges all extracted notes into one result:\n\
                 - Synthesize into a coherent, comprehensive answer\n\
                 - Deduplicate and organize logically\n\n\
                 Respond in JSON only:\n\
                 {{\"map_prompt\": \"...\", \"reduce_prompt\": \"...\"}}"
        );

        // SLOT_POLICY §3 Synthesize: map/reduce prompt generation on the
        // primary slot — the 0.6B fast model can't reliably produce this
        // JSON, so it's a deliberate primary call. Schema-constrained, so
        // think stays suppressed.
        let mut prompt_request = CompletionRequest::for_workload(Workload::Synthesize, prompt)
            .with_system("You write analysis prompts. Output ONLY the JSON object, nothing else.")
            .with_output_budget(512);
        prompt_request.temperature = Some(0.0);
        prompt_request.think_budget = Some(0); // no thinking — just produce the JSON
        prompt_request.structured_output = Some(serde_json::json!({
            "type": "object",
            "properties": {
                "map_prompt": { "type": "string" },
                "reduce_prompt": { "type": "string" }
            },
            "required": ["map_prompt", "reduce_prompt"]
        }));

        let prompt_response = self.inference.complete(&prompt_request).await?;
        let prompt_text = prompt_response.text.trim();

        // Parse the generated prompts. Strip think tags and code fences
        // before parsing — models often wrap JSON in these.
        let cleaned = prompt_text
            // Strip <think>...</think> blocks (Qwen3 thinking mode).
            .split("</think>")
            .last()
            .unwrap_or(prompt_text)
            .trim()
            // Strip markdown code fences.
            .strip_prefix("```json")
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(
                prompt_text
                    .split("</think>")
                    .last()
                    .unwrap_or(prompt_text)
                    .trim(),
            )
            .trim();

        let (map_prompt, reduce_prompt) = match serde_json::from_str::<serde_json::Value>(cleaned) {
            Ok(v) => {
                let mp = v.get("map_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Extract key information relevant to the user's question from this passage."
                ).to_string();
                let rp = v
                    .get("reduce_prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Synthesize all extracted information into a comprehensive answer.")
                    .to_string();
                (mp, rp)
            }
            Err(e) => {
                // Fallback: use specific prompts tailored to the user's question.
                tracing::warn!(
                    error = %e,
                    raw_output = %prompt_text,
                    "Failed to parse prompt JSON — using tailored fallback prompts"
                );
                (
                    format!(
                        "Read this passage carefully. The user asked: \"{user_query}\"\n\n\
                         Extract ALL information from this passage that is relevant to \
                         answering the user's question. Include:\n\
                         - Key facts, events, or arguments\n\
                         - Character names and their actions (if narrative)\n\
                         - Direct quotes that are significant\n\
                         If nothing relevant appears, respond with just: null"
                    ),
                    format!(
                        "The user asked: \"{user_query}\"\n\n\
                         You have been given extracted notes from across an entire document. \
                         Synthesize ALL the extracted information into a comprehensive, \
                         well-organized answer to the user's question. \
                         Be thorough — include every relevant detail from the notes. \
                         Organize logically with clear sections."
                    ),
                )
            }
        };

        tracing::debug!(
            map_prompt_chars = map_prompt.len(),
            reduce_prompt_chars = reduce_prompt.len(),
            "runtime: document_operation — prompts generated"
        );
        tracing::info!("runtime: document_operation — invoking map/reduce");

        // 3. Call document_operation tool directly.
        let tool = self.tools.get("document_operation")?;
        let params = serde_json::json!({
            "source": resolved_source,
            "operation": user_query,
            "map_prompt": map_prompt,
            "reduce_prompt": reduce_prompt,
            "conversation_id": conversation_id,
        });

        let tool_ctx = ToolContext {
            conversation_id: conversation_id.to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        };

        // Tool-activity narration — bracket the tool.execute call with
        // Start/Complete frames. Bypasses `try_emit_narration` (which
        // suppresses under 1.5s elapsed) because tool dispatch needs to
        // surface immediately for the "feels alive" UX. The call_id
        // correlates Start with Complete so the desktop can pair them
        // even if they arrive out of order with other narration frames.
        let call_id = uuid::Uuid::new_v4().to_string();
        let (session_id, session_elapsed_ms) = self
            .sessions
            .latest_for_conversation(conversation_id)
            .map(|s| (s.id, s.started_at.elapsed().as_millis() as u64))
            .unwrap_or_default();
        let tool_start = std::time::Instant::now();
        let resolved_label = std::path::Path::new(&resolved_source)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(resolved_source.as_str())
            .to_string();
        self.routing_events
            .emit_turn_narration(TurnNarration {
                session_id: session_id.clone(),
                conversation_id: conversation_id.to_string(),
                event: NarrationEvent {
                    phase: NarrationPhase::ToolInvocationStart {
                        call_id: call_id.clone(),
                        tool_id: "document_operation".to_string(),
                        summary: format!("Analyzing {resolved_label}"),
                    },
                    text: format!(
                        "Analyzing {resolved_label} — {chunk_count} chunk{} across {word_count} words",
                        if chunk_count == 1 { "" } else { "s" }
                    ),
                    elapsed_ms: session_elapsed_ms,
                },
            })
            .await;

        let exec_result = tool.execute(&params, &tool_ctx).await;
        let elapsed_after = session_elapsed_ms + tool_start.elapsed().as_millis() as u64;
        let (ok, result_summary) = match &exec_result {
            Ok(out) => {
                let chars = match out {
                    StepOutput::Text(t) => t.len(),
                    StepOutput::Json(v) => serde_json::to_string(v).map(|s| s.len()).unwrap_or(0),
                    _ => 0,
                };
                (
                    true,
                    format!("Synthesized {chars} chars from {chunk_count} chunks"),
                )
            }
            Err(e) => (false, format!("Document analysis failed: {e}")),
        };
        self.routing_events
            .emit_turn_narration(TurnNarration {
                session_id: session_id.clone(),
                conversation_id: conversation_id.to_string(),
                event: NarrationEvent {
                    phase: NarrationPhase::ToolInvocationComplete {
                        call_id: call_id.clone(),
                        tool_id: "document_operation".to_string(),
                        ok,
                        result_summary: result_summary.clone(),
                    },
                    text: result_summary,
                    elapsed_ms: elapsed_after,
                },
            })
            .await;

        let result = exec_result?;
        let result_text = match &result {
            StepOutput::Text(t) => t.clone(),
            StepOutput::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            _ => String::new(),
        };

        tracing::info!(
            output_chars = result_text.len(),
            "runtime: document_operation — complete"
        );

        // 4. Build response.
        let provenance = ResponseProvenance {
            intent: "DocumentOperation".to_string(),
            search_method: Some("document_operation".to_string()),
            sources: vec![SourceSummary {
                origin: "user_document".to_string(),
                count: chunk_count,
                from_peer: None,
                display_name: None,
            }],
            inference_backend: prompt_response.model_id.clone(),
            oicp_match: None,
            total_latency_ms: 0,
            tokens_used: 0,
            coarse_intent: None,
            self_assessment: None,
            routing_trigger: None,
            coverage: None,
            finish_reason: prompt_response.finish_reason.clone(),
            max_tokens_budget: Some(self.inference_config.max_tokens),
            completion_tokens: prompt_response.completion_tokens,
            context_window: self.inference.effective_context_size(),
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: result_text,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "provenance": provenance,
                "document_source": resolved_source,
                "document_chunks": chunk_count,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: None,
            metrics: None,
        })
    }
}
