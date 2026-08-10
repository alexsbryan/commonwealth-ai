// SPDX-License-Identifier: AGPL-3.0-or-later
//! JSON Schema for the workflow TOML data model.
//!
//! Returned by `WorkflowWriteStructuredTool::descriptor()` as the
//! `parameters.workflow` schema, so the daemon's LLGuidance sampler can
//! grammar-constrain the model's emitted tool-call arguments to a valid workflow.
//! Two layers of correctness, exactly as for recipes:
//!
//! 1. **Generation-time** — the daemon constrains the model to emit a JSON object
//!    matching this schema. The `source.type` discriminator and the `<kind>:`
//!    prefix of every step's `uses` are grammar-validated at sampling time.
//! 2. **Validation-time** — `WorkflowWriteStructuredTool` re-runs the workflow
//!    validator (`Workflow::parse` + `summarize_capabilities`) on the written
//!    TOML, so anything the schema didn't pin (a dangling `{ref}`, an `mcp:` tool
//!    that isn't connected, a `recipe:<id>` not in the registry) still surfaces.
//!
//! ## The closed sets are DERIVED — they cannot drift
//!
//! Unlike the recipe schema (whose 22-extractor catalog is extracted from
//! `recipe.rs` by a `build.rs`), the workflow's closed sets are small and stable
//! and their wire form is a custom `<kind>:<rest>` *string* parsed by hand in
//! [`StepKind::parse`] — not a serde tagged enum a `build.rs` could read. So the
//! single source of truth lives *as data* beside the parser:
//! [`StepKind::WIRE_KINDS`] (the `uses` taxonomy + per-kind regex) and
//! [`StepKind::MODEL_LATENCIES`] (the `model:` vocabulary). This module reads them;
//! `kind.rs`'s tests pin that they stay exhaustive over the enum. Add a step kind
//! and it appears here on the next build — no hand-sync (§2.1).
//!
//! What stays hand-authored here is the *shape*: the top-level
//! `[workflow]`/`[source]`/`[[step]]` structure and the per-`StepSpec`-field
//! property hints. Those fields are a small, stable struct
//! (`sovereign_workflow::StepSpec`); the `source_round_trips` test pins the three
//! `[source]` arms against the real `Source` deserializer so a rename can't slip.

use serde_json::{json, Value};
use sovereign_workflow::StepKind;

/// Top-level JSON Schema for a workflow document. Mirrors the on-disk
/// `~/.svrnmesh/workflows/<id>.toml` shape (`[workflow]` + optional `[source]` +
/// one-or-more `[[step]]`) as a JSON object the tool mechanically serializes to
/// TOML. There is no top-level `[params]` table — run-time parameters arrive via
/// `--param`/`--folder` and are read in templates as `{param.*}`.
pub fn workflow_json_schema() -> Value {
    json!({
        "type": "object",
        "title": "Workflow",
        "description":
            "A Sovereign workflow: a small pipeline of steps over a source of items, \
             run on the user's machine. Serializes to \
             ~/.svrnmesh/workflows/<id>.toml. The authoritative vocabulary (every \
             step kind, source type, and `{ref}` form, with a worked example) is \
             `docs/WRITE_A_WORKFLOW.md`.",
        "required": ["workflow", "step"],
        "additionalProperties": false,
        "properties": {
            "workflow": meta_schema(),
            "source":   source_schema(),
            "step":     steps_schema(),
        }
    })
}

fn meta_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name"],
        "additionalProperties": false,
        "properties": {
            "name": {
                "type": "string",
                "description":
                    "A short name for the workflow (the `[workflow] name`). The file \
                     is named by the `path` argument, not this."
            }
        }
    })
}

