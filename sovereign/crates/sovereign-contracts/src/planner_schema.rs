// SPDX-License-Identifier: AGPL-3.0-or-later
//! The JSON Schema the planner DECODES under — the constraint handed to
//! `structured_output` so a plan is masked into shape at logit level rather
//! than asked for in prose and repaired afterwards.
//!
//! Moved down from `sovereign-core::planner::schema` (2026-09-21) so a crate
//! that must compile the plan schema against the real engine — the inference
//! parity fixture — does not link the runtime hub. The leaf already owns
//! `ToolDescriptor`, so the schema's only input is vocabulary. `sovereign-core`
//! re-exports [`plan_schema`] at its historical path.
//!
//! The step-kind branches, the closed `tool_id` vocabulary, and the field
//! ORDER llguidance will force (see `step_branch` — that order is a contract
//! with the worked example in `sovereign-core`'s `PLAN_SYSTEM_PROMPT`) all
//! live here.

use crate::error::{Error, Result};
use crate::types::*;

/// Bounds on a step's declared `max_iterations`.
///
/// The floor is 1 because the executor already does
/// `max_iterations.max(1)` (`executor.rs:528`) — a declared `0` is silently
/// coerced to one iteration today, and a value the engine rewrites is a value
/// the planner should not be able to state.
///
/// The ceiling exists because the field was previously an unbounded integer:
/// one `delegate` step could declare 1000 iterations and the executor would
/// run them, since it caps nothing (it only ADDS, +2 for
/// `StepDifficulty::Hard`, `executor.rs:1019`). An unbounded loop counter
/// chosen by a model is the kind of thing that should be unrepresentable
/// rather than merely unlikely.
///
/// 12 is HEADROOM over the 6 that the prompt documents as typical — two times
/// the documented figure, leaving 14 as the worst case after the Hard bump. It
/// is NOT a measured optimum and no run was performed to choose it; if a real
/// workload needs more, raise it deliberately rather than treating this as a
/// tuned value.
const MIN_PLANNED_ITERATIONS: u64 = 1;
const MAX_PLANNED_ITERATIONS: u64 = 12;

/// The `max_iterations` sub-schema shared by `reason_with_tools` and
/// `delegate` — one decider for the bound, so the two step kinds cannot drift
/// apart (ARCH §10.6).
fn iteration_count() -> serde_json::Value {
    serde_json::json!({
        "type": "integer",
        "minimum": MIN_PLANNED_ITERATIONS,
        "maximum": MAX_PLANNED_ITERATIONS,
    })
}

/// A free-form JSON object whose keys the schema does not constrain.
///
/// `additionalProperties` is set EXPLICITLY: the engine boundary runs
/// `default_additional_properties_false` over every typed-object node that
/// doesn't declare one, so an unannotated `{"type": "object"}` would arrive at
/// llguidance sealed to the empty object — a block the model could only ever
/// fill with `{}`.
///
/// Used for `delegate`'s `return_schema`, which is a shape the PLANNER invents
/// for a sub-agent rather than one any registered tool declares. Tool `params`
/// are NOT open — see [`tool_params_schema`].
fn open_object() -> serde_json::Value {
    serde_json::json!({ "type": "object", "additionalProperties": true })
}

/// The `params` sub-schema for one tool: the tool's own declared `parameters`,
/// verbatim.
///
/// Verbatim is the point. Copying or summarising it would be a second decider
/// for what the tool accepts, and the two would drift — the tool would start
/// rejecting arguments the grammar still allowed. The prompt renders the SAME
/// schema via `format_param_hint`, so what the model is shown, what it can
/// sample, and what the tool will accept are one thing (ARCH §10.6).
///
/// The typed-object check is the guard against a quiet break. A `parameters`
/// that is `{}`, or a bare `{"properties": …}` with no `"type"`, compiles
/// happily and constrains NOTHING: the plan would carry a schema, return 200,
/// and leave that tool's arguments as free as they were before F3. Refusing
/// names the tool instead.
fn tool_params_schema(t: &ToolDescriptor) -> Result<serde_json::Value> {
    let is_typed_object = t
        .parameters
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "object");
    if !is_typed_object {
        return Err(Error::Planning(format!(
            "tool {:?} declares a `parameters` schema that is not a typed object, so its \
             arguments cannot be masked. Give it `\"type\": \"object\"`. Refused rather \
             than planned with that tool's arguments left unconstrained (ARCH §18.3).",
            t.id
        )));
    }
    Ok(t.parameters.clone())
}

