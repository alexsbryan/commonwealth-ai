// SPDX-License-Identifier: AGPL-3.0-or-later
//! The step-output reference grammar shared by the planner and the
//! executor — the one plan-language convention that genuinely spans
//! both files.
//!
//! The planner's system prompt teaches the model to write
//! `{N.output}` / `{N.key}` placeholders inside step prompts and to
//! declare each referenced step in `inputs` (default key:
//! [`DEFAULT_OUTPUT_KEY`]). The executor substitutes those
//! placeholders with upstream step outputs via [`resolve_inputs`]
//! before dispatching a step. Until 2026-07 the emit side (prompt
//! text + the `"output"` default in `parse_plan_json`) lived in
//! `planner.rs` while the parse side (`resolve_inputs`, with its own
//! copy of the format strings) lived in `executor.rs` — an 18-month
//! git-coupling analysis flagged the pair as hidden coupling (25
//! joint commits, no structural edge). This module owns both halves:
//! the placeholder format strings, the default key, and the
//! resolver. A grammar change is now a one-file edit, and the unit
//! tests round-trip every placeholder form the planner documents.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::types::{StepInput, StepOutput};

/// The default key a step input resolves when the plan JSON omits
/// `"key"` — and the key name the `{N.output}` shorthand refers to.
/// Used by the planner (`parse_plan_json` input parsing) and by the
/// executor-side [`resolve_inputs`] substitution.
pub const DEFAULT_OUTPUT_KEY: &str = "output";

/// Emit form of the whole-output placeholder: `{N.output}`.
pub fn output_placeholder(step_id: usize) -> String {
    key_placeholder(step_id, DEFAULT_OUTPUT_KEY)
}

/// Emit form of the keyed placeholder: `{N.key}` — pulls one field
/// out of an upstream step's JSON output.
pub fn key_placeholder(step_id: usize, key: &str) -> String {
    format!("{{{step_id}.{key}}}")
}

/// Substitute `{N.output}` / `{N.key}` placeholders in a step's
/// prompt template with the outputs of completed upstream steps.
///
/// This is the parse side of the grammar the planner prompt emits.
/// Every step named in `inputs` must already be present in
/// `completed`; a missing step is an execution error (the DAG
/// scheduler should never dispatch a step before its inputs).
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
                if input.key == DEFAULT_OUTPUT_KEY {
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

        let placeholder_output = output_placeholder(input.step_id);
        let placeholder_key = key_placeholder(input.step_id, &input.key);
        result = result.replace(&placeholder_output, &value);
        result = result.replace(&placeholder_key, &value);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Emit ↔ parse round-trips ───────────────────────────────

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
        // Emit the placeholder through the same helper the resolver
        // matches against — the round-trip that keeps the two format
        // strings one format string.
        let template = format!("Hello {}", key_placeholder(0, "name"));
        let result = resolve_inputs(&template, &inputs, &completed).unwrap();
        assert_eq!(result, "Hello \"Alice\"");
    }

    #[test]
    fn resolve_inputs_no_inputs() {
        let completed = HashMap::new();
        let result = resolve_inputs("no placeholders here", &[], &completed).unwrap();
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn output_placeholder_round_trips() {
        let mut completed = HashMap::new();
        completed.insert(3, StepOutput::Text("payload".to_string()));
        let inputs = vec![StepInput {
            step_id: 3,
            key: DEFAULT_OUTPUT_KEY.to_string(),
        }];
        let template = format!("Given: {}", output_placeholder(3));
        let result = resolve_inputs(&template, &inputs, &completed).unwrap();
        assert_eq!(result, "Given: payload");
    }

    // ── Cross-file grammar sync (planner ↔ executor) ───────────

    /// The planner's system prompt must document exactly the
    /// placeholder forms this module resolves. If the emit syntax
    /// changes, this test fails until the prompt and the resolver
    /// move together — the coupling the git history showed being
    /// mirrored by hand.
    #[test]
    fn planner_prompt_documents_this_grammar() {
        let prompt = crate::planner::PLAN_SYSTEM_PROMPT;
        // SCHEMA example uses the concrete step-0 form.
        assert!(
            prompt.contains(&output_placeholder(0)),
            "planner SCHEMA example no longer shows `{}`",
            output_placeholder(0)
        );
        // RULES line documents the general form with N in place of a
        // concrete id — same format string, same default key.
        let generic = format!("{{N.{DEFAULT_OUTPUT_KEY}}}");
        assert!(
            prompt.contains(&generic),
            "planner RULES no longer document `{generic}`"
        );
    }

    /// Full round-trip across the pair: a plan parsed by the planner
    /// (input `key` omitted → DEFAULT_OUTPUT_KEY) resolves through
    /// the executor-side resolver.
    #[test]
    fn parsed_plan_inputs_resolve_through_grammar() {
        let json = r#"{
            "goal": "g",
            "steps": [
                {"id": 0, "description": "a", "kind": "reason", "prompt": "x"},
                {"id": 1, "description": "b", "kind": "reason", "prompt": "use {0.output}", "inputs": [{"step_id": 0}]}
            ],
            "edges": [[0, 1]]
        }"#;
        let plan = crate::planner::parse_plan_json(json, "g").unwrap();
        assert_eq!(plan.steps[1].inputs.len(), 1);
        assert_eq!(plan.steps[1].inputs[0].key, DEFAULT_OUTPUT_KEY);

        let mut completed = HashMap::new();
        completed.insert(0, StepOutput::Text("hello".to_string()));
        let resolved = resolve_inputs("use {0.output}", &plan.steps[1].inputs, &completed).unwrap();
        assert_eq!(resolved, "use hello");
    }
}