/// `[source]` — where the per-item driver gets its items. Discriminated by `type`.
/// The three arms mirror `sovereign_workflow::Source` (folder | list | inline);
/// `source_round_trips` pins them against the real deserializer.
fn source_schema() -> Value {
    json!({
        "description":
            "Optional. Where the workflow gets the items it runs over (it runs once \
             per item). Omit for a one-shot workflow with no iteration. \
             Discriminated by `type`.",
        "oneOf": [
            json!({
                "type": "object",
                "required": ["type", "path"],
                "additionalProperties": false,
                "properties": {
                    "type": { "const": "folder" },
                    "path": {
                        "type": "string",
                        "description":
                            "A directory of files. Each file becomes one item, \
                             exposing {item.path}/{item.name}/{item.stem} and — for \
                             small UTF-8 files — {item.text}. Templated, so \
                             `{param.folder}` takes the folder at run time."
                    },
                    "glob": {
                        "type": "string",
                        "description":
                            "Optional. A comma-separated list of `*.<ext>` filters \
                             (e.g. \"*.pdf,*.md,*.txt\"). An unset/empty glob matches \
                             every file in the folder."
                    }
                }
            }),
            json!({
                "type": "object",
                "required": ["type", "path"],
                "additionalProperties": false,
                "properties": {
                    "type": { "const": "list" },
                    "path": {
                        "type": "string",
                        "description":
                            "A text file read one non-empty line per item; each line \
                             is {item} (e.g. a list of URLs feeding a tool:web_fetch \
                             step). Templated."
                    }
                }
            }),
            json!({
                "type": "object",
                "required": ["type", "items"],
                "additionalProperties": false,
                "properties": {
                    "type": { "const": "inline" },
                    "items": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "type": "string" },
                        "description":
                            "Literal items, one run each. A single item makes the \
                             workflow a one-shot."
                    }
                }
            })
        ]
    })
}

fn steps_schema() -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "description":
            "The steps, run in dependency order (a DAG auto-derived from the \
             `{step_id.key}` references between them). Each is one [[step]].",
        "items": step_schema(),
    })
}

/// One `[[step]]` — `sovereign_workflow::StepSpec`. Only `id` + `uses` are
/// required; every other field is an optional knob for a particular step kind.
fn step_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "uses"],
        "additionalProperties": false,
        "properties": {
            "id": {
                "type": "string",
                "description":
                    "Unique step id. Other steps read this step's output as \
                     {<id>.output}; that reference is what orders the DAG."
            },
            "uses": uses_schema(),
            "prompt": {
                "type": "string",
                "description":
                    "For a model: step — the user/content message, templated with \
                     {item.*}/{element.*}/{<step>.output}/{param.*}."
            },
            "system": {
                "type": "string",
                "description": "For a model: step — an inline system prompt."
            },
            "system_file": {
                "type": "string",
                "description":
                    "For a model: step — load the system prompt from a file (its \
                     content used verbatim). Templated path; overrides `system`."
            },
            "input": {
                "type": "string",
                "description":
                    "The step's primary input string (e.g. an embed: step's text, or \
                     a transform: input). Templated."
            },
            "params": {
                "type": "object",
                "additionalProperties": true,
                "description":
                    "Arguments for a tool:/mcp:/recipe: step, as a table — e.g. \
                     { path = \"{item.path}\" }. Values are templated. For a \
                     recipe: step these are the recipe's install parameters."
            },
            "for_each": {
                "type": "string",
                "description":
                    "Map this step over the (array) output of another step, named by \
                     that step's id. The step runs once per element; inside, read \
                     {element} or {element.<field>}. Its output is the array of \
                     per-element results."
            },
            "on_error": {
                "enum": ["skip", "abort"],
                "description":
                    "For a for_each step: `skip` records the failing element in the \
                     step's failures and continues; `abort` (default) fails on the \
                     first element error."
            },
            "cache": {
                "type": "boolean",
                "description":
                    "Set false to never cache a volatile Read step (e.g. a web fetch \
                     whose target changes). Default: a Read step caches, a Write step \
                     never does."
            },
            "structured_output": {
                "type": "object",
                "additionalProperties": true,
                "description":
                    "For a model: step — a JSON schema the model's output is \
                     constrained to conform to (the general extraction primitive)."
            },
            "grammar": {
                "type": "string",
                "description":
                    "For a model: step — a Lark grammar constraining the output \
                     (lower-level alternative to structured_output). Templated."
            },
            "stamp": {
                "type": "object",
                "additionalProperties": true,
                "description":
                    "A templated object merged into this step's (object) output — \
                     e.g. { chapter_id = \"{element.index}\" } to carry per-element \
                     identity through a for_each map."
            },
            "raw_output": {
                "type": "boolean",
                "description":
                    "For a constrained model: step — keep the grammar but return the \
                     raw model text instead of auto-parsing it to JSON (when a \
                     downstream step does the parsing)."
            },
            "resources": {
                "type": "object",
                "additionalProperties": false,
                "description": "Optional scheduler hints (mostly inert today).",
                "properties": {
                    "latency_class": { "type": "string" },
                    "privacy":       { "type": "string" }
                }
            }
        }
    })
}