/// One `oneOf` branch: the four fields every step carries, plus the
/// kind-specific ones. The `kind` `const` is the discriminator that proves the
/// branches disjoint — without it llguidance refuses the whole `oneOf` ("oneOf
/// constraints are not supported"), and with F1 that refusal is now an error
/// rather than silent free-form sampling.
///
/// MEASURED 2026-08-19, and load-bearing in a non-obvious way. llguidance
/// emits object keys in the order it ITERATES `properties`, and masks any
/// other order — the `required` array order does not drive it (probed with
/// three permutations; the emitted first key tracked `properties` every time).
///
/// `serde_json::Map` iterates in INSERTION order only because
/// `serde_json/preserve_order` is on, which every binary that runs the planner
/// resolves transitively (sovereign-desktop, sovereign-server,
/// sovereign-cli-daemon — verified via `cargo tree -e features`). Build
/// `sovereign-core` ALONE and the feature is off, the map sorts
/// alphabetically, and the mask would demand `description, id, inputs, kind, …`
/// instead. So the insertion order below is a contract with the prompt's
/// worked example — `id, description, kind, <kind-specific>, inputs` — and it
/// holds only while that feature does. Reorder one without the other, or lose
/// the feature, and the model spends every step fighting a mask it cannot win:
/// no error, no refusal, just worse plans. The order is pinned by the
/// sovereign-core tests.
fn step_branch(
    kind: &str,
    extra: Vec<(&str, serde_json::Value)>,
    extra_required: &[&str],
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert("id".to_string(), serde_json::json!({"type": "integer"}));
    properties.insert(
        "description".to_string(),
        serde_json::json!({"type": "string", "minLength": 1}),
    );
    properties.insert(
        "kind".to_string(),
        serde_json::json!({"type": "string", "const": kind}),
    );
    let mut required = vec![
        "id".to_string(),
        "description".to_string(),
        "kind".to_string(),
    ];
    for (name, spec) in extra {
        properties.insert(name.to_string(), spec);
    }
    required.extend(extra_required.iter().map(|s| (*s).to_string()));

    // `inputs` last, matching the prompt example. Required, so the model always
    // states its dependencies — `[]` is a claim of independence, an omitted key
    // is silence the executor would have read as the same thing.
    properties.insert(
        "inputs".to_string(),
        serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["step_id", "key"],
                "properties": {
                    "step_id": {"type": "integer"},
                    "key": {"type": "string", "minLength": 1}
                }
            }
        }),
    );
    required.push("inputs".to_string());

    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

/// The closed vocabulary a `tool_id` may take: the same `t.id` field the
/// prompt builder renders as the quoted ID, so the ids the model is shown and
/// the ids the grammar admits cannot diverge (ARCH §10.6).
pub fn tool_id_vocabulary(available_tools: &[ToolDescriptor]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::with_capacity(available_tools.len());
    for t in available_tools {
        if !t.id.is_empty() && !ids.iter().any(|seen| seen == &t.id) {
            ids.push(t.id.clone());
        }
    }
    ids
}

