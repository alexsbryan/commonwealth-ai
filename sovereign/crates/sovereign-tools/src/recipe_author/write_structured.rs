// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RecipeWriteStructuredTool` — write a recipe from a structured
//! JSON object instead of a raw TOML string.
//!
//! Why this exists: when the agent emits raw TOML (via the original
//! `RecipeWriteTool`), every per-character mistake is fatal —
//! invalid `null`, missing `[acquire]` parent table, escape-quote
//! errors inside JSON-string-of-TOML. This tool inverts the
//! contract: the agent emits a structured JSON object whose schema
//! lives in `parameters.recipe`, the daemon's grammar-constrained
//! sampler keeps the model's tool-call arguments in the schema, the
//! tool serialises JSON → TOML mechanically, and a final
//! `RecipeValidate` against the on-disk file catches anything the
//! schema didn't cover.
//!
//! Failure mode coverage:
//!
//! - **Malformed TOML syntax** — impossible: the tool generates the
//!   TOML, not the model.
//! - **Invented top-level keys** — caught by `additionalProperties:
//!   false` on the root in `recipe_json_schema`.
//! - **Wrong discriminator values** — caught by the enum
//!   constraints on `acquire.type` / `extract.type` / `chunk.type` /
//!   `enrichment.type` / `enrichment.patterns[].type`.
//! - **Missing required sections** — caught by `required` lists.
//! - **Per-variant errors** (wrong field name inside an acquirer
//!   variant, missing url for bulk_download, etc.) — caught by the
//!   on-disk `RecipeValidate` after the file is written.

use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::json_to_toml::{json_to_toml, toml_value_to_string};
use super::recipe_schema::recipe_json_schema;
use super::resolve_recipe_path;

#[derive(Default)]
pub struct RecipeWriteStructuredTool {
    /// Optional override for the recipes-dir root. Tests inject a
    /// per-test tempdir so the tool runs without mutating
    /// process-global `HOME`.
    recipes_dir: Option<PathBuf>,
}

impl RecipeWriteStructuredTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_recipes_dir(dir: PathBuf) -> Self {
        Self {
            recipes_dir: Some(dir),
        }
    }
}

