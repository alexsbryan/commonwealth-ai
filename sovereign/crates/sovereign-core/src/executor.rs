use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::oicp::LatencyPreference;
use crate::registry::ToolRegistry;
use crate::skills::SkillRegistry;
use crate::traits::{ApprovalChannel, InferenceProvider, StateStore};
use crate::types::*;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ─── Tool Call Parsing ────────────────────────────────────────

struct ParsedToolCall {
    tool_id: String,
    query: String,
}

/// Parse a `<tool_call>{"tool":"...","query":"..."}</tool_call>` from model output.
fn parse_tool_call(text: &str) -> Option<ParsedToolCall> {
    let start = text.find("<tool_call>")?;
    let end = text.find("</tool_call>")?;
    if end <= start {
        return None;
    }
    let json_str = &text[start + "<tool_call>".len()..end];
    let value: serde_json::Value = serde_json::from_str(json_str.trim()).ok()?;
    let tool_id = value.get("tool")?.as_str()?.to_string();
    let query = value.get("query")?.as_str()?.to_string();
    Some(ParsedToolCall { tool_id, query })
}

// ─── Public Types ──────────────────────────────────────────────

pub struct Executor {
    pub inference: Arc<dyn InferenceProvider>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub approval: Arc<dyn ApprovalChannel>,
    pub skills: Arc<SkillRegistry>,
}

pub struct TaskContext {
    pub task: Task,
    pub completed: HashMap<usize, StepOutput>,
}

pub struct ExecutionResult {
    pub completed: HashMap<usize, StepOutput>,
    pub error: Option<StepError>,
}

// ─── AutoApprovalChannel ───────────────────────────────────────

pub struct AutoApprovalChannel;

#[async_trait]
impl ApprovalChannel for AutoApprovalChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }

    async fn ask_user(&self, _question: &str) -> Result<String> {
        Err(Error::NotImplemented(
            "Interactive input not available".to_string(),
        ))
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        let status = match output {
            StepOutput::Text(_) | StepOutput::Json(_) | StepOutput::ReasonWithToolsResult { .. } => "done",
            StepOutput::Jump(t) => {
                eprintln!("  [step {}] {} → jump to {t}", step.id, step.description);
                return;
            }
            StepOutput::Skipped => "skipped",
        };
        eprintln!("  [step {}] {} [{status}]", step.id, step.description);
    }
}

// ─── Input Resolution ──────────────────────────────────────────

pub fn resolve_inputs(
    template: &str,
    inputs: &[StepInput],
    completed: &HashMap<usize, StepOutput>,
) -> Result<String> {
    let mut result = template.to_string();

    for input in inputs {
        let output = completed.get(&input.step_id).ok_or_else(|| {
            Error::Execution(format!(
                "Step {} references incomplete step {}",
                input.step_id, input.step_id
            ))
        })?;

        let value = match output {
            StepOutput::Text(s) => s.clone(),
            StepOutput::Json(v) => {
                if input.key == "output" {
                    serde_json::to_string_pretty(v).unwrap_or_default()
                } else {
                    v.get(&input.key)
                        .map(|v| v.to_string())
                        .unwrap_or_default()
                }
            }
            StepOutput::ReasonWithToolsResult { ref text, .. } => text.clone(),
            StepOutput::Jump(_) | StepOutput::Skipped => String::new(),
        };

        let placeholder_output = format!("{{{}.output}}", input.step_id);
        let placeholder_key = format!("{{{}.{}}}", input.step_id, input.key);
        result = result.replace(&placeholder_output, &value);
        result = result.replace(&placeholder_key, &value);
    }

    Ok(result)
}

// ─── Skip Propagation ──────────────────────────────────────────

