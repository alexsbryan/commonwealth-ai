use std::sync::Arc;

use async_trait::async_trait;

use crate::context::format_history_as_prompt;
use crate::error::{Error, Result};
use crate::skills::SkillRegistry;
use crate::traits::{InferenceProvider, Planner};
use crate::types::*;

/// LLM-based planner that uses the Primary inference slot to generate execution plans.
///
/// Uses a flat JSON schema that small models (7-14B) can reliably produce.
/// Retries up to 2 times on parse failure, with a guaranteed fallback to a
/// single-step Reason plan.
pub struct LlmPlanner {
    inference: Arc<dyn InferenceProvider>,
    skills: Arc<SkillRegistry>,
}

impl LlmPlanner {
    pub fn new(inference: Arc<dyn InferenceProvider>, skills: Arc<SkillRegistry>) -> Self {
        Self { inference, skills }
    }
}

#[async_trait]
impl Planner for LlmPlanner {
    async fn plan(
        &self,
        goal: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Plan> {
        let context_summary = format_history_as_prompt(context, 4);
        let mut last_error = String::new();

        // Check for matching skill templates.
        let templates = self.skills.planner_templates(&Intent::ComplexTask);
        let template_hint = find_matching_template(&templates, goal);

        for attempt in 0..3 {
            let mut prompt =
                build_plan_prompt(goal, &context_summary, available_tools, &last_error);

            if let Some(ref hint) = template_hint {
                prompt.push_str(&format!(
                    "\n\nA suggested plan template for this type of task:\n{hint}\n\n\
                     You may use this as a starting point and adapt it to the specific goal."
                ));
            }

            let request = CompletionRequest {
                prompt,
                system_message: Some(PLAN_SYSTEM_PROMPT.to_string()),
                preferred_speed: Speed::Slow,
                max_tokens: Some(1024),
                temperature: Some(0.0),
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

            match extract_json(&response.text).and_then(|j| parse_plan_json(&j, goal)) {
                Ok(plan) => {
                    eprintln!(
                        "[planner] Generated plan: {} steps, {} edges (attempt {})",
                        plan.steps.len(),
                        plan.edges.len(),
                        attempt + 1,
                    );
                    return Ok(plan);
                }
                Err(e) => {
                    eprintln!(
                        "[planner] Parse failed (attempt {}): {}",
                        attempt + 1,
                        e
                    );
                    last_error = format!(
                        "Your previous output could not be parsed: {e}. Output ONLY valid JSON matching the schema."
                    );
                }
            }
        }

        eprintln!("[planner] All attempts failed, using fallback plan");
        Ok(fallback_plan(goal))
    }

    async fn replan(
        &self,
        original: &Plan,
        completed: &[(usize, StepOutput)],
        failure: &StepError,
    ) -> Result<Plan> {
        let completed_summary: Vec<String> = completed
            .iter()
            .map(|(id, output)| {
                let out_str = match output {
                    StepOutput::Text(t) => t.chars().take(200).collect::<String>(),
                    StepOutput::Json(v) => serde_json::to_string(v).unwrap_or_default(),
                    StepOutput::ReasonWithToolsResult { text, iterations, .. } => {
                        format!("({iterations} searches) {}", text.chars().take(200).collect::<String>())
                    }
                    StepOutput::Jump(t) => format!("jumped to step {t}"),
                    StepOutput::Skipped => "skipped".to_string(),
                };
                format!("Step {id}: {out_str}")
            })
            .collect();

        let prompt = format!(
            "The original plan for \"{}\" failed at step {} with error: {}\n\n\
             Completed steps:\n{}\n\n\
             Create a new plan that accounts for what already succeeded and works around the failure.\n\
             Goal: {}",
            original.goal,
            failure.step_id,
            failure.message,
            completed_summary.join("\n"),
            original.goal,
        );

        let request = CompletionRequest {
            prompt,
            system_message: Some(PLAN_SYSTEM_PROMPT.to_string()),
            preferred_speed: Speed::Slow,
            max_tokens: Some(1024),
            temperature: Some(0.0),
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

        match extract_json(&response.text).and_then(|j| parse_plan_json(&j, &original.goal)) {
            Ok(plan) => {
                eprintln!("[planner] Replan succeeded: {} steps", plan.steps.len());
                Ok(plan)
            }
            Err(_) => {
                eprintln!("[planner] Replan parse failed, using fallback");
                Ok(fallback_plan(&original.goal))
            }
        }
    }
}

// ─── Prompt Templates ──────────────────────────────────────────

const PLAN_SYSTEM_PROMPT: &str = r#"You are a planning assistant. Given a goal, create a step-by-step execution plan as a JSON object.

SCHEMA:
{
  "goal": "<restate the goal>",
  "steps": [
    {"id": 0, "description": "<what>", "kind": "tool", "tool_id": "<tool name>", "params": {"query": "<search query>"}, "inputs": []},
    {"id": 1, "description": "<what>", "kind": "reason", "prompt": "Given the following: {0.output}\n<prompt>", "speed": "slow", "inputs": [{"step_id": 0, "key": "output"}]}
  ],
  "edges": [[0, 1]]
}

STEP KINDS:
- "reason": Thinking/analysis. Requires "prompt" and "speed" ("fast" or "slow").
- "tool": Execute a tool. Requires "tool_id" (must match an available tool name) and "params" (JSON object passed to the tool).
- "reason_with_tools": Iterative research. The model thinks, searches, examines results, and searches again as needed. Requires "prompt", "speed", "tools" (list of tool IDs like ["search"]), and "max_iterations" (number, typically 6). Use for complex questions needing multiple searches.
- "await_user_info": Suspend the task and surface a structured information request to the user. The output is whatever content the user pastes back (or empty on skip). Use when the corpus is genuinely insufficient and a specific external source would resolve the question. Optionally include a pre-filled "request" object with fields {current_understanding, gap, relevance, satisfying_source, search_hints}.

RULES:
- Step IDs start at 0 and increment by 1
- "edges" lists [from, to] pairs showing dependencies
- IMPORTANT: Edge IDs must reference step IDs that exist in the "steps" array. If you have N steps, valid IDs are 0 through N-1. Do NOT reference step IDs that don't exist.
- Use {N.output} in "prompt" to reference step N's output
- "inputs" must list every step referenced in the prompt
- When a question needs current or real-time information, use the web_search tool with a "query" param
- Keep plans simple: 2-5 steps
- Output ONLY the JSON object, nothing else"#;

/// Format a tool's behavioural properties as a compact tag like
/// `[Read · Persistent · Fast]`. Emitted in the planner prompt so the
/// model can pick parallelisable reads vs. gate-required writes
/// without parsing the natural-language description.
fn format_behaviour_tag(t: &ToolDescriptor) -> String {
    let effect = match t.effect {
        Effect::Read => "Read",
        Effect::Write => "Write",
        Effect::ReadWrite => "ReadWrite",
    };
    let scope = match t.scope {
        Scope::Session => "Session",
        Scope::Persistent => "Persistent",
        Scope::External => "External",
    };
    let latency = match t.latency {
        Latency::Instant => "Instant",
        Latency::Fast => "Fast",
        Latency::Slow => "Slow",
        Latency::Streaming => "Streaming",
    };
    format!("[{effect} · {scope} · {latency}]")
}

fn build_plan_prompt(
    goal: &str,
    context_summary: &str,
    available_tools: &[ToolDescriptor],
    error_feedback: &str,
) -> String {
    let mut prompt = format!("Goal: {goal}");

    if !context_summary.is_empty() {
        prompt.push_str(&format!("\n\nConversation context:\n{context_summary}"));
    }

    if !available_tools.is_empty() {
        let tools: String = available_tools
            .iter()
            .map(|t| {
                // Phase 1 annotation: behavioural property tag lets the
                // planner distinguish "safe to call speculatively" from
                // "will persist to disk" mechanically, not by prose parse.
                let tag = format_behaviour_tag(t);
                let mut line = format!("- \"{}\" {tag} — {}", t.id, t.description);
                if let Some(ex) = t.examples.first() {
                    if let Ok(json) = serde_json::to_string(&ex.call) {
                        line.push_str(&format!("\n  Example: {json}"));
                    }
                }
                // Composition hint: when the tool declares an
                // output_schema, show its top-level keys so the
                // planner knows what `{N.key}` references are valid
                // in downstream template substitution.
                if let Some(schema) = &t.output_schema {
                    if let Some(keys) = schema
                        .get("properties")
                        .and_then(|p| p.as_object())
                        .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    {
                        if !keys.is_empty() {
                            line.push_str(&format!(
                                "\n  Output keys: {}",
                                keys.join(", ")
                            ));
                        }
                    }
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n");
        prompt.push_str(&format!(
            "\n\nAvailable tools (use the quoted ID as \"tool_id\" in your plan):\n{tools}"
        ));
    }

    if !error_feedback.is_empty() {
        prompt.push_str(&format!("\n\nIMPORTANT: {error_feedback}"));
    }

    prompt
}

// ─── JSON Extraction and Parsing ───────────────────────────────

/// Extract a JSON object from model output.
/// Tries: (1) ```json fences, (2) first `{` to last `}`.
pub fn extract_json(raw: &str) -> Result<String> {
    // Try fenced JSON block.
    if let Some(start) = raw.find("```json") {
        let after_fence = &raw[start + 7..];
        if let Some(end) = after_fence.find("```") {
            let json = after_fence[..end].trim();
            if !json.is_empty() {
                return Ok(json.to_string());
            }
        }
    }

    // Try raw ``` fences.
    if let Some(start) = raw.find("```") {
        let after_fence = &raw[start + 3..];
        if let Some(end) = after_fence.find("```") {
            let json = after_fence[..end].trim();
            if json.starts_with('{') {
                return Ok(json.to_string());
            }
        }
    }

    // Try finding { to }.
    if let Some(start) = raw.find('{') {
        if let Some(end) = raw.rfind('}') {
            if end > start {
                return Ok(raw[start..=end].to_string());
            }
        }
    }

    Err(Error::Planning("No JSON object found in model output".to_string()))
}

/// Parse a flat JSON plan into a Plan struct.
/// Handles the simplified schema where `kind` is a string, not a tagged enum.
pub fn parse_plan_json(json_str: &str, goal: &str) -> Result<Plan> {
    let raw: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| Error::Planning(format!("Invalid JSON: {e}")))?;

    let obj = raw
        .as_object()
        .ok_or_else(|| Error::Planning("Plan must be a JSON object".to_string()))?;

    let plan_goal = obj
        .get("goal")
        .and_then(|v| v.as_str())
        .unwrap_or(goal)
        .to_string();

    let steps_arr = obj
        .get("steps")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Planning("Plan must have a 'steps' array".to_string()))?;

    if steps_arr.is_empty() {
        return Err(Error::Planning("Plan has no steps".to_string()));
    }

    let mut steps = Vec::new();
    for (i, step_val) in steps_arr.iter().enumerate() {
        let step_obj = step_val
            .as_object()
            .ok_or_else(|| Error::Planning(format!("Step {i} must be an object")))?;

        let id = step_obj
            .get("id")
            .and_then(|v| v.as_u64())
            .unwrap_or(i as u64) as usize;

        let description = step_obj
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("step")
            .to_string();

        let kind_str = step_obj
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("reason");

        let speed_str = step_obj
            .get("speed")
            .and_then(|v| v.as_str())
            .unwrap_or("slow");

        let speed = match speed_str {
            "fast" => Speed::Fast,
            _ => Speed::Slow,
        };

        let kind = match kind_str {
            "tool" => {
                let tool_id = step_obj
                    .get("tool_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let params = step_obj
                    .get("params")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                StepKind::Tool { tool_id, params }
            }
            "branch" => {
                let condition = step_obj
                    .get("condition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let if_true = step_obj
                    .get("if_true")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let if_false = step_obj
                    .get("if_false")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                StepKind::Branch {
                    condition,
                    if_true,
                    if_false,
                }
            }
            "reason_with_tools" => {
                let prompt = step_obj
                    .get("prompt")
                    .or_else(|| step_obj.get("prompt_template"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&description)
                    .to_string();
                let tools: Vec<String> = step_obj
                    .get("tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_else(|| vec!["search".to_string()]);
                let max_iter = step_obj
                    .get("max_iterations")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(6) as usize;
                StepKind::ReasonWithTools {
                    prompt_template: prompt,
                    speed,
                    available_tools: tools,
                    max_iterations: max_iter,
                }
            }
            "await_user_info" => {
                // The planner can either pre-fill the request from the
                // skill template, or leave it as a placeholder that gets
                // populated from a previous step's gap-assessment output.
                // Either way, the executor stamps task_id/step_id at
                // dispatch time.
                let request_obj = step_obj
                    .get("request")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let request: crate::types::InformationRequest =
                    serde_json::from_value(request_obj).unwrap_or(
                        crate::types::InformationRequest {
                            current_understanding: String::new(),
                            gap: step_obj
                                .get("gap")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&description)
                                .to_string(),
                            relevance: String::new(),
                            satisfying_source: String::new(),
                            search_hints: Vec::new(),
                            task_id: String::new(),
                            step_id: 0,
                        },
                    );
                StepKind::AwaitUserInfo { request }
            }
            _ => {
                // Default to Reason.
                let prompt = step_obj
                    .get("prompt")
                    .or_else(|| step_obj.get("prompt_template"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&description)
                    .to_string();
                StepKind::Reason {
                    prompt_template: prompt,
                    speed,
                }
            }
        };

        let inputs = step_obj
            .get("inputs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|inp| {
                        let obj = inp.as_object()?;
                        Some(StepInput {
                            step_id: obj.get("step_id")?.as_u64()? as usize,
                            key: obj
                                .get("key")
                                .and_then(|v| v.as_str())
                                .unwrap_or("output")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        steps.push(Step {
            id,
            description,
            kind,
            requires_approval: false,
            inputs,
            sampling: None,
            evaluation: None,
        });
    }

    let mut edges: Vec<(usize, usize)> = obj
        .get("edges")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|edge| {
                    let e = edge.as_array()?;
                    if e.len() == 2 {
                        Some((e[0].as_u64()? as usize, e[1].as_u64()? as usize))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Validate edges — auto-repair by dropping invalid ones rather than
    // rejecting the entire plan. A plan with a missing edge is better than
    // no plan at all.
    let max_id = steps.len();
    let original_edge_count = edges.len();
    edges.retain(|&(from, to)| {
        if from >= max_id || to >= max_id {
            tracing::warn!(
                from, to, max_id,
                "Dropping invalid edge — references non-existent step"
            );
            return false;
        }
        if from == to {
            tracing::warn!(from, "Dropping self-edge");
            return false;
        }
        true
    });
    if edges.len() < original_edge_count {
        tracing::info!(
            original = original_edge_count,
            retained = edges.len(),
            "Plan edges auto-repaired"
        );
    }

    // If all edges were dropped, add sequential edges so steps run in order.
    if edges.is_empty() && steps.len() > 1 {
        tracing::info!("No valid edges — adding sequential edges");
        for i in 0..steps.len() - 1 {
            edges.push((i, i + 1));
        }
    }

    let plan = Plan {
        id: uuid::Uuid::new_v4().to_string(),
        goal: plan_goal,
        steps,
        edges,
    };

    // Quick cycle check via topological sort — if not all steps are reached, there's a cycle.
    let batches = plan.topological_batches();
    let total_in_batches: usize = batches.iter().map(|b| b.len()).sum();
    if total_in_batches < plan.steps.len() {
        return Err(Error::Planning("Plan contains a cycle".to_string()));
    }

    Ok(plan)
}

/// Find a matching template for a goal based on keyword overlap.
/// Returns the template steps if a match is found.
fn find_matching_template(
    templates: &[&crate::skills::PlanTemplate],
    goal: &str,
) -> Option<String> {
    if templates.is_empty() {
        return None;
    }

    let goal_lower = goal.to_lowercase();
    let goal_words: Vec<&str> = goal_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .collect();

    let mut best_match: Option<(usize, &str)> = None;

    for template in templates {
        let trigger_lower = template.trigger.to_lowercase();
        let overlap = goal_words
            .iter()
            .filter(|w| trigger_lower.contains(**w))
            .count();

        if overlap > 0 {
            if best_match.is_none() || overlap > best_match.unwrap().0 {
                best_match = Some((overlap, &template.steps));
            }
        }
    }

    best_match.map(|(_, steps)| steps.to_string())
}

/// Create a guaranteed single-step fallback plan.
pub fn fallback_plan(goal: &str) -> Plan {
    Plan {
        id: uuid::Uuid::new_v4().to_string(),
        goal: goal.to_string(),
        steps: vec![Step {
            id: 0,
            description: "Answer the question directly".to_string(),
            kind: StepKind::Reason {
                prompt_template: goal.to_string(),
                speed: Speed::Slow,
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_fenced() {
        let input = "Here is the plan:\n```json\n{\"goal\": \"test\"}\n```\nDone.";
        assert_eq!(extract_json(input).unwrap(), "{\"goal\": \"test\"}");
    }

    #[test]
    fn extract_json_raw_fenced() {
        let input = "```\n{\"goal\": \"test\"}\n```";
        assert_eq!(extract_json(input).unwrap(), "{\"goal\": \"test\"}");
    }

    #[test]
    fn extract_json_bare() {
        let input = "The plan is {\"goal\": \"test\", \"steps\": []} and that's it.";
        assert_eq!(
            extract_json(input).unwrap(),
            "{\"goal\": \"test\", \"steps\": []}"
        );
    }

    #[test]
    fn extract_json_none() {
        assert!(extract_json("no json here").is_err());
    }

    #[test]
    fn parse_plan_valid() {
        let json = r#"{
            "goal": "compare languages",
            "steps": [
                {"id": 0, "description": "Analyze Python", "kind": "reason", "prompt": "List Python strengths", "speed": "slow", "inputs": []},
                {"id": 1, "description": "Analyze Rust", "kind": "reason", "prompt": "List Rust strengths", "speed": "slow", "inputs": []},
                {"id": 2, "description": "Compare", "kind": "reason", "prompt": "Given Python: {0.output} and Rust: {1.output}, compare them", "speed": "slow", "inputs": [{"step_id": 0, "key": "output"}, {"step_id": 1, "key": "output"}]}
            ],
            "edges": [[0, 2], [1, 2]]
        }"#;

        let plan = parse_plan_json(json, "compare").unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.edges.len(), 2);
        assert_eq!(plan.goal, "compare languages");
    }

    #[test]
    fn parse_plan_single_step() {
        let json = r#"{"goal": "answer", "steps": [{"id": 0, "description": "think", "kind": "reason", "prompt": "answer this"}], "edges": []}"#;
        let plan = parse_plan_json(json, "test").unwrap();
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn parse_plan_empty_steps_fails() {
        let json = r#"{"goal": "test", "steps": [], "edges": []}"#;
        assert!(parse_plan_json(json, "test").is_err());
    }

    #[test]
    fn parse_plan_invalid_edge_auto_repaired() {
        // Invalid edges are dropped, not rejected. The plan should parse
        // successfully with the invalid edge removed.
        let json = r#"{"goal": "test", "steps": [{"id": 0, "description": "a", "kind": "reason", "prompt": "x"}], "edges": [[0, 5]]}"#;
        let plan = parse_plan_json(json, "test").unwrap();
        assert!(plan.edges.is_empty(), "Invalid edge should be dropped");
    }

    #[test]
    fn parse_plan_self_edge_auto_repaired() {
        let json = r#"{"goal": "test", "steps": [{"id": 0, "description": "a", "kind": "reason", "prompt": "x"}], "edges": [[0, 0]]}"#;
        let plan = parse_plan_json(json, "test").unwrap();
        assert!(plan.edges.is_empty(), "Self-edge should be dropped");
    }

    #[test]
    fn parse_plan_cycle_fails() {
        let json = r#"{"goal": "test", "steps": [
            {"id": 0, "description": "a", "kind": "reason", "prompt": "x"},
            {"id": 1, "description": "b", "kind": "reason", "prompt": "y"}
        ], "edges": [[0, 1], [1, 0]]}"#;
        assert!(parse_plan_json(json, "test").is_err());
    }

    #[test]
    fn fallback_plan_is_valid() {
        let plan = fallback_plan("test goal");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.goal, "test goal");
        assert!(plan.edges.is_empty());
    }

    #[test]
    fn parse_plan_with_branch() {
        let json = r#"{"goal": "test", "steps": [
            {"id": 0, "description": "check", "kind": "branch", "condition": "is it raining?", "if_true": 1, "if_false": 2},
            {"id": 1, "description": "umbrella", "kind": "reason", "prompt": "bring umbrella"},
            {"id": 2, "description": "no umbrella", "kind": "reason", "prompt": "no umbrella needed"}
        ], "edges": [[0, 1], [0, 2]]}"#;

        let plan = parse_plan_json(json, "test").unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(
            plan.steps[0].kind,
            StepKind::Branch {
                if_true: 1,
                if_false: 2,
                ..
            }
        ));
    }
}
