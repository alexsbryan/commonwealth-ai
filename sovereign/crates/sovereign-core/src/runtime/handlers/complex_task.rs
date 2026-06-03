//! ComplexTask dispatch — planner-driven multi-step execution.
//! Builds a Task, hands it to the Planner, then drives the Executor
//! through each step until completion or yield.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Result;
use crate::traits::*;

use super::super::*;

impl Runtime {
    pub(crate) async fn handle_complex_task(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        // Document-attached messages are handled by handle_document_operation
        // before reaching this point. This path is for non-document ComplexTasks.

        tracing::info!("runtime: complex_task — generating plan");
        let plan = self
            .planner
            .plan(message, context, tool_descriptors)
            .await?;

        tracing::info!(
            steps = plan.steps.len(),
            "runtime: complex_task — plan generated"
        );
        for step in &plan.steps {
            tracing::debug!(
                step_id = step.id,
                description = %step.description,
                kind = ?std::mem::discriminant(&step.kind),
                "runtime: complex_task — step"
            );
        }

        // 2. Create task.
        let mut task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            goal: message.to_string(),
            plan: plan.clone(),
            status: TaskStatus::Running,
            completed_steps: Vec::new(),
            created_at: now(),
            updated_at: now(),
            version: now(),
        };
        self.store.save_task(&task).await?;

        // 3. Execute.
        let executor = Executor::new(
            Arc::clone(&self.inference),
            Arc::clone(&self.tools),
            Arc::clone(&self.store),
            Arc::clone(&self.approval),
            Arc::clone(&self.skills),
        );

        let mut ctx = TaskContext {
            task: task.clone(),
            completed: HashMap::new(),
        };

        let mut result = executor.run(&plan, &mut ctx).await?;

