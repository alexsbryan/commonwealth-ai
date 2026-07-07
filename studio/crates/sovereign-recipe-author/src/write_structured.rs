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
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::recipe::testing::{RecipeTestParams, RecipeTester};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

use super::json_to_toml::{json_to_toml, toml_value_to_string};
use super::recipe_schema::recipe_json_schema;
use super::resolve_recipe_path;

pub struct RecipeWriteStructuredTool {
    /// Optional override for the recipes-dir root. Tests inject a
    /// per-test tempdir so the tool runs without mutating
    /// process-global `HOME`.
    recipes_dir: Option<PathBuf>,
    /// Runs the on-disk validation after writing (the RecipeTester seam, so
    /// this tool carries no corpus-engine dependency).
    tester: Arc<dyn RecipeTester>,
}

impl RecipeWriteStructuredTool {
    pub fn new(tester: Arc<dyn RecipeTester>) -> Self {
        Self {
            recipes_dir: None,
            tester,
        }
    }

    pub fn with_recipes_dir(tester: Arc<dyn RecipeTester>, dir: PathBuf) -> Self {
        Self {
            recipes_dir: Some(dir),
            tester,
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
        let validation_report = run_disk_validation(&resolved, self.tester.as_ref()).await;

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
async fn run_disk_validation(
    path: &std::path::Path,
    tester: &dyn RecipeTester,
) -> serde_json::Value {
    let params = RecipeTestParams {
        sample_size: 0,
        offline: true,
        ..Default::default()
    };
    match tester.test(path, &params).await {
        Ok(report) => {
            let passed = report.passed;
            let errors: Vec<String> = report.validation.errors.clone();
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
