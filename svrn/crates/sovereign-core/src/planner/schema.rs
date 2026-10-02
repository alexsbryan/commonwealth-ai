// SPDX-License-Identifier: AGPL-3.0-or-later
//! The planner's step-kind vocabulary — and the historical path of the JSON
//! Schema the planner DECODES under.
//!
//! The schema builder itself (`plan_schema`, the `oneOf` branches, the closed
//! `tool_id` vocabulary, the field ORDER llguidance forces) moved to the
//! `sovereign-contracts` leaf (2026-09-21) so the inference parity fixture can
//! compile it against the real engine without linking the runtime hub. It is
//! re-exported here at its historical path, so `sovereign_core::planner::
//! plan_schema` is unchanged.
//!
//! What stays is the vocabulary the parser and the prompt own:
//! [`PLANNABLE_KINDS`] (what a model may emit), [`HAND_WRITTEN_KINDS`] (what
//! the parser will still construct), and `parseable_kinds`. The test that pins
//! the prompt's STEP KINDS block against [`PLANNABLE_KINDS`] lives here too,
//! because it needs `PLAN_SYSTEM_PROMPT`.

pub use sovereign_contracts::planner_schema::plan_schema;
/// The closed `tool_id` vocabulary — one decider, shared with the moved
/// schema builder. Re-exported `pub(crate)` for the prompt-alignment test.
pub(crate) use sovereign_contracts::planner_schema::tool_id_vocabulary;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::PLAN_SYSTEM_PROMPT;
    use crate::types::*;

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

    /// covers: RT-32
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