/// The `uses` selector, built from [`StepKind::WIRE_KINDS`] so the kind set and the
/// `model:` latency vocabulary are never re-listed here. Each arm carries the
/// kind's anchored regex (grammar-constrains the whole string at generation) plus
/// its one-line summary.
fn uses_schema() -> Value {
    let arms: Vec<Value> = StepKind::WIRE_KINDS
        .iter()
        .map(|w| {
            json!({
                "type": "string",
                "pattern": w.uses_pattern,
                "description": w.summary,
            })
        })
        .collect();
    json!({
        "description":
            "What this step does, as `<kind>:<rest>`. One of: a local-model \
             completion (model:), an embedding (embed:), a built-in tool (tool:), an \
             MCP server tool (mcp:), a deterministic transform (transform:), or a \
             corpus ingest/enrich stage (recipe:).",
        "oneOf": arms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_workflow::{Source, Workflow};

    fn required(schema: &Value) -> Vec<&str> {
        schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect()
    }

    #[test]
    fn root_requires_workflow_and_step() {
        let s = workflow_json_schema();
        assert_eq!(s["type"], "object");
        let req = required(&s);
        assert!(req.contains(&"workflow"), "root must require workflow");
        assert!(req.contains(&"step"), "root must require step");
        // additionalProperties:false keeps the model from inventing a top-level key
        // the deserializer would silently drop (there is no [params] table).
        assert_eq!(s["additionalProperties"], json!(false));
    }

    #[test]
    fn uses_oneof_covers_every_wire_kind() {
        // The whole point of deriving from WIRE_KINDS: every step kind surfaces as a
        // `uses` arm, with its anchored pattern. A new StepKind variant flows here
        // automatically (kind.rs's exhaustiveness test guards the catalog).
        let s = workflow_json_schema();
        let arms = s["properties"]["step"]["items"]["properties"]["uses"]["oneOf"]
            .as_array()
            .expect("uses.oneOf array");
        let patterns: Vec<&str> = arms.iter().filter_map(|a| a["pattern"].as_str()).collect();
        assert_eq!(
            patterns.len(),
            StepKind::WIRE_KINDS.len(),
            "one uses arm per WireKind"
        );
        for w in StepKind::WIRE_KINDS {
            assert!(
                patterns.contains(&w.uses_pattern),
                "uses.oneOf is missing the `{}` pattern {}",
                w.prefix,
                w.uses_pattern
            );
        }
    }

    #[test]
    fn step_requires_id_and_uses_only() {
        let s = workflow_json_schema();
        let step = &s["properties"]["step"]["items"];
        let req = required(step);
        assert_eq!(req, vec!["id", "uses"], "a step requires exactly id + uses");
        // The optional knobs are present so the grammar permits them.
        for k in [
            "prompt",
            "params",
            "for_each",
            "on_error",
            "structured_output",
        ] {
            assert!(
                step["properties"].get(k).is_some(),
                "step schema should declare optional `{k}`"
            );
        }
    }

    #[test]
    fn source_arms_round_trip_through_the_real_deserializer() {
        // Pin the three [source] arms against `Source` itself: a minimal document of
        // each schema-declared type must deserialize. If someone renames a Source
        // variant, this trips here rather than letting the schema guide the model to
        // an unparseable source.
        let s = workflow_json_schema();
        let arms = s["properties"]["source"]["oneOf"].as_array().unwrap();
        let types: Vec<&str> = arms
            .iter()
            .filter_map(|a| a["properties"]["type"]["const"].as_str())
            .collect();
        assert_eq!(types, vec!["folder", "list", "inline"]);

        // Each schema-declared source type must parse as a real `Source` through
        // the workflow parser (so no extra `toml` dep). The exhaustive match also
        // anchors the schema to `Source` at compile time: a renamed/added variant
        // breaks this arm until the schema is updated to match.
        let cases = [
            ("folder", "type = \"folder\"\npath = \"/tmp/x\"\n"),
            ("list", "type = \"list\"\npath = \"/tmp/urls.txt\"\n"),
            ("inline", "type = \"inline\"\nitems = [\"one\"]\n"),
        ];
        for (name, src) in cases {
            let toml = format!(
                "[workflow]\nname = \"t\"\n[source]\n{src}[[step]]\nid = \"a\"\nuses = \"transform:json\"\n"
            );
            let wf = Workflow::parse(&toml)
                .unwrap_or_else(|e| panic!("source `{name}` must parse: {e}"));
            let got = match wf.source.expect("source present") {
                Source::Folder { .. } => "folder",
                Source::List { .. } => "list",
                Source::Inline { .. } => "inline",
            };
            assert_eq!(got, name, "source `{name}` parsed to the wrong variant");
        }
    }
}
