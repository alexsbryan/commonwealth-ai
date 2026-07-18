// SPDX-License-Identifier: AGPL-3.0-or-later
//! ComplexTask dispatch — planner-driven multi-step execution.
//! Builds a Task, hands it to the Planner, then drives the Executor
//! through each step until completion or yield.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Result;

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
                    // Prefer a tool's human-formatted fields over raw JSON:
                    // `answer` (search tools), then `summary` (the COMPACT
                    // cited figures from a deterministic figure tool like
                    // parcel_analytics). Showing the raw JSON would put
                    // precise multi-digit values (e.g. 1477806471.0) in
                    // front of the model — which it cannot retype faithfully
                    // and corrupts into digit-salad. So the model narrates
                    // from compact figures it can copy; the exact derivation
                    // is appended verbatim from the tool downstream.
                    let text = v
                        .get("answer")
                        .and_then(|a| a.as_str())
                        .or_else(|| v.get("summary").and_then(|s| s.as_str()))
                        .unwrap_or("");
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
             answer.\n\n\
             Provenance rule: every dollar amount and percentage in your answer \
             must be a figure that already appears in the step results above \
             (for example a `parcel_analytics` cited figure). Quote the COMPACT \
             form exactly as given — `$1.48B`, `94.74%`, `874 parcels`. Do NOT \
             compute, round, expand, or re-type numbers; in particular never \
             write out a long exact value like `$1,477,806,471.00`. The exact, \
             to-the-cent derivation is appended to your answer automatically \
             from the tool, so narrate only with the compact figures and refer \
             the reader to the derivation below for the precise numbers. The \
             tools are the deterministic calculator; you relay their numbers, \
             you never originate one.\n\n{budget_note}"
        );
        let synthesis_system = self.build_primary_system_message(&synthesis_base, context);

        // Shared synthesis-request core (`synthesis_common`) + the
        // surface's own system message.
        let synthesis_request = CompletionRequest {
            system_message: Some(synthesis_system),
            ..self.synthesis_request(synthesis_prompt, self.build_oicp(&Intent::ComplexTask))
        };
        let synthesis = self.inference.complete(&synthesis_request).await?;

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
        // Cited dollar/percentage figures emitted by deterministic tools
        // (e.g. `parcel_analytics`) — the audit set for Layer 3 of the
        // "no confabulated numbers" guarantee.
        let mut cited_figures: Vec<String> = Vec::new();
        // The raw numeric outputs of the figure-emitting tool(s) — the
        // exact (un-rounded) side of the audit's allowed set, so a precise
        // quote of a computed value traces even when the cited string is
        // the compact form. See `runtime::numeric_audit`.
        let mut raw_values: Vec<f64> = Vec::new();
        // The tool's verbatim derivation trace + reproduce hint. Rendered
        // verbatim into the final answer (NOT retyped by the model, which
        // corrupts long precise numbers) so the reader sees the exact
        // formula, inputs, and result — the glassbox half of the guarantee.
        let mut derivation_lines: Vec<String> = Vec::new();
        let mut reproduce_hints: Vec<String> = Vec::new();
        for (_step_idx, output) in &task.completed_steps {
            match output {
                StepOutput::Json(ref val) => {
                    if let Some(figs) = val.get("cited_figures").and_then(|v| v.as_array()) {
                        cited_figures
                            .extend(figs.iter().filter_map(|f| f.as_str().map(String::from)));
                        crate::runtime::numeric_audit::json_numeric_leaves(val, &mut raw_values);
                        if let Some(d) = val.get("derivation").and_then(|v| v.as_array()) {
                            derivation_lines
                                .extend(d.iter().filter_map(|x| x.as_str().map(String::from)));
                        }
                        if let Some(r) = val.get("reproduce").and_then(|v| v.as_str()) {
                            reproduce_hints.push(r.to_string());
                        }
                    }
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

        // Layer 3 — deterministic numeric-provenance audit. When a tool
        // emitted cited figures, every $/% figure in the synthesized
        // answer must trace to one; otherwise flag it (glassbox, via
        // `self_assessment` + a warning) rather than letting an unsourced
        // number pass silently. See `runtime::numeric_audit`.
        // Production grounding gate (GateSurface::ComplexTask,
        // env-gated default-off): the model's NARRATION is verified
        // per-claim against the step transcript — the snapshot IS the
        // sealed universe (searcher: None; tool outputs have no wider
        // corpus to search). longform_chars=0 in the profile: synthesis
        // claims assemble across step outputs, so every draft takes the
        // per-claim joint-judge ladder, never single-claim max-support.
        // The verbatim derivation appendix below is system-rendered and
        // is appended AFTER gating — the gate never touches it. The
        // numeric audit stays the deterministic complement and runs on
        // the RELEASED text.
        let gate_surface = crate::runtime::grounding::GateSurface::ComplexTask;
        let mut grounding_gate_meta: Option<serde_json::Value> = None;
        let gated_text: String = if gate_surface.enabled() && !step_summaries.is_empty() {
            // Step summaries are synthesized prose, not retrieved
            // chunks — transcript-shaped evidence (body-only citation
            // check), and the snapshot IS the universe (no searcher).
            let gate_evidence = super::synthesis_common::transcript_gate_evidence(
                step_summaries.clone(),
                None,
                false,
            );
            let outcome = crate::runtime::grounding::gate_answer(
                &self.inference,
                message,
                synthesis.text.clone(),
                &gate_evidence,
                &synthesis_request,
                &gate_surface.profile(),
            )
            .await;
            grounding_gate_meta = Some(outcome.meta);
            outcome.text
        } else {
            synthesis.text.clone()
        };

        let numeric_audit_note: Option<String> = if cited_figures.is_empty() {
            None
        } else {
            let violations = crate::runtime::numeric_audit::uncited_numerics(
                &gated_text,
                &cited_figures,
                &raw_values,
            );
            if violations.is_empty() {
                tracing::info!(
                    "numeric_audit: every answer figure traces to a tool computation or cited datum"
                );
                None
            } else {
                tracing::warn!(
                    violations = ?violations,
                    "numeric_audit: answer has figure(s) not traceable to a tool computation or cited datum"
                );
                Some(format!(
                    "Provenance audit flag — {} figure(s) not traceable to a tool computation or cited datum: {}",
                    violations.len(),
                    violations.join(", ")
                ))
            }
        };

        // Save and return assistant message. Completion-telemetry
        // tail comes from the shared helper (`synthesis_common`);
        // only surface-varying fields here.
        let provenance = ResponseProvenance {
            search_method,
            sources: all_sources,
            self_assessment: numeric_audit_note,
            ..self.synthesis_provenance("ComplexTask", &synthesis)
        };

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // Evidence is the same `step_summaries` the synthesis prompt saw
        // — keeps the gap check grounded in exactly what the model had.
        let evidence = step_summaries.join("\n\n");
        let mut final_content = self
            .maybe_collaborate(conversation_id, message, &gated_text, &evidence)
            .await;
        // Append the tool's exact derivation VERBATIM. The model narrated
        // with compact figures it can copy faithfully; this block — rendered
        // by the system, never retyped by the model — is where the reader
        // sees the precise, to-the-cent computation that cannot be corrupted.
        if !derivation_lines.is_empty() {
            let mut block = String::from(
                "\n\n**How this was computed** (deterministic — `parcel_analytics`):\n",
            );
            for line in &derivation_lines {
                block.push_str(&format!("- {line}\n"));
            }
            for hint in &reproduce_hints {
                block.push_str(&format!("\n{hint}\n"));
            }
            final_content.push_str(&block);
        }

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
            created_at: now(),
            metadata: Some({
                let mut m = serde_json::json!({
                    "model": synthesis.model_id,
                    "tokens": synthesis.tokens_used,
                    "latency_ms": synthesis.latency_ms,
                    "task_id": task.id,
                    "steps_completed": task.completed_steps.len(),
                    "provenance": provenance,
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
            task: Some(task),
            metrics: None,
        })
    }
}