/// The JSON Schema the planner decodes under.
///
/// Two things it makes structurally impossible, both of which
/// `parse_plan_json` used to paper over with a default (ARCH §7.6 — encode the
/// invariant, don't ask the model to honour it):
///
/// - A step with no `kind`, or a kind outside the planner's accepted set. The
///   parser defaulted these to `reason`, which silently turned a malformed tool
///   step into a no-op answer.
/// - A `tool` step naming a tool that does not exist. `tool_id` is an `enum`
///   over the ids actually passed in, so the model cannot sample a fabricated
///   one — the same closed-vocabulary argument the prompt makes for tool
///   params.
///
/// - A `tool` step whose ARGUMENTS are outside what the named tool accepts.
///   There is one branch PER TOOL, keyed by a `const` `tool_id`, and that
///   branch's `params` IS the tool's own `parameters` schema — so a declared
///   `enum` (the `sec_facts` concept vocabulary, say) is masked at logit level
///   rather than merely rendered into the prompt. This is the surface the
///   `sec-facts-concept-enum` work was fighting by hand.
///
/// # Errors
///
/// Refuses when a tool's `parameters` is not a typed object. That is the one
/// shape that would break QUIETLY: it compiles fine and masks nothing, so the
/// plan looks constrained while the tool's arguments are free — the §18.3 shape
/// exactly. A schema that fails to compile is already loud (F1 turns it into a
/// 503 naming the error), so the two failure modes are covered between here
/// and there and neither degrades silently. There is deliberately NO fallback
/// to an open `params`: an unmaskable tool must break the build or the request,
/// never the guarantee.
///
/// With no tools available the `tool`, `reason_with_tools` and `delegate`
/// branches are omitted entirely rather than carrying an empty `enum`: a plan
/// cannot call a tool that isn't there, and an `"enum": []` is a vocabulary
/// with no legal member.
pub fn plan_schema(available_tools: &[ToolDescriptor]) -> Result<serde_json::Value> {
    let tool_ids = tool_id_vocabulary(available_tools);
    let mut branches = vec![step_branch(
        "reason",
        vec![
            (
                "prompt",
                serde_json::json!({"type": "string", "minLength": 1}),
            ),
            (
                "speed",
                serde_json::json!({"type": "string", "enum": ["fast", "slow"]}),
            ),
        ],
        &["prompt", "speed"],
    )];

    if !tool_ids.is_empty() {
        let tool_id_enum = serde_json::json!({"type": "string", "enum": tool_ids});
        // One branch per tool: `tool_id` pinned to a `const` and `params`
        // bound to that tool's declared schema. The `const` is also what
        // proves the tool branches disjoint from each other (invariant
        // 0479b961) — an `enum` over all ids with a shared `params` could not
        // carry per-tool arguments at all.
        for t in available_tools {
            if t.id.is_empty() {
                continue;
            }
            branches.push(step_branch(
                "tool",
                vec![
                    (
                        "tool_id",
                        serde_json::json!({"type": "string", "const": t.id}),
                    ),
                    ("params", tool_params_schema(t)?),
                ],
                &["tool_id", "params"],
            ));
        }
        branches.push(step_branch(
            "reason_with_tools",
            vec![
                (
                    "prompt",
                    serde_json::json!({"type": "string", "minLength": 1}),
                ),
                (
                    "speed",
                    serde_json::json!({"type": "string", "enum": ["fast", "slow"]}),
                ),
                (
                    "tools",
                    serde_json::json!({"type": "array", "minItems": 1, "items": tool_id_enum}),
                ),
                ("max_iterations", iteration_count()),
            ],
            &["prompt", "speed", "tools", "max_iterations"],
        ));
        branches.push(step_branch(
            "delegate",
            vec![
                (
                    "goal",
                    serde_json::json!({"type": "string", "minLength": 1}),
                ),
                (
                    "tools",
                    serde_json::json!({"type": "array", "minItems": 1, "items": tool_id_enum}),
                ),
                ("return_schema", open_object()),
                ("max_iterations", iteration_count()),
            ],
            &["goal", "tools", "return_schema", "max_iterations"],
        ));
    }

    // `request` carries exactly the four fields `InformationRequest`
    // deserialises without a serde default, plus optional hints. The executor
    // stamps task_id/step_id/kind/task_title afterwards, so requiring these
    // four means `serde_json::from_value` succeeds and the parser's
    // `unwrap_or` reconstruction never runs.
    branches.push(step_branch(
        "await_user_info",
        vec![(
            "request",
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["current_understanding", "gap", "relevance", "satisfying_source"],
                "properties": {
                    "current_understanding": {"type": "string"},
                    "gap": {"type": "string", "minLength": 1},
                    "relevance": {"type": "string"},
                    "satisfying_source": {"type": "string"},
                    "search_hints": {"type": "array", "items": {"type": "string"}}
                }
            }),
        )],
        &["request"],
    ));

    Ok(serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["goal", "steps", "edges"],
        "properties": {
            "goal": {"type": "string", "minLength": 1},
            "steps": {"type": "array", "minItems": 1, "items": {"oneOf": branches}},
            "edges": {
                "type": "array",
                "items": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 2,
                    "items": {"type": "integer"}
                }
            }
        }
    }))
}