fn propagate_skips(plan: &Plan, completed: &mut HashMap<usize, StepOutput>) {
    let jumps: Vec<(usize, usize)> = completed
        .iter()
        .filter_map(|(&step_id, output)| {
            if let StepOutput::Jump(target) = output {
                Some((step_id, *target))
            } else {
                None
            }
        })
        .collect();

    for (branch_id, taken_target) in jumps {
        let step = match plan.steps.iter().find(|s| s.id == branch_id) {
            Some(s) => s,
            None => continue,
        };

        let skipped_target = match &step.kind {
            StepKind::Branch {
                if_true, if_false, ..
            } => {
                if taken_target == *if_true {
                    *if_false
                } else {
                    *if_true
                }
            }
            _ => continue,
        };

        if !completed.contains_key(&skipped_target) {
            completed.insert(skipped_target, StepOutput::Skipped);
        }

        let mut queue = vec![skipped_target];
        while let Some(current) = queue.pop() {
            for &(from, to) in &plan.edges {
                if from == current && !completed.contains_key(&to) {
                    let all_preds_skipped = plan
                        .edges
                        .iter()
                        .filter(|&&(_, t)| t == to)
                        .all(|&(f, _)| matches!(completed.get(&f), Some(StepOutput::Skipped)));

                    if all_preds_skipped {
                        completed.insert(to, StepOutput::Skipped);
                        queue.push(to);
                    }
                }
            }
        }
    }
}

// ─── Executor ──────────────────────────────────────────────────