#[async_trait]
impl Tool for RecipeWriteStructuredTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recipe_write_structured".into(),
            name: "RecipeWriteStructured".into(),
            description: "Write a recipe from a structured JSON object. The \
                 `recipe` argument is a recipe-shaped object — not raw \
                 TOML. The tool serialises it to TOML and writes \
                 atomically to ~/.sovereign/recipes/<path>/recipe.toml. \
                 \n\nALWAYS prefer this over recipe_write for new \
                 drafts: the JSON Schema for `recipe` (declared in this \
                 tool's parameters) lets the daemon grammar-constrain \
                 your output to the recipe shape, so you cannot emit \
                 invalid keys, malformed TOML, or `null` values that \
                 TOML doesn't support. Discriminators (acquire.type, \
                 extract.type, chunk.type, enrichment.type, \
                 enrichment.patterns[].type) are enum-validated at \
                 generation time. \
                 \n\nReturns the TOML the tool wrote plus the \
                 validator's report so you can see at a glance \
                 whether the recipe is ready to test."
                .into(),
            parameters: json!({
                "type": "object",
                "required": ["path", "recipe"],
                "additionalProperties": false,
                "properties": {
                    "path": {
                        "type": "string",
                        "description":
                            "Recipe id (writes to <id>/recipe.toml) or \
                             relative path under ~/.sovereign/recipes/."
                    },
                    "recipe": recipe_json_schema(),
                }
            }),
            examples: vec![ToolExample {
                situation: "Draft a fresh recipe from scratch. The agent emits \
                     a structured JSON object; the tool produces clean \
                     TOML on disk."
                    .into(),
                call: json!({
                    "path": "demo-investigation",
                    "recipe": {
                        "corpus": {
                            "id": "demo-investigation",
                            "name": "Demo investigation"
                        },
                        "acquire": {
                            "type": "bulk_download",
                            "url": "https://example.com/data.zip"
                        },
                        "extract": { "type": "html" },
                        "chunk":   { "type": "paragraph", "max_chars": 2048 }
                    }
                }),
            }],
            effect: Effect::ReadWrite,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path":          { "type": "string" },
                    "bytes_written": { "type": "integer" },
                    "toml_preview":  { "type": "string" },
                    "validation": {
                        "type": "object",
                        "properties": {
                            "passed":   { "type": "boolean" },
                            "errors":   { "type": "array" },
                            "warnings": { "type": "array" }
                        }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::InvalidInput("RecipeWriteStructuredTool requires `path`".into())
        })?;
        // Accept either of two arg shapes:
        //
        //   {"path": "...", "recipe": {<recipe>}}     — canonical
        //   {"path": "...", "corpus": {...}, ...}     — flattened
        //
        // The canonical shape matches the JSON Schema in the tool's
        // descriptor, but real models (~35B Hermes-style ones in
        // particular) frequently flatten the recipe fields into the
        // args root instead of nesting under `recipe`. Rather than
        // burning agent iterations on a "wrap your recipe" loop,
        // we recover the recipe from whichever shape we received.
        // If neither shape produces a recipe-shaped object, the
        // downstream `json_to_toml` + on-disk validator surface the
        // real problem.
        let recipe_owned: serde_json::Value;
        let recipe: &serde_json::Value = match params.get("recipe") {
            Some(v) if v.is_object() => v,
            _ => {
                // Flatten path: take args minus the `path` key as the
                // recipe. Skip `path` and any other non-recipe keys
                // we recognize (currently just `path`).
                let mut map = serde_json::Map::new();
                if let Some(obj) = params.as_object() {
                    for (k, v) in obj {
                        if k == "path" {
                            continue;
                        }
                        map.insert(k.clone(), v.clone());
                    }
                }
                if map.is_empty() {
                    return Err(Error::InvalidInput(
                        "RecipeWriteStructuredTool requires either a \
                         `recipe` object argument or recipe fields \
                         (corpus, acquire, extract, chunk, …) at the \
                         args root."
                            .into(),
                    ));
                }
                recipe_owned = serde_json::Value::Object(map);
                &recipe_owned
            }
        };
        if !recipe.is_object() {
            return Err(Error::InvalidInput("`recipe` must be a JSON object".into()));
        }

        // 1. JSON → TOML. First repair the two structured-output
        //    artifacts the 35B reliably emits (stray `key": ` escapes,
        //    null-valued optional keys) so a well-formed recipe survives
        //    instead of forcing the agent into a raw-recipe_write
        //    fallback; the on-disk validator then catches anything real.
        let sanitized = super::json_to_toml::sanitize_for_toml(recipe);
        let toml_value = json_to_toml(&sanitized)
            .map_err(|e| Error::InvalidInput(format!("recipe → TOML conversion failed: {e}")))?;
        let toml_text = toml_value_to_string(&toml_value)
            .map_err(|e| Error::InvalidInput(format!("TOML serialization failed: {e}")))?;

        // 2. Atomic write to <recipes>/<path>/recipe.toml.
        let resolved = resolve_recipe_path(raw_path, self.recipes_dir.as_ref())?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::InvalidInput(format!("failed to create parent {}: {e}", parent.display()))
            })?;
        }
        let part = resolved.with_extension("toml.part");
        fs::write(&part, &toml_text)
            .map_err(|e| Error::InvalidInput(format!("failed to write {}: {e}", part.display())))?;
        fs::rename(&part, &resolved).map_err(|e| {
            Error::InvalidInput(format!(
                "failed to commit {} → {}: {e}",
                part.display(),
                resolved.display(),
            ))
        })?;

        // 3. Run the on-disk validator. If validation fails the
        //    file is still on disk (the agent gets to inspect /
        //    iterate); the response just reports the failure so
        //    the agent can fix it on the next call.
        let validation_report = run_disk_validation(&resolved).await;

        // Push the agent through the fix-and-rewrite cycle in the
        // SAME turn instead of yielding to the partner with a
        // narrated plan. Trial after trial showed the model would
        // diagnose the validation error correctly, write "let me
        // fix this" — and then end the turn. This nudge mirrors
        // what `recipe_test` does on its failure path.
        let mut payload = json!({
            "path": resolved.display().to_string(),
            "bytes_written": toml_text.len(),
            "toml_preview": preview(&toml_text, 1200),
            "validation": validation_report,
        });
        let validation_failed = validation_report
            .get("passed")
            .and_then(|v| v.as_bool())
            .map(|b| !b)
            .unwrap_or(false);
        if validation_failed {
            payload["nudge"] = json!(
                "Recipe is on disk but validation FAILED. Read \
                 `validation.errors`, fix the recipe, and call \
                 `recipe_write_structured` AGAIN in this same turn. \
                 Do NOT yield to the partner with a narrated plan — \
                 act on the fix now. The cycle is: read errors → \
                 rewrite → re-validate, all in one turn."
            );
        }
        Ok(StepOutput::Json(payload))
    }
}

