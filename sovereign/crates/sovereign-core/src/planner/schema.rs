// SPDX-License-Identifier: AGPL-3.0-or-later
//! The JSON Schema the planner DECODES under — the constraint handed to
//! `structured_output` so a plan is masked into shape at logit level
//! rather than asked for in prose and repaired afterwards.
//!
//! NOT `crate::plan_grammar`, despite the adjacent names. That module
//! owns the `{N.output}` placeholder-reference syntax shared with the
//! executor's resolver, and has nothing to do with constrained
//! decoding. This one owns the step-kind branches, the closed `tool_id`
//! vocabulary, and the field ORDER llguidance will force (see
//! `step_branch` — that order is a contract with the worked example in
//! `super::PLAN_SYSTEM_PROMPT`).

use crate::error::{Error, Result};
use crate::types::*;

/// The step kinds the planner may emit. One source for three things:
/// the `oneOf` branches `plan_schema` builds, the arms
/// `parse_plan_json` accepts from a model, and the "STEP KINDS:" block
/// of [`PLAN_SYSTEM_PROMPT`] — `prompt_documents_exactly_the_schema_kinds`
/// fails if the prompt and this list drift apart (ARCH §10.6).
///
/// `branch` is deliberately absent: `parse_plan_json` still constructs
/// [`StepKind::Branch`] for hand-written and template plans, but the
/// prompt has never documented it, so a model shown this schema has no
/// contract to emit it against.
pub(crate) const PLANNABLE_KINDS: [&str; 5] = [
    "reason",
    "tool",
    "reason_with_tools",
    "await_user_info",
    "delegate",
];

/// Kinds `parse_plan_json` accepts but never offers to a model. Kept
/// separate from [`PLANNABLE_KINDS`] rather than merged into it so the
/// two questions stay one decider each: what a model may emit, and
/// what the parser will construct. `every_parseable_kind_parses`
/// pins the union against the match arms.
pub(crate) const HAND_WRITTEN_KINDS: [&str; 1] = ["branch"];