impl Executor {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
        approval: Arc<dyn ApprovalChannel>,
        skills: Arc<SkillRegistry>,
    ) -> Self {
        Self {
            inference,
            tools,
            store,
            approval,
            skills,
        }
    }

    pub async fn run(&self, plan: &Plan, ctx: &mut TaskContext) -> Result<ExecutionResult> {
        let batches: Vec<Vec<usize>> = plan
            .topological_batches()
            .iter()
            .map(|batch| batch.iter().map(|step| step.id).collect())
            .collect();

        for batch in &batches {
            let pending: Vec<usize> = batch
                .iter()
                .copied()
                .filter(|id| !ctx.completed.contains_key(id))
                .collect();

            if pending.is_empty() {
                continue;
            }

            let futures: Vec<_> = pending
                .iter()
                .map(|&step_id| {
                    let step = &plan.steps[step_id];
                    self.execute_step(step, &ctx.completed, &ctx.task)
                })
                .collect();

            let results = futures::future::join_all(futures).await;

            for (step_id, result) in pending.iter().zip(results) {
                let step = &plan.steps[*step_id];
                match result {
                    Ok(output) => {
                        self.approval.emit_progress(step, &output);
                        ctx.completed.insert(*step_id, output);
                    }
                    Err(e) => {
                        let step_error = StepError {
                            step_id: *step_id,
                            message: e.to_string(),
                        };
                        ctx.task.completed_steps =
                            ctx.completed.iter().map(|(&k, v)| (k, v.clone())).collect();
                        ctx.task.status = TaskStatus::Failed;
                        ctx.task.updated_at = now();
                        let _ = self.store.save_task(&ctx.task).await;

                        return Ok(ExecutionResult {
                            completed: ctx.completed.clone(),
                            error: Some(step_error),
                        });
                    }
                }
            }

            propagate_skips(plan, &mut ctx.completed);

            for &step_id in batch {
                if let Some(StepOutput::Skipped) = ctx.completed.get(&step_id) {
                    self.approval
                        .emit_progress(&plan.steps[step_id], &StepOutput::Skipped);
                }
            }

            ctx.task.completed_steps =
                ctx.completed.iter().map(|(&k, v)| (k, v.clone())).collect();
            ctx.task.updated_at = now();
            let _ = self.store.save_task(&ctx.task).await;
        }

        Ok(ExecutionResult {
            completed: ctx.completed.clone(),
            error: None,
        })
    }

    async fn execute_step(
        &self,
        step: &Step,
        completed: &HashMap<usize, StepOutput>,
        task: &Task,
    ) -> Result<StepOutput> {
        match &step.kind {
            StepKind::Reason {
                prompt_template,
                speed,
            } => {
                let resolved = resolve_inputs(prompt_template, &step.inputs, completed)?;

                let base_system = "You are a helpful assistant performing a step in a multi-step task. Be thorough and specific.";
                let system_message = if let Some(overrides) =
                    self.skills.prompt_overrides(&Intent::ComplexTask)
                {
                    format!("{overrides}\n\n{base_system}")
                } else {
                    base_system.to_string()
                };

                // Attach OICP requirements from active skills.
                let mut oicp_req = self.skills.inference_requirements();
                oicp_req.latency = match speed {
                    Speed::Fast => LatencyPreference::Interactive,
                    Speed::Medium => LatencyPreference::BestEffort,
                    Speed::Slow => LatencyPreference::Throughput,
                };
                let oicp = if oicp_req.required.is_empty() && oicp_req.preferred.is_empty() {
                    None
                } else {
                    Some(oicp_req)
                };

                // Adaptive compute: estimate difficulty and adjust budget.
                let difficulty = self.estimate_difficulty(step, &resolved).await;
                let budget = self.compute_budget(difficulty, step);

                let effective_speed = budget.speed_override.unwrap_or(*speed);
                let request = CompletionRequest {
                    prompt: resolved.clone(),
                    system_message: Some(system_message),
                    preferred_speed: effective_speed,
                    max_tokens: Some(budget.max_tokens),
                    temperature: Some(0.7),
                    structured_output: None,
                    oicp,
                };

                // Best-of-N sampling or single completion.
                let mut output = match &budget.sampling {
                    Some(config) if config.n > 1 => {
                        self.sample_and_select(&request, config).await?
                    }
                    _ => {
                        let response = self.inference.complete(&request).await?;
                        StepOutput::Text(response.text)
                    }
                };

                // Evaluation-as-architecture: closed-loop self-correction.
                if let Some(eval_config) = &budget.evaluation {
                    output = self
                        .evaluate_and_retry(output, &resolved, &request, eval_config)
                        .await?;
                }

                Ok(output)
            }

            StepKind::Branch {
                condition,
                if_true,
                if_false,
            } => {
                let resolved = resolve_inputs(condition, &step.inputs, completed)?;
                let context_str: String = completed
                    .iter()
                    .filter_map(|(id, out)| match out {
                        StepOutput::Text(t) => Some(format!("Step {id}: {t}")),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let request = CompletionRequest::yes_no(&resolved, &context_str);
                let response = self.inference.complete(&request).await?;
                let target = if response.as_bool() {
                    *if_true
                } else {
                    *if_false
                };
                Ok(StepOutput::Jump(target))
            }

            StepKind::Tool { tool_id, params } => {
                // 1. Resolve params.
                let params_str = serde_json::to_string(params)
                    .map_err(|e| Error::Execution(e.to_string()))?;
                let resolved_str = resolve_inputs(&params_str, &step.inputs, completed)?;
                let resolved_params: serde_json::Value = serde_json::from_str(&resolved_str)
                    .map_err(|e| Error::Execution(format!("Invalid resolved params: {e}")))?;

                // 2. Get tool.
                let tool = self.tools.get(tool_id)?;

                // 3. Check permissions.
                for permission in tool.required_permissions() {
                    let scope = format!("{permission:?}");
                    let granted = self.store.get_permission(tool_id, &scope).await?;

                    match granted {
                        Some(true) => {}
                        Some(false) => return Ok(StepOutput::Skipped),
                        None => {
                            let preview = ActionPreview {
                                tool_id: tool_id.clone(),
                                description: format!(
                                    "{}: {}",
                                    tool.descriptor().name,
                                    tool.descriptor().description
                                ),
                                params: resolved_params.clone(),
                            };

                            let approved =
                                self.approval.request_approval(step, &preview).await?;

                            if !approved {
                                return Ok(StepOutput::Skipped);
                            }
                        }
                    }
                }

                // 4. Validate.
                tool.validate(&resolved_params)?;

                // 5. Execute with retry on transient failures.
                let tool_ctx = ToolContext {
                    conversation_id: task.conversation_id.clone(),
                    task_id: Some(task.id.clone()),
                    working_directory: None,
                    in_reasoning_loop: false,
                };

                let retry = tool.retry_config().unwrap_or_default();
                let mut last_error = None;

                for attempt in 0..=retry.max_retries {
                    if attempt > 0 {
                        let delay = retry
                            .backoff_ms
                            .get(attempt - 1)
                            .copied()
                            .unwrap_or(3000);
                        eprintln!(
                            "[executor] Tool '{}' retry {}/{} after {}ms",
                            tool_id, attempt, retry.max_retries, delay
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }

                    match tool.execute(&resolved_params, &tool_ctx).await {
                        Ok(output) => return Ok(output),
                        Err(e) => {
                            let msg = e.to_string().to_lowercase();
                            let retryable = msg.contains("timeout")
                                || msg.contains("rate limit")
                                || msg.contains("connection")
                                || msg.contains("temporarily");
                            if retryable && attempt < retry.max_retries {
                                last_error = Some(e);
                                continue;
                            }
                            return Err(e);
                        }
                    }
                }

                Err(last_error.unwrap_or_else(|| {
                    Error::Execution("All retries exhausted".to_string())
                }))
            }

            StepKind::UserInput { question } => {
                let resolved = resolve_inputs(question, &step.inputs, completed)?;
                let answer = self.approval.ask_user(&resolved).await?;
                Ok(StepOutput::Text(answer))
            }

            StepKind::ReasonWithTools {
                prompt_template,
                speed,
                available_tools,
                max_iterations,
            } => {
                let resolved = resolve_inputs(prompt_template, &step.inputs, completed)?;

                // Adaptive compute: adjust max iterations based on difficulty.
                let difficulty = self.estimate_difficulty(step, &resolved).await;
                let effective_max_iter = match difficulty {
                    StepDifficulty::Routine => (*max_iterations).min(2),
                    StepDifficulty::Moderate => *max_iterations,
                    StepDifficulty::Hard => *max_iterations + 2,
                };

                let output = self
                    .execute_reason_with_tools(
                        &resolved,
                        *speed,
                        available_tools,
                        effective_max_iter,
                        task,
                    )
                    .await?;

                // Apply evaluation if configured.
                if let Some(eval_config) = &step.evaluation {
                    if let StepOutput::ReasonWithToolsResult { ref text, .. } = output {
                        let text_output = StepOutput::Text(text.clone());
                        let evaluated = self
                            .evaluate_and_retry(
                                text_output,
                                &resolved,
                                &CompletionRequest::new(&resolved).with_speed(*speed),
                                eval_config,
                            )
                            .await?;
                        if let StepOutput::Text(improved_text) = evaluated {
                            if let StepOutput::ReasonWithToolsResult {
                                search_log,
                                iterations,
                                capped,
                                ..
                            } = output
                            {
                                return Ok(StepOutput::ReasonWithToolsResult {
                                    text: improved_text,
                                    search_log,
                                    iterations,
                                    capped,
                                });
                            }
                        }
                    }
                }

                Ok(output)
            }
        }
    }

    // ─── ReasonWithTools Loop ─────────────────────────────────

    async fn execute_reason_with_tools(
        &self,
        prompt: &str,
        speed: Speed,
        available_tools: &[ToolId],
        max_iterations: usize,
        task: &Task,
    ) -> Result<StepOutput> {
        // Build tool descriptions for the system prompt.
        let tool_descs: Vec<String> = available_tools
            .iter()
            .filter_map(|id| self.tools.get(id).ok())
            .map(|t| {
                let d = t.descriptor();
                format!("- {} (id: {}): {}", d.name, d.id, d.description)
            })
            .collect();

        let system = self.build_retrieval_reasoning_prompt(&tool_descs, max_iterations);

        // Build the growing conversation as a single prompt.
        let mut conversation = format!(
            "{system}\n\n---\n\nUser question: {prompt}\n\nAssistant:"
        );

        let mut search_log: Vec<SearchLogEntry> = Vec::new();
        let mut iterations = 0;

        loop {
            let request = CompletionRequest {
                prompt: conversation.clone(),
                system_message: None, // System is baked into the prompt
                preferred_speed: speed,
                max_tokens: Some(2048),
                temperature: Some(0.3),
                structured_output: None,
                oicp: None,
            };

            let response = self.inference.complete(&request).await?;
            let response_text = response.text.trim().to_string();

            // Check if the model emitted a tool call.
            if let Some(tool_call) = parse_tool_call(&response_text) {
                // Find and execute the tool.
                let tool_result = match self.tools.get(&tool_call.tool_id) {
                    Ok(tool) => {
                        let params = serde_json::json!({"query": tool_call.query});
                        let ctx = ToolContext {
                            conversation_id: task.conversation_id.clone(),
                            task_id: Some(task.id.clone()),
                            working_directory: None,
                            in_reasoning_loop: true,
                        };
                        match tool.execute(&params, &ctx).await {
                            Ok(output) => match output {
                                StepOutput::Text(t) => t,
                                StepOutput::Json(v) => v
                                    .get("answer")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("No results.")
                                    .to_string(),
                                _ => "No results.".to_string(),
                            },
                            Err(e) => format!("Search failed: {e}. Try a different query."),
                        }
                    }
                    Err(_) => format!("Tool '{}' not available.", tool_call.tool_id),
                };

                // Count results (rough heuristic: count [Source lines).
                let result_count = tool_result.matches("[Source").count();

                search_log.push(SearchLogEntry {
                    iteration: iterations,
                    tool_id: tool_call.tool_id.clone(),
                    query: tool_call.query.clone(),
                    result_count,
                });

                // Append model's thinking + tool results to the conversation.
                let thinking = response_text
                    .split("<tool_call>")
                    .next()
                    .unwrap_or("")
                    .trim();
                conversation.push_str(&format!(
                    " {thinking}\n\n[Search results for \"{}\"]:\n{tool_result}\n\nAssistant:",
                    tool_call.query
                ));

                iterations += 1;

                // Safety cap.
                if iterations >= max_iterations {
                    conversation.push_str(
                        " You have used all available searches. Synthesize your answer now \
                         from what you've found. If you couldn't find everything, note what's \
                         missing.\n\nAssistant:",
                    );

                    let final_request = CompletionRequest {
                        prompt: conversation,
                        system_message: None,
                        preferred_speed: speed,
                        max_tokens: Some(2048),
                        temperature: Some(0.3),
                        structured_output: None,
                        oicp: None,
                    };
                    let final_response = self.inference.complete(&final_request).await?;

                    return Ok(StepOutput::ReasonWithToolsResult {
                        text: final_response.text.trim().to_string(),
                        search_log,
                        iterations,
                        capped: true,
                    });
                }

                continue;
            }

            // No tool call — model is done reasoning. Return the synthesis.
            return Ok(StepOutput::ReasonWithToolsResult {
                text: response_text,
                search_log,
                iterations,
                capped: false,
            });
        }
    }

    fn build_retrieval_reasoning_prompt(
        &self,
        tool_descriptions: &[String],
        max_iterations: usize,
    ) -> String {
        let tools_list = tool_descriptions.join("\n");

        // Check if active skills have a retrieval reasoning addendum.
        let skill_addendum = self
            .skills
            .prompt_overrides(&Intent::ComplexTask)
            .unwrap_or_default();

        format!(
            r#"You are a research assistant with access to knowledge bases. Answer the user's question by searching for relevant information and reasoning about what you find.

## How to search

When you need information, emit a tool call in this exact format:
<tool_call>{{"tool":"search","query":"your search terms"}}</tool_call>

After each search, you'll receive results. Read them carefully, then either:
- Search again with different terms if you need more information
- Write your final answer if you have enough

## When to search

- Search when you need factual information you're not certain about.
- Search when you need specific details, quotes, dates, or names.
- If results are incomplete, try different search terms — different angle, more specific, or broader.
- Don't search for things you already found.

## When to stop

- Stop when you have enough sources to answer confidently.
- Stop when additional searches return information you've already seen.
- You have a maximum of {max_iterations} searches. Use them wisely.

## Your answer

When ready to answer (without a <tool_call>):
- Cite sources using [Source: name] notation.
- If sources conflict, present both positions.
- If you couldn't find part of the answer, say so explicitly.

## Available tools

{tools_list}

{skill_addendum}"#
        )
    }

    // ─── Best-of-N Sampling ──────────────────────────────────

    async fn sample_and_select(
        &self,
        request: &CompletionRequest,
        config: &SamplingConfig,
    ) -> Result<StepOutput> {
        // Generate N candidates.
        let futures: Vec<_> = (0..config.n)
            .map(|_| self.inference.complete(request))
            .collect();
        let results = futures::future::join_all(futures).await;
        let candidates: Vec<String> = results
            .into_iter()
            .filter_map(|r| r.ok())
            .map(|r| r.text)
            .collect();

        if candidates.is_empty() {
            return Err(Error::Execution(
                "All sampling candidates failed".to_string(),
            ));
        }
        if candidates.len() == 1 {
            return Ok(StepOutput::Text(candidates.into_iter().next().unwrap()));
        }

        let best = self
            .select_best(&candidates, &config.selector, &request.prompt)
            .await?;
        Ok(StepOutput::Text(best))
    }

    async fn select_best(
        &self,
        candidates: &[String],
        selector: &SampleSelector,
        original_prompt: &str,
    ) -> Result<String> {
        match selector {
            SampleSelector::LlmJudge { selection_prompt } => {
                let numbered = candidates
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("--- Response {} ---\n{}", i + 1, c))
                    .collect::<Vec<_>>()
                    .join("\n\n");

                let judge_prompt = format!(
                    "{}\n\nOriginal task:\n{}\n\nCandidate responses:\n{}\n\n\
                     Select the best response. Return only the number (1-{}).",
                    selection_prompt.as_deref().unwrap_or(
                        "You are evaluating multiple responses. Select the most \
                         accurate, complete, well-reasoned, and appropriately cited."
                    ),
                    &original_prompt[..original_prompt.len().min(500)],
                    numbered,
                    candidates.len()
                );

                let request =
                    CompletionRequest::new(&judge_prompt).with_speed(Speed::Fast);
                let response = self.inference.complete(&request).await?;
                let choice: usize = response.text.trim().parse().unwrap_or(1);
                let idx = choice.saturating_sub(1).min(candidates.len() - 1);
                Ok(candidates[idx].clone())
            }

            SampleSelector::MajorityVote => {
                let mut votes: HashMap<String, usize> = HashMap::new();
                for c in candidates {
                    let key = c.lines().next().unwrap_or("").to_string();
                    *votes.entry(key).or_insert(0) += 1;
                }
                let winner = votes
                    .into_iter()
                    .max_by_key(|(_, count)| *count)
                    .map(|(key, _)| key)
                    .unwrap_or_default();
                Ok(candidates
                    .iter()
                    .find(|c| c.starts_with(&winner))
                    .cloned()
                    .unwrap_or_else(|| candidates[0].clone()))
            }

            SampleSelector::Verify { tool_id } => {
                let tool = self.tools.get(tool_id)?;
                let ctx = ToolContext {
                    conversation_id: String::new(),
                    task_id: None,
                    working_directory: None,
                    in_reasoning_loop: false,
                };
                for candidate in candidates {
                    let params = serde_json::json!({"input": candidate});
                    if tool.execute(&params, &ctx).await.is_ok() {
                        return Ok(candidate.clone());
                    }
                }
                Ok(candidates[0].clone())
            }
        }
    }

    // ─── Evaluation & Self-Correction ────────────────────────

    async fn evaluate_and_retry(
        &self,
        mut output: StepOutput,
        original_prompt: &str,
        request: &CompletionRequest,
        eval_config: &EvaluationConfig,
    ) -> Result<StepOutput> {
        for retry in 0..=eval_config.max_retries {
            let text = match &output {
                StepOutput::Text(t) => t.as_str(),
                _ => return Ok(output),
            };

            let eval_prompt = format!(
                "{}\n\nOutput to evaluate:\n{}\n\n\
                 Respond with JSON: {{\"pass\": true}} or {{\"pass\": false, \"feedback\": \"what's wrong\"}}",
                eval_config.eval_prompt,
                &text[..text.len().min(2000)]
            );

            let eval_request = CompletionRequest::new(&eval_prompt)
                .with_speed(eval_config.eval_speed);
            let eval_response = self.inference.complete(&eval_request).await?;

            // Parse evaluation result.
            let passed = eval_response.text.contains("\"pass\": true")
                || eval_response.text.contains("\"pass\":true");

            if passed {
                return Ok(output);
            }

            if retry >= eval_config.max_retries {
                eprintln!("[executor] Evaluation failed after {} retries, accepting output", retry);
                return Ok(output);
            }

            // Extract feedback and retry.
            let feedback = eval_response
                .text
                .split("\"feedback\":")
                .nth(1)
                .and_then(|s| s.split('"').nth(1))
                .unwrap_or("The previous attempt had quality issues. Please improve.");

            let retry_prompt = format!(
                "{}\n\n[Previous attempt had an issue: {}. Please correct.]",
                original_prompt, feedback
            );
            let mut retry_request = request.clone();
            retry_request.prompt = retry_prompt;
            let response = self.inference.complete(&retry_request).await?;
            output = StepOutput::Text(response.text);
        }

        Ok(output)
    }

    // ─── Adaptive Test-Time Compute ──────────────────────────

    async fn estimate_difficulty(&self, step: &Step, _resolved_prompt: &str) -> StepDifficulty {
        // Only estimate difficulty if skills are active (production use).
        // Without active skills, default to Moderate to avoid unnecessary inference calls.
        if self.skills.active_skills().is_empty() {
            return StepDifficulty::Moderate;
        }

        // Use the Fast model for a quick classification.
        let prompt = format!(
            "Rate the difficulty of this task as 'routine', 'moderate', or 'hard'. \
             Respond with one word.\n\nTask: {}",
            step.description
        );

        let request = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
        match self.inference.complete(&request).await {
            Ok(response) => match response.text.trim().to_lowercase().as_str() {
                "routine" => StepDifficulty::Routine,
                "hard" => StepDifficulty::Hard,
                _ => StepDifficulty::Moderate,
            },
            Err(_) => StepDifficulty::Moderate,
        }
    }

    fn compute_budget(&self, difficulty: StepDifficulty, step: &Step) -> ComputeBudget {
        match difficulty {
            StepDifficulty::Routine => ComputeBudget {
                max_tokens: 1024,
                sampling: None,
                evaluation: None,
                speed_override: Some(Speed::Fast),
            },
            StepDifficulty::Moderate => ComputeBudget {
                max_tokens: 4096,
                sampling: step.sampling.clone(),
                evaluation: step.evaluation.clone(),
                speed_override: None,
            },
            StepDifficulty::Hard => ComputeBudget {
                max_tokens: 4096,
                sampling: step.sampling.clone().or_else(|| {
                    Some(SamplingConfig {
                        n: 3,
                        selector: SampleSelector::LlmJudge {
                            selection_prompt: None,
                        },
                    })
                }),
                evaluation: step.evaluation.clone().or_else(|| {
                    Some(EvaluationConfig {
                        eval_prompt: "Check this output for logical consistency, \
                                      factual grounding, and completeness."
                            .to_string(),
                        max_retries: 1,
                        eval_speed: Speed::Fast,
                    })
                }),
                speed_override: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_inputs_simple() {
        let mut completed = HashMap::new();
        completed.insert(0, StepOutput::Text("hello world".to_string()));

        let inputs = vec![StepInput {
            step_id: 0,
            key: "output".to_string(),
        }];
        let result = resolve_inputs("Previous said: {0.output}", &inputs, &completed).unwrap();
        assert_eq!(result, "Previous said: hello world");
    }

    #[test]
    fn resolve_inputs_multiple() {
        let mut completed = HashMap::new();
        completed.insert(0, StepOutput::Text("Python is great".to_string()));
        completed.insert(1, StepOutput::Text("Rust is fast".to_string()));

        let inputs = vec![
            StepInput {
                step_id: 0,
                key: "output".to_string(),
            },
            StepInput {
                step_id: 1,
                key: "output".to_string(),
            },
        ];
        let result = resolve_inputs(
            "Compare: {0.output} vs {1.output}",
            &inputs,
            &completed,
        )
        .unwrap();
        assert_eq!(result, "Compare: Python is great vs Rust is fast");
    }

    #[test]
    fn resolve_inputs_missing_step() {
        let completed = HashMap::new();
        let inputs = vec![StepInput {
            step_id: 5,
            key: "output".to_string(),
        }];
        assert!(resolve_inputs("test {5.output}", &inputs, &completed).is_err());
    }

    #[test]
    fn resolve_inputs_json_key() {
        let mut completed = HashMap::new();
        completed.insert(
            0,
            StepOutput::Json(serde_json::json!({"name": "Alice", "age": 30})),
        );

        let inputs = vec![StepInput {
            step_id: 0,
            key: "name".to_string(),
        }];
        let result = resolve_inputs("Hello {0.name}", &inputs, &completed).unwrap();
        assert_eq!(result, "Hello \"Alice\"");
    }

    #[test]
    fn resolve_inputs_no_inputs() {
        let completed = HashMap::new();
        let result = resolve_inputs("no placeholders here", &[], &completed).unwrap();
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn propagate_skips_branch() {
        let plan = Plan {
            id: "t1".to_string(),
            goal: "test".to_string(),
            steps: vec![
                Step {
                    id: 0,
                    description: "branch".to_string(),
                    kind: StepKind::Branch {
                        condition: "test".to_string(),
                        if_true: 1,
                        if_false: 2,
                    },
                    requires_approval: false,
                    inputs: vec![],
                    sampling: None,
                    evaluation: None,
                },
                Step {
                    id: 1,
                    description: "true path".to_string(),
                    kind: StepKind::Reason {
                        prompt_template: "x".to_string(),
                        speed: Speed::Fast,
                    },
                    requires_approval: false,
                    inputs: vec![],
                    sampling: None,
                    evaluation: None,
                },
                Step {
                    id: 2,
                    description: "false path".to_string(),
                    kind: StepKind::Reason {
                        prompt_template: "y".to_string(),
                        speed: Speed::Fast,
                    },
                    requires_approval: false,
                    inputs: vec![],
                    sampling: None,
                    evaluation: None,
                },
            ],
            edges: vec![(0, 1), (0, 2)],
        };

        let mut completed = HashMap::new();
        completed.insert(0, StepOutput::Jump(1));

        propagate_skips(&plan, &mut completed);

        assert!(matches!(completed.get(&2), Some(StepOutput::Skipped)));
        assert!(!completed.contains_key(&1));
    }
}
