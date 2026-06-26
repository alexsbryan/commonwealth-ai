// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::oicp::LatencyClass;
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

// ─── LlmJudge Rubrics ─────────────────────────────────────────

/// The pre-existing default judge rubric. Optimised for factual /
/// retrieval-grounded synthesis: pick the most accurate, complete,
/// well-reasoned, appropriately-cited answer. Active when an
/// `LlmJudge` selector has neither a custom `selection_prompt` nor
/// `preset = JudgePreset::Voice`.
pub(crate) const DEFAULT_JUDGE_PROMPT: &str =
    "You are evaluating multiple responses. Select the most \
     accurate, complete, well-reasoned, and appropriately cited.";

/// The voice-judge rubric for the glass-box relational contract.
/// Active when a caller selects `JudgePreset::Voice` (e.g. via
/// `SampleSelector::voice_judge()` in the Tier-B `voice_eval`
/// harness or a future planner annotation).
///
/// Mirrors the structure of `runtime::RELATIONAL_BASE_SYSTEM_PROMPT`:
/// load-bearing posture (witness, not performer) + eight Right-X
/// folds + named disqualifiers. The fold names are the keys the
/// `voice_eval::judge::JudgeScore` struct deserialises against —
/// renaming a fold here requires the matching rename there.
pub(crate) const VOICE_JUDGE_PROMPT: &str = "\
You are scoring a candidate response for a situated, relational \
exchange. The right voice is a witness, not a performer: it pays \
attention to what's there, says what it sees, admits what it doesn't, \
trusts the user to do their own work. Score the candidate on the \
eight folds of that posture.\n\
\n\
right_attention — does the response notice what's actually in front \
of it (what the person said, what they didn't, what's changed) \
rather than reach for the generic shape of this kind of \
conversation?\n\
\n\
right_specificity — does the response speak to the particular \
thing, not the category? \"That sounds hard\" is 0; \"hearing him \
say that after the week you'd had\" is 3.\n\
\n\
right_calibration — does the response distinguish what's known from \
history (\"you told me...\") vs. what's inferred (\"it sounds \
like...\") vs. what's a guess (\"I'm reaching, but...\")? Different \
phrasings for different evidence.\n\
\n\
right_question — are questions present only when the answer would \
change what comes next? Filler (\"Does that make sense?\", \"What do \
you think?\") is a disqualifier. One focused question beats three.\n\
\n\
right_silence — is the response the right length? Two sentences when \
two are right. Padding with closing reflection or reassurance fails.\n\
\n\
right_disagreement — when the user's framing is visibly off given \
prior context, does the candidate gently surface an alternative \
read once, as inquiry? Pure validation when disagreement is warranted \
is a disqualifier.\n\
\n\
right_edge — for medical / legal / high-stakes domains, does the \
candidate locate itself precisely (\"here's what I can do; here's \
what's outside my range\") rather than perform a disclaimer and \
proceed anyway?\n\
\n\
right_self_honesty — when asked about memory or about itself, is the \
candidate specific about what was saved and what wasn't, rather than \
a confident yes / flat no?\n\
\n\
Score each axis 0 (worst) to 3 (best). For `avoid_list_penalty`, \
count every avoid-list pattern hit (0 means none):\n\
- Therapist register (\"It sounds like you're feeling...\", \"I hear \
you saying...\")\n\
- Wisdom voice (\"perhaps the question isn't X but Y\", \"the deeper \
question is...\")\n\
- Over-affirmation (\"thoughtful question\", \"beautiful insight\", \
\"I love that you're...\")\n\
- The \"there's no right answer\" cop-out when there is one.\n\
- Generic AI disclaimers (\"As an AI...\", \"I'm just a language \
model...\")";

/// Look up the prompt body for a `JudgePreset`. Pure mapping;
/// callers may override by providing an explicit
/// `selection_prompt` on the selector.
pub(crate) fn judge_rubric_for_preset(preset: JudgePreset) -> &'static str {
    match preset {
        JudgePreset::Default => DEFAULT_JUDGE_PROMPT,
        JudgePreset::Voice => VOICE_JUDGE_PROMPT,
    }
}