fn preview(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars - 1).collect();
    format!("{cut}…")
}

/// Run the existing recipe-engine validator against the on-disk
/// file. Validation-only mode (`sample_size == 0`) — no download,
/// no extraction. Returns a `{passed, errors, warnings}` JSON
/// object so the tool's response shape is uniform across success
/// and failure paths.
async fn run_disk_validation(path: &std::path::Path) -> serde_json::Value {
    use corpus_engine::testing::TestOptions;
    use corpus_engine::CorpusEngine;
    let recipes_dir = path
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let indexes_dir = recipes_dir
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| recipes_dir.clone());
    let stub_embed: corpus_engine::EmbedFn = std::sync::Arc::new(|_text| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![]) })
    });
    let engine = CorpusEngine::new(recipes_dir, indexes_dir, stub_embed);
    let opts = TestOptions {
        sample_size: 0,
        offline: true,
        ..Default::default()
    };
    match engine.test_recipe(path, &opts).await {
        Ok(report) => {
            let passed = report.passed();
            let errors: Vec<String> = report.validation.errors.to_vec();
            let warnings: Vec<String> = report.warnings();
            json!({
                "passed": passed,
                "errors": errors,
                "warnings": warnings,
            })
        }
        Err(e) => json!({
            "passed": false,
            "errors": [e.to_string()],
            "warnings": []
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: ConversationId::new(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    fn make_root(home: &std::path::Path) -> PathBuf {
        let recipes = home.join(".sovereign/recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        recipes
    }

    #[tokio::test]
    async fn writes_clean_recipe_from_structured_input() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeWriteStructuredTool::with_recipes_dir(root.clone());
        let out = tool
            .execute(
                &json!({
                    "path": "demo",
                    "recipe": {
                        "corpus": {
                            "id": "demo",
                            "name": "demo",
                            "license": "MIT"
                        },
                        "acquire": {
                            "type": "bulk_download",
                            "url": "https://example.com/data.zip"
                        },
                        "extract": { "type": "plaintext" },
                        "chunk":   { "type": "sentence" }
                    }
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let StepOutput::Json(v) = out else {
            panic!("expected json output");
        };
        let on_disk = root.join("demo/recipe.toml");
        assert!(on_disk.exists());
        let body = std::fs::read_to_string(&on_disk).unwrap();
        assert!(body.contains("[corpus]"));
        assert!(body.contains("id = \"demo\""));
        assert!(body.contains("[acquire]"));
        assert!(body.contains("type = \"bulk_download\""));
        // Validation report is present and passing.
        assert_eq!(v["validation"]["passed"], true, "report: {v}");
    }

    #[tokio::test]
    async fn nested_arrays_become_double_bracket_blocks() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeWriteStructuredTool::with_recipes_dir(root.clone());
        tool.execute(
            &json!({
                "path": "investigation-shape",
                "recipe": {
                    "corpus": { "id": "investigation-shape", "name": "x" },
                    "acquire": {
                        "type": "bulk_download",
                        "url": "https://example.com/data.zip"
                    },
                    "extract": { "type": "html" },
                    "chunk":   { "type": "paragraph" },
                    "enrichment": {
                        "enabled": true,
                        "type": "investigation",
                        "entity_types": [
                            { "name": "company",
                              "attributes": ["name", "ticker"] },
                            { "name": "person",
                              "attributes": ["name"] }
                        ],
                        "relationship_types": [
                            { "name": "investment",
                              "attributes": ["amount_usd"] }
                        ]
                    }
                }
            }),
            &ctx(),
        )
        .await
        .unwrap();
        let body = std::fs::read_to_string(root.join("investigation-shape/recipe.toml")).unwrap();
        let count = body.matches("[[enrichment.entity_types]]").count();
        assert_eq!(count, 2, "got: {body}");
        assert!(body.contains("[[enrichment.relationship_types]]"));
    }

    #[tokio::test]
    async fn accepts_flat_args_recipe_shape() {
        // Tolerant shape: agent emits recipe fields directly at args
        // root instead of under `recipe`. Tool should still produce a
        // valid recipe TOML.
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeWriteStructuredTool::with_recipes_dir(root.clone());
        let out = tool
            .execute(
                &json!({
                    "path": "flat-shape",
                    "corpus": { "id": "flat-shape", "name": "flat" },
                    "acquire": {
                        "type": "bulk_download",
                        "url": "https://example.com/data.zip"
                    },
                    "extract": { "type": "plaintext" },
                    "chunk":   { "type": "sentence" }
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let StepOutput::Json(v) = out else {
            panic!("expected json output");
        };
        let on_disk = root.join("flat-shape/recipe.toml");
        assert!(on_disk.exists());
        assert_eq!(v["validation"]["passed"], true, "report: {v}");
    }

    #[tokio::test]
    async fn null_optional_key_is_dropped_not_fatal() {
        // F1: a null-valued optional key (the 35B's `attribute: null` /
        // `max_pages: null` artifact) is now SANITIZED (dropped) before
        // conversion rather than hard-failing — the recipe writes and the
        // on-disk validator handles the rest, so the agent never needs the
        // raw-recipe_write fallback.
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeWriteStructuredTool::with_recipes_dir(root.clone());
        let out = tool
            .execute(
                &json!({
                    "path": "nulldrop",
                    "recipe": {
                        "corpus":  { "id": "nulldrop", "name": "nulldrop" },
                        "acquire": {
                            "type": "bulk_download",
                            "url": "https://example.com/data.zip"
                        },
                        "extract": { "type": "html" },
                        "chunk":   { "type": "paragraph", "max_chars": null }
                    }
                }),
                &ctx(),
            )
            .await
            .expect("null optional key should be dropped, not error the tool");
        let StepOutput::Json(_) = out else {
            panic!("expected json output");
        };
        let body = std::fs::read_to_string(root.join("nulldrop/recipe.toml")).unwrap();
        assert!(body.contains("[chunk]"));
        assert!(!body.contains("max_chars"), "null max_chars dropped: {body}");
    }

    #[tokio::test]
    async fn recovers_malformed_comparison_key_artifact() {
        // F1: the recurring `comparison": ` escaped-quote key artifact is
        // repaired to `comparison` so the threshold pattern survives
        // recipe_write_structured (previously a hard conversion failure).
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeWriteStructuredTool::with_recipes_dir(root.clone());
        let mut threshold = serde_json::Map::new();
        threshold.insert("type".into(), json!("threshold"));
        threshold.insert("name".into(), json!("hotspots"));
        threshold.insert("edge_type".into(), json!("occurred_near"));
        threshold.insert("attribute".into(), json!("sighting_count"));
        threshold.insert("threshold".into(), json!(3.0));
        // The artifact: a key carrying a stray escaped quote + colon.
        threshold.insert("comparison\": ".into(), json!("greater_than"));
        let out = tool
            .execute(
                &json!({
                    "path": "artifact",
                    "recipe": {
                        "corpus":  { "id": "artifact", "name": "artifact" },
                        "acquire": { "type": "bulk_download", "url": "https://example.com/d.zip" },
                        "extract": { "type": "jsonl", "content_field": "narrative" },
                        "chunk":   { "type": "paragraph" },
                        "enrichment": {
                            "enabled": true,
                            "type": "investigation",
                            "entity_types": [{ "name": "installation" }],
                            "relationship_types": [{ "name": "occurred_near" }],
                            "patterns": [serde_json::Value::Object(threshold)]
                        }
                    }
                }),
                &ctx(),
            )
            .await
            .expect("malformed comparison key should be repaired, not error");
        let StepOutput::Json(_) = out else {
            panic!("expected json output");
        };
        let body = std::fs::read_to_string(root.join("artifact/recipe.toml")).unwrap();
        assert!(body.contains("comparison = \"greater_than\""), "recovered key: {body}");
        assert!(!body.contains("comparison\\\""), "no escaped-quote key remains: {body}");
    }
}