        // 4. Replan on failure (one retry).
        if let Some(ref error) = result.error {
            tracing::warn!(
                step_id = error.step_id,
                error = %error.message,
                "runtime: complex_task — step failed, attempting replan"
            );

            let completed_vec: Vec<(usize, StepOutput)> = result
                .completed
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect();

            match self.planner.replan(&plan, &completed_vec, error).await {
                Ok(new_plan) => {
                    tracing::info!(
                        steps = new_plan.steps.len(),
                        "runtime: complex_task — replan generated"
                    );
                    task.plan = new_plan.clone();
                    task.status = TaskStatus::Running;
                    task.updated_at = now();

                    let mut retry_ctx = TaskContext {
                        task: task.clone(),
                        completed: HashMap::new(),
                    };

                    result = executor.run(&new_plan, &mut retry_ctx).await?;

                    if result.error.is_some() {
                        tracing::warn!("runtime: complex_task — replan also failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "runtime: complex_task — replan failed");
                }
            }
        }

        // 5. Synthesize final answer from step outputs.
        let step_summaries: Vec<String> = result
            .completed
            .iter()
            .filter_map(|(id, output)| match output {
                StepOutput::Text(t) => Some(format!("Step {id}: {t}")),
                StepOutput::Json(v) => {
                    // For search tool output, use the "answer" field.
                    let text = v.get("answer").and_then(|a| a.as_str()).unwrap_or({
                        // Fallback: serialize the whole JSON.
                        ""
                    });
                    if text.is_empty() {
                        Some(format!(
                            "Step {id}: {}",
                            serde_json::to_string_pretty(v).unwrap_or_default()
                        ))
                    } else {
                        Some(format!("Step {id}: {text}"))
                    }
                }
                StepOutput::ReasonWithToolsResult {
                    ref text,
                    iterations,
                    capped,
                    ..
                } => {
                    let note = if *capped { " (search cap reached)" } else { "" };
                    Some(format!("Step {id} ({iterations} searches{note}): {text}"))
                }
                _ => None,
            })
            .collect();

        let synthesis_prompt = format!(
            "Goal: {message}\n\nStep results:\n{}\n\nProvide a comprehensive final answer that synthesizes all the step results above.",
            step_summaries.join("\n\n")
        );

        let budget_note =
            crate::runtime::build_response_length_directive(self.inference_config.max_tokens);
        let synthesis_base = format!(
            "Synthesize the given step results into a clear, comprehensive \
             answer.\n\n{budget_note}"
        );
        let synthesis_system = self.build_primary_system_message(&synthesis_base, context);

        let synthesis = self
            .inference
            .complete(&CompletionRequest {
                prompt: synthesis_prompt,
                system_message: Some(synthesis_system),
                preferred_speed: Speed::Slow,
                max_tokens: Some(self.inference_config.max_tokens),
                temperature: Some(self.inference_config.temperature),
                think_budget: Some(self.inference_config.think_budget),
                structured_output: None,
                top_k: self.inference_config.top_k,
                top_p: None,
                oicp: self.build_oicp(&Intent::ComplexTask),
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
            })
            .await?;

        // 6. Update task status.
        task.completed_steps = result
            .completed
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();
        task.status = if result.error.is_some() {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        task.updated_at = now();
        self.store.save_task(&task).await?;

        // 7. Extract search provenance from tool step outputs.
        let mut search_method: Option<String> = None;
        let mut all_sources: Vec<SourceSummary> = Vec::new();
        for (_step_idx, output) in &task.completed_steps {
            match output {
                StepOutput::Json(ref val) => {
                    if let Some(method) = val.get("search_method").and_then(|v| v.as_str()) {
                        search_method = Some(method.to_string());
                    }
                    if let Some(sources) = val.get("sources").and_then(|v| v.as_array()) {
                        for src in sources {
                            if let (Some(origin), Some(count)) = (
                                src.get("origin").and_then(|v| v.as_str()),
                                src.get("count").and_then(|v| v.as_u64()),
                            ) {
                                all_sources.push(SourceSummary {
                                    origin: origin.to_string(),
                                    count: count as usize,
                                    from_peer: None,
                                    display_name: None,
                                });
                            }
                        }
                    }
                }
                StepOutput::ReasonWithToolsResult {
                    search_log,
                    iterations,
                    ..
                } => {
                    search_method = Some(format!("ReasonWithTools ({iterations} iterations)"));
                    // Aggregate search log into source summaries.
                    let mut tool_counts: HashMap<String, usize> = HashMap::new();
                    for entry in search_log {
                        *tool_counts.entry(entry.tool_id.clone()).or_insert(0) +=
                            entry.result_count;
                    }
                    for (tool_id, count) in tool_counts {
                        all_sources.push(SourceSummary {
                            origin: tool_id,
                            count,
                            from_peer: None,
                            display_name: None,
                        });
                    }
                }
                _ => {}
            }
        }

        // Save and return assistant message.
        let provenance = ResponseProvenance {
            intent: "ComplexTask".to_string(),
            search_method,
            sources: all_sources,
            inference_backend: synthesis.model_id.clone(),
            oicp_match: synthesis
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: synthesis.latency_ms,
            tokens_used: synthesis.tokens_used,
            coarse_intent: None,
            self_assessment: None,
            routing_trigger: None,
            coverage: None,
            finish_reason: synthesis.finish_reason.clone(),
            max_tokens_budget: Some(self.inference_config.max_tokens),
            completion_tokens: synthesis.completion_tokens,
            context_window: self.inference.effective_context_size(),
        };

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // Evidence is the same `step_summaries` the synthesis prompt saw
        // — keeps the gap check grounded in exactly what the model had.
        let evidence = step_summaries.join("\n\n");
        let final_content = self
            .maybe_collaborate(conversation_id, message, &synthesis.text, &evidence)
            .await;

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": synthesis.model_id,
                "tokens": synthesis.tokens_used,
                "latency_ms": synthesis.latency_ms,
                "task_id": task.id,
                "steps_completed": task.completed_steps.len(),
                "provenance": provenance,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: Some(task),
            metrics: None,
        })
    }
}