/// Public Tier-B test seam — exposes the voice-judge rubric so the
/// `sovereign-cli/src/voice_eval/` harness can build judge requests
/// against the same constant the executor uses. Stability caveat:
/// not part of the production API.
#[doc(hidden)]
pub fn __voice_test_voice_judge_prompt() -> &'static str {
    VOICE_JUDGE_PROMPT
}

/// Public Tier-B test seam — exposes the default judge rubric for
/// regression assertions. Same stability caveat as
/// `__voice_test_voice_judge_prompt`.
#[doc(hidden)]
pub fn __voice_test_default_judge_prompt() -> &'static str {
    DEFAULT_JUDGE_PROMPT
}

// ─── Tool Call Parsing ────────────────────────────────────────

struct ParsedToolCall {
    tool_id: String,
    query: String,
}

/// Parse a `<tool_call>{"tool":"...","query":"..."}</tool_call>` from model output.
/// Content-derived idempotency key for a tool action: stable across a
/// replan that re-issues the same `(tool, params)` under a new step_id,
/// and across a process restart (the hash is deterministic, unlike
/// `DefaultHasher`). Scoped by task so identical actions in different
/// tasks never collide.
///
/// Public so callers can precompute the key a tool would be deduped under
/// (e.g. to forward it to a server for downstream dedup) and so tests can
/// seed a matching ledger row.
pub fn idempotency_key(task_id: &str, tool_id: &str, params: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(params).unwrap_or_default();
    format!("{task_id}:{tool_id}:{:016x}", fnv1a64(canonical.as_bytes()))
}

/// FNV-1a 64-bit — a tiny, dependency-free, deterministic hash. Stable
/// across processes and toolchain versions, so an idempotency key written
/// before a crash still matches after the restart.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Compress a step output into the ledger's `summary` field — bounded text
/// the idempotency-skip path replays as the action's prior result.
fn summarize_output(output: &StepOutput) -> Option<String> {
    let text = match output {
        StepOutput::Text(t) => t.clone(),
        StepOutput::Json(v) => v.to_string(),
        StepOutput::ReasonWithToolsResult { text, .. } => text.clone(),
        StepOutput::Jump(_) | StepOutput::Skipped => return None,
    };
    const CAP: usize = 2000;
    if text.chars().count() > CAP {
        Some(text.chars().take(CAP).collect())
    } else {
        Some(text)
    }
}

