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
            StepOutput::Text(_) | StepOutput::Json(_) => "done",
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

                let request = CompletionRequest {
                    prompt: resolved,
                    system_message: Some(system_message),
                    preferred_speed: *speed,
                    max_tokens: Some(1024),
                    temperature: Some(0.7),
                    structured_output: None,
                    oicp,
                };
                let response = self.inference.complete(&request).await?;
                Ok(StepOutput::Text(response.text))
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
                        Some(true) => {} // Already approved.
                        Some(false) => return Ok(StepOutput::Skipped),
                        None => {
                            // Ask for approval.
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
                            // Permission granted — persist for this session.
                            // "Always allow" is handled by the CLI approval channel
                            // which calls set_permission directly.
                        }
                    }
                }

                // 4. Validate.
                tool.validate(&resolved_params)?;

                // 5. Execute.
                let tool_ctx = ToolContext {
                    conversation_id: task.conversation_id.clone(),
                    task_id: Some(task.id.clone()),
                    working_directory: None,
                };

                tool.execute(&resolved_params, &tool_ctx).await
            }

            StepKind::UserInput { question } => {
                let resolved = resolve_inputs(question, &step.inputs, completed)?;
                let answer = self.approval.ask_user(&resolved).await?;
                Ok(StepOutput::Text(answer))
            }
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