/// The full set `parse_plan_json` will construct, for error messages
/// that name what the caller could have said.
pub(super) fn parseable_kinds() -> String {
    PLANNABLE_KINDS
        .iter()
        .chain(HAND_WRITTEN_KINDS.iter())
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

/// The closed vocabulary a `tool_id` may take: the same `t.id` field
/// `build_plan_prompt` renders as the quoted ID, so the ids the model
/// is shown and the ids the grammar admits cannot diverge (ARCH §10.6).
pub(super) fn tool_id_vocabulary(available_tools: &[ToolDescriptor]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::with_capacity(available_tools.len());
    for t in available_tools {
        if !t.id.is_empty() && !ids.iter().any(|seen| seen == &t.id) {
            ids.push(t.id.clone());
        }
    }
    ids
}

/// Bounds on a step's declared `max_iterations`.
///
/// The floor is 1 because the executor already does
/// `max_iterations.max(1)` (`executor.rs:528`) — a declared `0` is
/// silently coerced to one iteration today, and a value the engine
/// rewrites is a value the planner should not be able to state.
///
/// The ceiling exists because the field was previously an unbounded
/// integer: one `delegate` step could declare 1000 iterations and the
/// executor would run them, since it caps nothing (it only ADDS, +2 for
/// `StepDifficulty::Hard`, `executor.rs:1019`). An unbounded loop
/// counter chosen by a model is the kind of thing that should be
/// unrepresentable rather than merely unlikely.
///
/// 12 is HEADROOM over the 6 that [`PLAN_SYSTEM_PROMPT`] documents as
/// typical — two times the documented figure, leaving 14 as the worst
/// case after the Hard bump. It is NOT a measured optimum and no run
/// was performed to choose it; if a real workload needs more, raise it
/// deliberately rather than treating this as a tuned value.
const MIN_PLANNED_ITERATIONS: u64 = 1;
const MAX_PLANNED_ITERATIONS: u64 = 12;

/// The `max_iterations` sub-schema shared by `reason_with_tools` and
/// `delegate` — one decider for the bound, so the two step kinds
/// cannot drift apart (ARCH §10.6).
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
/// `default_additional_properties_false` over every typed-object node
/// that doesn't declare one, so an unannotated `{"type": "object"}`
/// would arrive at llguidance sealed to the empty object — a block the
/// model could only ever fill with `{}`.
///
/// Used for `delegate`'s `return_schema`, which is a shape the PLANNER
/// invents for a sub-agent rather than one any registered tool
/// declares. Tool `params` are NOT open — see [`tool_params_schema`].
fn open_object() -> serde_json::Value {
    serde_json::json!({ "type": "object", "additionalProperties": true })
}

/// The `params` sub-schema for one tool: the tool's own declared
/// `parameters`, verbatim.
///
/// Verbatim is the point. Copying or summarising it would be a second
/// decider for what the tool accepts, and the two would drift — the
/// tool would start rejecting arguments the grammar still allowed.
/// `format_param_hint` renders the SAME schema into the prompt, so
/// what the model is shown, what it can sample, and what the tool will
/// accept are one thing (ARCH §10.6).
///
/// The typed-object check is the guard against a quiet break. A
/// `parameters` that is `{}`, or a bare `{"properties": …}` with no
/// `"type"`, compiles happily and constrains NOTHING: the plan would
/// carry a schema, return 200, and leave that tool's arguments as free
/// as they were before F3. Refusing names the tool instead.
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
/// kind-specific ones. The `kind` `const` is the discriminator that
/// proves the branches disjoint — without it llguidance refuses the
/// whole `oneOf` ("oneOf constraints are not supported"), and with F1
/// that refusal is now an error rather than silent free-form sampling.
/// See the pinned invariant in `tests/llguidance_parity.rs`.
///
/// MEASURED 2026-08-19, and load-bearing in a non-obvious way.
/// llguidance emits object keys in the order it ITERATES `properties`,
/// and masks any other order — the `required` array order does not
/// drive it (probed with three permutations; the emitted first key
/// tracked `properties` every time).
///
/// `serde_json::Map` iterates in INSERTION order only because
/// `serde_json/preserve_order` is on, which every binary that runs the
/// planner resolves transitively (sovereign-desktop, sovereign-server,
/// sovereign-cli-daemon — verified via `cargo tree -e features`).
/// Build `sovereign-core` ALONE and the feature is off, the map sorts
/// alphabetically, and the mask would demand `description, id, inputs,
/// kind, …` instead. So the insertion order below is a contract with
/// [`PLAN_SYSTEM_PROMPT`]'s worked example — `id, description, kind,
/// <kind-specific>, inputs` — and it holds only while that feature
/// does. Reorder one without the other, or lose the feature, and the
/// model spends every step fighting a mask it cannot win: no error, no
/// refusal, just worse plans.
///
/// `plan_schema_key_order_matches_the_prompt_example` pins the
/// alignment and `plan_schema_key_order_depends_on_preserve_order`
/// pins the feature it rests on.
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

    // `inputs` last, matching the prompt example. Required, so the
    // model always states its dependencies — `[]` is a claim of
    // independence, an omitted key is silence the executor would have
    // read as the same thing.
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

/// The JSON Schema the planner decodes under.
///
/// Two things it makes structurally impossible, both of which
/// `parse_plan_json` used to paper over with a default (ARCH §7.6 —
/// encode the invariant, don't ask the model to honour it):
///
/// - A step with no `kind`, or a kind outside [`PLANNABLE_KINDS`]. The
///   parser defaulted these to `reason`, which silently turned a
///   malformed tool step into a no-op answer.
/// - A `tool` step naming a tool that does not exist. `tool_id` is an
///   `enum` over the ids actually passed in, so the model cannot sample
///   a fabricated one — the same closed-vocabulary argument
///   `format_param_hint` makes for tool params.
///
/// - A `tool` step whose ARGUMENTS are outside what the named tool
///   accepts. There is one branch PER TOOL, keyed by a `const`
///   `tool_id`, and that branch's `params` IS the tool's own
///   `parameters` schema — so a declared `enum` (the `sec_facts`
///   concept vocabulary, say) is masked at logit level rather than
///   merely rendered into the prompt by `format_param_hint`. This is
///   the surface the `sec-facts-concept-enum` work was fighting by
///   hand.
///
/// # Errors
///
/// Refuses when a tool's `parameters` is not a typed object. That is
/// the one shape that would break QUIETLY: it compiles fine and masks
/// nothing, so the plan looks constrained while the tool's arguments
/// are free — the §18.3 shape exactly. A schema that fails to compile
/// is already loud (F1 turns it into a 503 naming the error), so the
/// two failure modes are covered between here and there and neither
/// degrades silently. There is deliberately NO fallback to an open
/// `params`: an unmaskable tool must break the build or the request,
/// never the guarantee.
///
/// With no tools available the `tool`, `reason_with_tools` and
/// `delegate` branches are omitted entirely rather than carrying an
/// empty `enum`: a plan cannot call a tool that isn't there, and an
/// `"enum": []` is a vocabulary with no legal member.
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
        // One branch per tool: `tool_id` pinned to a `const` and
        // `params` bound to that tool's declared schema. The `const`
        // is also what proves the tool branches disjoint from each
        // other (invariant 0479b961) — an `enum` over all ids with a
        // shared `params` could not carry per-tool arguments at all.
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
    // deserialises without a serde default, plus optional hints. The
    // executor stamps task_id/step_id/kind/task_title afterwards, so
    // requiring these four means `serde_json::from_value` succeeds and
    // the parser's `unwrap_or` reconstruction never runs.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::PLAN_SYSTEM_PROMPT;

    /// A descriptor carrying a real `parameters` schema — the thing
    /// `plan_schema` now embeds, so fixtures must be shaped like the
    /// tools rather than like bare ids.
    fn tool(id: &str, parameters: serde_json::Value) -> ToolDescriptor {
        ToolDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            description: id.to_string(),
            parameters,
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: None,
        }
    }

    /// The `search`-shaped tool most fixtures want.
    fn query_tool(id: &str) -> ToolDescriptor {
        tool(
            id,
            serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": { "query": {"type": "string", "minLength": 1} }
            }),
        )
    }

    // ─── F3: the plan schema and the refusals it backs ─────────

    /// Kind names the prompt's "STEP KINDS:" block documents, scoped to
    /// that block so the "RULES:" bullets below it don't leak in.
    fn kinds_documented_in_prompt() -> Vec<String> {
        let start = PLAN_SYSTEM_PROMPT
            .find("STEP KINDS:")
            .expect("prompt must have a STEP KINDS block");
        let block = &PLAN_SYSTEM_PROMPT[start..];
        let block = &block[..block.find("\nRULES:").unwrap_or(block.len())];
        block
            .lines()
            .filter_map(|l| l.strip_prefix("- \""))
            .filter_map(|l| l.split_once("\":"))
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// The `const` on each `oneOf` branch of a built schema.
    fn kinds_in_schema(schema: &serde_json::Value) -> Vec<String> {
        schema["properties"]["steps"]["items"]["oneOf"]
            .as_array()
            .expect("steps.items.oneOf")
            .iter()
            .map(|b| {
                b["properties"]["kind"]["const"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .fold(Vec::new(), |mut acc, k| {
                // Deduped: there is one branch PER TOOL, so "tool"
                // appears once per registered tool. This helper answers
                // "which kinds exist", not "how many branches".
                if !acc.contains(&k) {
                    acc.push(k);
                }
                acc
            })
    }

    #[test]
    fn prompt_documents_exactly_the_schema_kinds() {
        // One decider (ARCH §10.6). The prompt tells the model what a
        // step may be; the schema decides what it may sample. A kind
        // in one and not the other is either an instruction the
        // grammar forbids or a capability nothing explains — both
        // land as the model failing at something it was set up to fail.
        let mut documented = kinds_documented_in_prompt();
        let mut declared: Vec<String> = PLANNABLE_KINDS.iter().map(|s| s.to_string()).collect();
        documented.sort();
        declared.sort();
        assert_eq!(
            documented, declared,
            "PLAN_SYSTEM_PROMPT's STEP KINDS block and PLANNABLE_KINDS must name \
             the same set"
        );

        let mut in_schema = kinds_in_schema(&plan_schema(&[query_tool("search")]).unwrap());
        in_schema.sort();
        assert_eq!(
            in_schema, declared,
            "plan_schema must build one branch per PLANNABLE_KINDS entry"
        );
    }

    #[test]
    fn plan_schema_tool_branches_vanish_without_tools() {
        let with = kinds_in_schema(&plan_schema(&[query_tool("search")]).unwrap());
        assert!(with.contains(&"tool".to_string()));
        assert!(with.contains(&"delegate".to_string()));

        // Not "an enum with no members" — the branches are gone. An
        // `"enum": []` is a vocabulary with no legal value, which is
        // how a schema stops compiling.
        let without = kinds_in_schema(&plan_schema(&[]).unwrap());
        assert_eq!(without, vec!["reason", "await_user_info"]);
        let json = serde_json::to_string(&plan_schema(&[]).unwrap()).unwrap();
        assert!(
            !json.contains("\"enum\":[]"),
            "an empty enum must never reach the engine: {json}"
        );
    }
    #[test]
    fn each_tool_gets_its_own_params_schema() {
        // The F3 requirement: a declared enum is MASKED, not merely
        // rendered into the prompt. Two tools, two different argument
        // shapes, one branch each.
        let schema = plan_schema(&[
            query_tool("search"),
            tool(
                "sec_facts",
                serde_json::json!({
                    "type": "object",
                    "required": ["concept"],
                    "properties": {
                        "concept": {"type": "string", "enum": ["revenue", "gross_profit"]}
                    }
                }),
            ),
        ])
        .unwrap();

        let branches = schema["properties"]["steps"]["items"]["oneOf"]
            .as_array()
            .unwrap();
        let tool_branch = |id: &str| {
            branches
                .iter()
                .find(|b| b["properties"]["tool_id"]["const"] == id)
                .unwrap_or_else(|| panic!("no branch for {id}"))
                .clone()
        };
        assert_eq!(
            tool_branch("sec_facts")["properties"]["params"]["properties"]["concept"]["enum"],
            serde_json::json!(["revenue", "gross_profit"]),
            "the tool's declared vocabulary must reach the grammar verbatim"
        );
        assert_eq!(
            tool_branch("search")["properties"]["params"]["properties"]["query"]["type"],
            "string",
            "each tool carries its OWN arguments, not a shared open object"
        );
    }

    #[test]
    fn a_tool_whose_params_cannot_be_masked_is_refused_not_widened() {
        // The quiet-break shape: `parameters` with no `"type"` compiles
        // fine and constrains nothing. Before refusing, this would have
        // produced a plan that LOOKED grammar-constrained while that
        // tool's arguments stayed free.
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"properties": {"query": {"type": "string"}}}),
            serde_json::json!({"type": "string"}),
        ] {
            let err = plan_schema(&[tool("loose", bad)]).unwrap_err().to_string();
            assert!(
                err.contains("loose"),
                "the refusal must name the tool: {err}"
            );
            assert!(
                err.contains("not a typed object"),
                "and say what is wrong: {err}"
            );
        }

        // Twin: the same tool with a typed object is accepted, so the
        // refusal is about the schema shape and nothing else.
        assert!(plan_schema(&[query_tool("loose")]).is_ok());
    }

    #[test]
    fn a_refused_tool_does_not_silently_drop_out_of_the_plan() {
        // The tempting "fix" is to skip the unmaskable tool and carry
        // on. That is the quiet break wearing a different hat: planning
        // would succeed while a registered capability had vanished.
        let tools = vec![query_tool("search"), tool("loose", serde_json::json!({}))];
        assert!(
            plan_schema(&tools).is_err(),
            "one unmaskable tool must fail the whole schema, not be omitted from it"
        );
    }
}