/// Augment a delegate worker's `return_schema` with a mandatory `anomalies`
/// string field — the §5.2 "dedicated channel for surprises" so the worker
/// can always flag partial/uncertain/unexpected results alongside the
/// contract fields. Idempotent: re-adding is a no-op.
fn with_anomalies_channel(schema: &serde_json::Value) -> serde_json::Value {
    let mut s = schema.clone();
    let obj = match s.as_object_mut() {
        Some(o) => o,
        // Not an object schema — wrap it so the worker still has the channel.
        None => {
            return serde_json::json!({
                "type": "object",
                "properties": { "anomalies": { "type": "string" } },
                "required": ["anomalies"],
            });
        }
    };
    obj.entry("type").or_insert_with(|| serde_json::json!("object"));
    match obj
        .get_mut("properties")
        .and_then(|p| p.as_object_mut())
    {
        Some(props) => {
            props.entry("anomalies").or_insert_with(|| {
                serde_json::json!({
                    "type": "string",
                    "description": "Anything surprising, partial, or uncertain; empty if none.",
                })
            });
        }
        None => {
            obj.insert(
                "properties".to_string(),
                serde_json::json!({ "anomalies": { "type": "string" } }),
            );
        }
    }
    match obj.get_mut("required").and_then(|r| r.as_array_mut()) {
        Some(req) => {
            if !req.iter().any(|v| v == "anomalies") {
                req.push(serde_json::json!("anomalies"));
            }
        }
        None => {
            obj.insert("required".to_string(), serde_json::json!(["anomalies"]));
        }
    }
    s
}

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
            StepOutput::Text(_)
            | StepOutput::Json(_)
            | StepOutput::ReasonWithToolsResult { .. } => "done",
            StepOutput::Jump(t) => {
                tracing::info!(
                    step_id = step.id,
                    description = %step.description,
                    jump_to = t,
                    "executor: step jump"
                );
                return;
            }
            StepOutput::Skipped => "skipped",
        };
        tracing::info!(
            step_id = step.id,
            description = %step.description,
            status,
            "executor: step progress"
        );
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
                    // Composition glassbox (per ARCH_PRINCIPLES §9):
                    // pulling a key that isn't in the Json output
                    // used to silently resolve to "". That breaks
                    // compositions in ways operators can't see. Now
                    // we emit a tracing::warn! naming the missing
                    // key and the step it came from.
                    match v.get(&input.key) {
                        Some(val) => val.to_string(),
                        None => {
                            tracing::warn!(
                                from_step = input.step_id,
                                key = %input.key,
                                available = ?v.as_object().map(|o| o.keys().collect::<Vec<_>>()),
                                "resolve_inputs: key not present in upstream Json output — \
                                 downstream template will see an empty string. Check the \
                                 upstream tool's `output_schema` for the correct key."
                            );
                            String::new()
                        }
                    }
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

        completed
            .entry(skipped_target)
            .or_insert(StepOutput::Skipped);

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

            ctx.task.completed_steps = ctx.completed.iter().map(|(&k, v)| (k, v.clone())).collect();
            ctx.task.updated_at = now();
            let _ = self.store.save_task(&ctx.task).await;
        }

        Ok(ExecutionResult {
            completed: ctx.completed.clone(),
            error: None,
        })
    }

    /// Run a context-firewall worker (a [`StepKind::Delegate`]). The worker
    /// drives a scoped rich-param tool loop in its OWN local context — raw
    /// tool observations accumulate in `transcript` here and are dropped when
    /// this method returns. Only a typed contract (the `return_schema` fields
    /// plus an `anomalies` channel) flows back to the orchestrator, so the
    /// planner never sees the DOM / cells the worker waded through. Shares the
    /// `{name, arguments}` parser + schema projection with the recipe-author
    /// loop via [`crate::tool_loop`].
    ///
    /// NOTE (v1): the worker's internal tool calls go straight to
    /// `tool.execute`, so they do NOT pass through the executor's idempotency
    /// ledger (#4). A `NonIdempotent` actuator used *inside* a worker is not
    /// yet replay-guarded — threading the ledger into the worker loop is a
    /// follow-on.
    async fn execute_delegate(
        &self,
        goal: &str,
        tool_ids: &[ToolId],
        return_schema: &serde_json::Value,
        max_iterations: usize,
        task: &Task,
    ) -> Result<StepOutput> {
        use crate::tool_loop::{format_step_output, parse_assistant_text, tool_schemas_for};

        let descriptors: Vec<ToolDescriptor> = tool_ids
            .iter()
            .filter_map(|id| self.tools.get(id).ok().map(|t| t.descriptor()))
            .collect();
        let tool_schemas = tool_schemas_for(&descriptors);
        let tool_list = descriptors
            .iter()
            .map(|d| format!("- {} (id: {}): {}", d.name, d.id, d.description))
            .collect::<Vec<_>>()
            .join("\n");

        let system = format!(
            "You are a focused sub-agent. Complete EXACTLY this subtask using \
             only the tools below, one call at a time as \
             `<tool_call>{{\"name\":\"<tool>\",\"arguments\":{{...}}}}</tool_call>`. \
             When you have what the subtask needs, reply WITHOUT a tool call and \
             state your findings concisely.\n\nTools:\n{tool_list}\n\nSubtask: {goal}"
        );
        let ctx = ToolContext {
            conversation_id: task.conversation_id.clone(),
            task_id: Some(task.id.clone()),
            working_directory: None,
            in_reasoning_loop: true,
            agent_session_token: None,
            turn_index: 0,
        };

        // The worker's fat context — raw tool results accumulate here and are
        // dropped when this fn returns. This is the firewall.
        let mut transcript = format!("{system}\n\nAssistant:");
        let mut findings = String::new();
        let mut calls_made = 0usize;

        for _ in 0..max_iterations.max(1) {
            let mut req = CompletionRequest::new(&transcript).with_speed(Speed::Slow);
            req.tools = Some(tool_schemas.clone());
            req.max_tokens = Some(2048);
            let resp = self.inference.complete(&req).await?;
            let (visible, calls) = parse_assistant_text(&resp.text);
            if !visible.trim().is_empty() {
                findings = visible.clone();
            }
            if calls.is_empty() {
                break; // worker signalled done (no tool call this turn)
            }
            transcript.push_str(&visible);
            for call in &calls {
                let result = match self.tools.get(&call.name) {
                    Ok(tool) => match tool.execute(&call.arguments, &ctx).await {
                        Ok(out) => format_step_output(&out),
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    },
                    Err(_) => serde_json::json!({
                        "error": format!("tool '{}' not available", call.name)
                    })
                    .to_string(),
                };
                calls_made += 1;
                tracing::info!(
                    target: "executor.delegate",
                    tool = %call.name,
                    "delegate worker tool call (observation stays in worker)"
                );
                transcript.push_str(&format!(
                    "\n<tool_call>{}</tool_call>\nResult: {result}\n",
                    serde_json::json!({ "name": call.name, "arguments": call.arguments })
                ));
            }
            transcript.push_str("\nAssistant:");
        }

        // Formalize the worker's findings into the typed contract. The schema
        // gets a mandatory `anomalies` channel (§5.2 surprises). Only this
        // contract flows back to the orchestrator.
        let contract_schema = with_anomalies_channel(return_schema);
        let synth_prompt = format!(
            "From your findings, output ONLY the JSON the schema requires. Put \
             anything surprising, partial, or uncertain in `anomalies` (empty \
             string if none).\n\nFindings:\n{findings}"
        );
        let mut synth_req = CompletionRequest::new(&synth_prompt).with_speed(Speed::Slow);
        synth_req.structured_output = Some(contract_schema);
        synth_req.max_tokens = Some(1024);
        let synth = self.inference.complete(&synth_req).await?;
        let contract: serde_json::Value = serde_json::from_str(synth.text.trim())
            .unwrap_or_else(|_| {
                serde_json::json!({
                    "anomalies": format!("worker output did not parse: {}", synth.text.trim())
                })
            });

        tracing::info!(
            target: "executor.delegate",
            tool_calls = calls_made,
            "delegate worker returned a typed contract (raw observations firewalled)"
        );
        Ok(StepOutput::Json(contract))
    }

    async fn execute_step(
        &self,
        step: &Step,
        completed: &HashMap<usize, StepOutput>,
        task: &Task,
    ) -> Result<StepOutput> {
        let step_start = std::time::Instant::now();
        let kind_name = match &step.kind {
            StepKind::Reason { .. } => "Reason",
            StepKind::Tool { .. } => "Tool",
            StepKind::ReasonWithTools { .. } => "ReasonWithTools",
            StepKind::Branch { .. } => "Branch",
            StepKind::UserInput { .. } => "UserInput",
            StepKind::AwaitUserInfo { .. } => "AwaitUserInfo",
            StepKind::Delegate { .. } => "Delegate",
        };
        tracing::info!(
            step_id = step.id,
            kind = kind_name,
            description = %step.description,
            "executor: step begin"
        );

        let result = self.execute_step_inner(step, completed, task).await;

        tracing::info!(
            step_id = step.id,
            kind = kind_name,
            success = result.is_ok(),
            latency_ms = step_start.elapsed().as_millis() as u64,
            "executor: step done"
        );
        result
    }

    async fn execute_step_inner(
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
                let system_message =
                    if let Some(overrides) = self.skills.prompt_overrides(&Intent::ComplexTask) {
                        format!("{overrides}\n\n{base_system}")
                    } else {
                        base_system.to_string()
                    };

                // Attach OICP requirements from active skills. If
                // skills haven't declared a latency class, derive one
                // from the step's Speed so the scheduler ranks fast
                // steps against fast claims and deep steps against
                // extended claims.
                let oicp_req = self.skills.inference_requirements();
                let default_class = match speed {
                    Speed::Fast => LatencyClass::Fast,
                    Speed::Medium => LatencyClass::Normal,
                    Speed::Slow => LatencyClass::Extended,
                };
                let oicp = if oicp_req.capability_hint.is_none()
                    && oicp_req.latency_class.is_none()
                    && oicp_req.context_tokens.is_none()
                    && oicp_req.max_output_tokens.is_none()
                {
                    None
                } else {
                    // Only override latency_class if the skill didn't
                    // declare one itself — skills know best.
                    let mut req = oicp_req;
                    if req.latency_class.is_none() {
                        req = req.with_latency_class(default_class);
                    }
                    Some(req)
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
                    think_budget: None,
                    top_k: None,
                    top_p: None,
                    oicp,
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
                let params_str =
                    serde_json::to_string(params).map_err(|e| Error::Execution(e.to_string()))?;
                let resolved_str = resolve_inputs(&params_str, &step.inputs, completed)?;
                let resolved_params: serde_json::Value = serde_json::from_str(&resolved_str)
                    .map_err(|e| Error::Execution(format!("Invalid resolved params: {e}")))?;

                // 2. Get tool.
                tracing::info!(
                    tool_id = tool_id,
                    params = %resolved_params,
                    "Executing tool step"
                );
                let tool = self.tools.get(tool_id)?;

                // 3. Check permissions.
                let perms = tool.required_permissions();
                // Phase 1 observability: a Write/ReadWrite tool with
                // an empty permissions vec bypasses approval entirely
                // — that's almost always a missing declaration, not a
                // deliberate choice. Surface it so per-tool permission
                // tightening (see §12 Roadmap) has a visible signal.
                if perms.is_empty() && tool.descriptor().effect != Effect::Read {
                    tracing::warn!(
                        tool_id = %tool_id,
                        effect = ?tool.descriptor().effect,
                        "executor: write-effectful tool has empty required_permissions \
                         — approval gate will not fire; declare permissions or wait for \
                         Phase 1.5 MCP-parity fix"
                    );
                }
                for permission in perms {
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

                            let approved = self.approval.request_approval(step, &preview).await?;

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
                    agent_session_token: None,
                    turn_index: 0,
                };

                let retry = tool.retry_config().unwrap_or_default();
                // Phase 1 retry gate: a NonIdempotent tool never
                // auto-retries, regardless of its retry_config or the
                // error-string heuristic. Retrying a write that
                // actually succeeded but returned a transient-looking
                // error would create duplicate notes / emails /
                // calendar entries. See ARCH_PRINCIPLES §7.
                let descriptor = tool.descriptor();
                let retry_is_safe = descriptor.idempotency == Idempotency::Idempotent;
                let mut last_error = None;

                // Idempotency ledger (replay safety). Only NonIdempotent
                // tools carry a durable attempt record — they are the exact
                // set where a second execution duplicates a side-effect
                // (email, calendar, note). The key is content-derived, so it
                // also catches a *replan* that re-issues the same action
                // under a new step_id, not just a same-plan resume.
                let mut ledger_id: Option<String> = None;
                if descriptor.idempotency == Idempotency::NonIdempotent {
                    let key = idempotency_key(&task.id, tool_id, &resolved_params);
                    if let Some(prior) = self.store.find_execution(&key).await? {
                        match prior.status {
                            ExecutionStatus::Completed => {
                                // This exact action already succeeded earlier
                                // in the task (a replan or duplicate step).
                                // Reuse the recorded result; do not re-run.
                                tracing::info!(
                                    target: "executor.execution",
                                    tool_id = %tool_id,
                                    step_id = step.id,
                                    idempotency_key = %key,
                                    "NonIdempotent action already completed — skipping re-execution"
                                );
                                return Ok(StepOutput::Text(prior.summary.unwrap_or_default()));
                            }
                            ExecutionStatus::Started => {
                                // A prior attempt began this side-effect and
                                // never recorded completion — a crash in the
                                // gap. Blind-replaying could duplicate it, so
                                // halt and surface for review rather than
                                // guess whether it landed. ARCH §7.
                                tracing::warn!(
                                    target: "executor.execution",
                                    tool_id = %tool_id,
                                    step_id = step.id,
                                    idempotency_key = %key,
                                    "NonIdempotent action was in flight at a prior interruption — halting to avoid a duplicate side-effect"
                                );
                                return Err(Error::Execution(format!(
                                    "step {} ({tool_id}) may have already executed its \
                                     non-idempotent side-effect before an interruption \
                                     (idempotency_key={key}); halting to avoid a duplicate — \
                                     resolve the prior attempt before resuming",
                                    step.id
                                )));
                            }
                            ExecutionStatus::Failed => {
                                // Prior attempt recorded as failed; re-attempt.
                            }
                        }
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    self.store
                        .record_started(&StepExecution {
                            id: id.clone(),
                            task_id: task.id.clone(),
                            step_id: step.id,
                            tool_id: tool_id.to_string(),
                            status: ExecutionStatus::Started,
                            idempotency_key: key,
                            summary: None,
                            anomalies: None,
                            started_at: now(),
                            ended_at: None,
                        })
                        .await?;
                    tracing::info!(
                        target: "executor.execution",
                        tool_id = %tool_id,
                        step_id = step.id,
                        execution_id = %id,
                        "NonIdempotent action started — durable attempt recorded"
                    );
                    ledger_id = Some(id);
                }

                for attempt in 0..=retry.max_retries {
                    if attempt > 0 {
                        let delay = retry.backoff_ms.get(attempt - 1).copied().unwrap_or(3000);
                        tracing::info!(
                            tool_id = %tool_id,
                            attempt,
                            max_retries = retry.max_retries,
                            delay_ms = delay,
                            "executor: tool retry"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }

                    // Tier 4: dispatch via `call_cached` so the
                    // ReasonWithTools loop benefits from the shared
                    // tool-result cache. Idempotent reads
                    // (knowledge_lookup, code-intel) hit the cache
                    // on duplicate args; non-idempotent tools
                    // bypass entirely per descriptor.idempotency.
                    match self
                        .tools
                        .call_cached(tool_id, &resolved_params, &tool_ctx)
                        .await
                    {
                        Ok(output) => {
                            if let Some(ref eid) = ledger_id {
                                // Best-effort: a failed completion write leaves
                                // the row `Started`, which on resume halts (a
                                // spurious review) rather than re-running — the
                                // safe direction, never a duplicate side-effect.
                                let _ = self
                                    .store
                                    .mark_completed(eid, summarize_output(&output), None)
                                    .await;
                            }
                            return Ok(output);
                        }
                        Err(e) => {
                            let msg = e.to_string().to_lowercase();
                            let retryable = msg.contains("timeout")
                                || msg.contains("rate limit")
                                || msg.contains("connection")
                                || msg.contains("temporarily");
                            if retryable && retry_is_safe && attempt < retry.max_retries {
                                last_error = Some(e);
                                continue;
                            }
                            if retryable && !retry_is_safe && attempt < retry.max_retries {
                                tracing::warn!(
                                    tool_id = %tool_id,
                                    error = %e,
                                    "executor: NonIdempotent tool failed with a transient-looking \
                                     error; retry suppressed to avoid duplicate side-effect"
                                );
                            }
                            if let Some(ref eid) = ledger_id {
                                let _ = self.store.mark_failed(eid, &e.to_string()).await;
                            }
                            return Err(e);
                        }
                    }
                }

                Err(last_error
                    .unwrap_or_else(|| Error::Execution("All retries exhausted".to_string())))
            }

            StepKind::UserInput { question } => {
                let resolved = resolve_inputs(question, &step.inputs, completed)?;
                let answer = self.approval.ask_user(&resolved).await?;
                Ok(StepOutput::Text(answer))
            }

            StepKind::AwaitUserInfo { request } => {
                // Stamp the executor-known task/step ids so the UI can
                // correlate the request back to the suspended task.
                // Also stamp kind = StepBlock and task_title so the
                // card can render its "task paused" chrome instead of
                // the post-answer "sharpen this" chrome (the two
                // share one payload but have distinct UX contracts —
                // see InformationRequestKind doc).
                let mut req = request.clone();
                req.task_id = task.id.clone();
                req.step_id = step.id;
                req.kind = crate::types::InformationRequestKind::StepBlock;
                req.task_title = task.goal.clone();
                tracing::info!(
                    step_id = step.id,
                    gap_chars = req.gap.len(),
                    task_title_chars = req.task_title.len(),
                    "executor: awaiting user info"
                );
                let resolved = self.approval.request_information(&req).await;
                let content = resolved.unwrap_or_default();
                tracing::info!(
                    step_id = step.id,
                    skipped = content.is_empty(),
                    content_chars = content.len(),
                    "executor: user info resolved"
                );
                Ok(StepOutput::Text(content))
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

            StepKind::Delegate {
                goal,
                tools,
                return_schema,
                max_iterations,
            } => {
                let resolved_goal = resolve_inputs(goal, &step.inputs, completed)?;
                self.execute_delegate(
                    &resolved_goal,
                    tools,
                    return_schema,
                    *max_iterations,
                    task,
                )
                .await
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
        // Build tool descriptions for the system prompt. Annotation
        // matches the planner prompt format (Phase 1.4) so the agent
        // sees consistent effect/scope/latency tags across both paths.
        let tool_descs: Vec<String> = available_tools
            .iter()
            .filter_map(|id| self.tools.get(id).ok())
            .map(|t| {
                let d = t.descriptor();
                let effect = match d.effect {
                    Effect::Read => "Read",
                    Effect::Write => "Write",
                    Effect::ReadWrite => "ReadWrite",
                };
                let scope = match d.scope {
                    Scope::Session => "Session",
                    Scope::Persistent => "Persistent",
                    Scope::External => "External",
                };
                let latency = match d.latency {
                    Latency::Instant => "Instant",
                    Latency::Fast => "Fast",
                    Latency::Slow => "Slow",
                    Latency::Streaming => "Streaming",
                };
                // Composition hint: surface the tool's declared
                // output keys so the agent can compose plans with
                // {N.key} template substitution instead of piping
                // the whole text output through reasoning.
                let output_hint = d
                    .output_schema
                    .as_ref()
                    .and_then(|s| s.get("properties"))
                    .and_then(|p| p.as_object())
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .filter(|k| !k.is_empty())
                    .map(|k| format!("  (output keys: {})", k.join(", ")))
                    .unwrap_or_default();
                format!(
                    "- {} (id: {}) [{effect} · {scope} · {latency}]: {}{}",
                    d.name, d.id, d.description, output_hint
                )
            })
            .collect();

        // Phase 2 signals: poll each available tool's cheap-state
        // summary. Most tools return None. The ones that don't give
        // the agent peripheral awareness of stale/failing/backlogged
        // state every turn — replaces the ad-hoc REFLECT_HINT_INTERVAL
        // text nudge in mcp_router with structured preamble data.
        let mut tool_signals: Vec<String> = Vec::new();
        for id in available_tools {
            if let Ok(tool) = self.tools.get(id) {
                if let Some(s) = tool.signal().await {
                    tool_signals.push(format!("- {id}: {s}"));
                }
            }
        }

        let system =
            self.build_retrieval_reasoning_prompt(&tool_descs, &tool_signals, max_iterations);

        // Build the growing conversation as a single prompt.
        let mut conversation = format!("{system}\n\n---\n\nUser question: {prompt}\n\nAssistant:");

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
                think_budget: None,
                top_k: None,
                top_p: None,
                oicp: None,
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
            };

            let response = self.inference.complete(&request).await?;
            let response_text = response.text.trim().to_string();

            // Check if the model emitted a tool call.
            if let Some(tool_call) = parse_tool_call(&response_text) {
                // Find and execute the tool.
                let tool_result = match self.tools.get(&tool_call.tool_id) {
                    Ok(_tool) => {
                        let params = serde_json::json!({"query": tool_call.query});
                        let ctx = ToolContext {
                            conversation_id: task.conversation_id.clone(),
                            task_id: Some(task.id.clone()),
                            working_directory: None,
                            in_reasoning_loop: true,
                            agent_session_token: None,
                            turn_index: 0,
                        };
                        // Tier 4: cache-aware dispatch.
                        match self
                            .tools
                            .call_cached(&tool_call.tool_id, &params, &ctx)
                            .await
                        {
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
                        think_budget: None,
                        top_k: None,
                        top_p: None,
                        oicp: None,
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
        tool_signals: &[String],
        max_iterations: usize,
    ) -> String {
        let tools_list = tool_descriptions.join("\n");

        // Phase 2: render an optional `## Tool state` block only when
        // at least one tool returned a non-None signal. Silent
        // otherwise (zero noise on a clean system). See ARCH_PRINCIPLES
        // §9 for the glassbox rationale.
        let signal_block = if tool_signals.is_empty() {
            String::new()
        } else {
            format!("\n\n## Tool state\n\n{}", tool_signals.join("\n"))
        };

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
- Cite sources using [Source: name] notation for every claim backed by search results.
- If you make a claim that is NOT directly supported by your search results,
  mark it with [unverified] so the user knows it comes from your general
  knowledge rather than a retrieved source.
- If sources conflict, present both positions.
- If you couldn't find part of the answer, say so explicitly.

## Available tools

{tools_list}{signal_block}

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
            SampleSelector::LlmJudge {
                selection_prompt,
                preset,
            } => {
                let numbered = candidates
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("--- Response {} ---\n{}", i + 1, c))
                    .collect::<Vec<_>>()
                    .join("\n\n");

                // Rubric resolution order: explicit `selection_prompt` wins
                // (caller-supplied custom rubric), otherwise the named
                // preset's prompt, otherwise the default factual rubric.
                let rubric: &str = if let Some(custom) = selection_prompt.as_deref() {
                    custom
                } else {
                    judge_rubric_for_preset(*preset)
                };

                let judge_prompt = format!(
                    "{}\n\nOriginal task:\n{}\n\nCandidate responses:\n{}\n\n\
                     Select the best response. Return only the number (1-{}).",
                    rubric,
                    &original_prompt[..original_prompt.len().min(500)],
                    numbered,
                    candidates.len()
                );

                let request = CompletionRequest::new(&judge_prompt).with_speed(Speed::Fast);
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
                // Confirm the tool exists before iterating
                // candidates — `call_cached` would also fail with
                // ToolNotFound, but checking once up-front gives a
                // cleaner failure mode.
                self.tools.get(tool_id)?;
                let ctx = ToolContext {
                    conversation_id: String::new(),
                    task_id: None,
                    working_directory: None,
                    in_reasoning_loop: false,
                    agent_session_token: None,
                    turn_index: 0,
                };
                for candidate in candidates {
                    let params = serde_json::json!({"input": candidate});
                    // Tier 4: empty conversation_id means the cache
                    // skips this call entirely (per `call_cached`
                    // logic). Verify is one-off discrimination
                    // across candidates — no value in caching.
                    if self.tools.call_cached(tool_id, &params, &ctx).await.is_ok() {
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
                // char-aware truncation: `&text[..2000]` panics when byte 2000
                // lands mid-character on multi-byte step output (same class as
                // the router log-slice panic, breaker 2026-06-25).
                crate::runtime::truncate_with_ellipsis(text, 2000)
            );

            let eval_request =
                CompletionRequest::new(&eval_prompt).with_speed(eval_config.eval_speed);
            let eval_response = self.inference.complete(&eval_request).await?;

            // Parse evaluation result.
            let passed = eval_response.text.contains("\"pass\": true")
                || eval_response.text.contains("\"pass\":true");

            if passed {
                return Ok(output);
            }

            if retry >= eval_config.max_retries {
                tracing::warn!(
                    retries = retry,
                    "executor: evaluation failed after retries, accepting output"
                );
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
                        selector: SampleSelector::default_judge(),
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
        let result =
            resolve_inputs("Compare: {0.output} vs {1.output}", &inputs, &completed).unwrap();
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
