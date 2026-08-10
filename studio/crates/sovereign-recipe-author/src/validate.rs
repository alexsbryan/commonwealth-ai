// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RecipeValidateTool` — schema + regex compile + URL-template
//! placeholder cross-reference checks against a recipe TOML.
//!
//! Runs the injected [`RecipeTester`] in `offline, sample_size = 0`
//! mode so it exercises the full validator (schema + regex compile +
//! placeholder cross-reference + for_each resolution) without touching
//! any network or file beyond the recipe itself. The in-process tester
//! adapter wraps `corpus_engine`'s test harness; the tool depends only
//! on the contract. Returns a structured `{errors, warnings, passed}`
//! payload so the LLM can branch on `passed` and iterate on the bad
//! pattern.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::recipe::testing::{RecipeTestParams, RecipeTester};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

use super::resolve_recipe_path;

pub struct RecipeValidateTool {
    recipes_dir: Option<PathBuf>,
    tester: Arc<dyn RecipeTester>,
}

impl RecipeValidateTool {
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
impl Tool for RecipeValidateTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recipe_validate".into(),
            name: "RecipeValidate".into(),
            description: "Validate a recipe TOML at ~/.svrnmesh/recipes/<id>/recipe.toml \
                 — schema, regex compile, URL-template placeholder cross-reference, \
                 for_each parameter resolution. Returns structured \
                 `{errors, warnings, passed}` so you can iterate on broken patterns \
                 without re-reading the file."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description":
                            "Recipe id or relative path under ~/.svrnmesh/recipes/",
                    }
                },
                "required": ["path"],
            }),
            examples: vec![ToolExample {
                situation: "Validate the SEC investigation recipe before testing extraction."
                    .into(),
                call: serde_json::json!({"path": "sec-ai-investigation"}),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "passed": { "type": "boolean" },
                    "errors": { "type": "array", "items": { "type": "string" } },
                    "warnings": { "type": "array", "items": { "type": "string" } }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("RecipeValidateTool requires `path`".into()))?;
        let resolved = resolve_recipe_path(raw_path, self.recipes_dir.as_ref())?;
        if !resolved.is_file() {
            return Err(Error::InvalidInput(format!(
                "recipe not found at {}",
                resolved.display()
            )));
        }

        // sample_size = 0 = validation-only: schema + regex-compile +
        // URL-template placeholder cross-reference + for_each resolution, no
        // download or extraction.
        let params = RecipeTestParams {
            sample_size: 0,
            embed: false,
            offline: true,
            ..Default::default()
        };
        let report = self.tester.test(&resolved, &params).await?;
        let passed = report.validation.errors.is_empty();
        Ok(StepOutput::Json(serde_json::json!({
            "passed": passed,
            "errors": report.validation.errors,
            "warnings": report.validation.warnings,
            "path": resolved.display().to_string(),
        })))
    }
}
