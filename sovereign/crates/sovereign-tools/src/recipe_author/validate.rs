// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RecipeValidateTool` — schema + regex compile + URL-template
//! placeholder cross-reference checks against a recipe TOML.
//!
//! Wraps `corpus_engine`'s test harness in `--offline, sample_size
//! = 0` mode so it runs the full validator without touching any
//! network or file beyond the recipe itself. Returns a structured
//! `{errors: [...], warnings: [...], passed: bool}` payload so the
//! LLM can branch on `passed` and iterate on the bad pattern.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use corpus_engine::{CorpusEngine, EmbedFn, TestOptions};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::resolve_recipe_path;

#[derive(Default)]
pub struct RecipeValidateTool {
    recipes_dir: Option<PathBuf>,
}

impl RecipeValidateTool {
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
impl Tool for RecipeValidateTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recipe_validate".into(),
            name: "RecipeValidate".into(),
            description: "Validate a recipe TOML at ~/.sovereign/recipes/<id>/recipe.toml \
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
                            "Recipe id or relative path under ~/.sovereign/recipes/",
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

        let engine = build_stub_engine();
        let options = TestOptions {
            sample_size: 0,
            embed: false,
            offline: true,
            ..Default::default()
        };
        let report = engine
            .test_recipe(&resolved, &options)
            .await
            .map_err(|e| Error::InvalidInput(format!("{e}")))?;
        let passed = report.validation.errors.is_empty();
        Ok(StepOutput::Json(serde_json::json!({
            "passed": passed,
            "errors": report.validation.errors,
            "warnings": report.validation.warnings,
            "path": resolved.display().to_string(),
        })))
    }
}

/// CorpusEngine with a stub embed function. Validation never
/// touches embeddings, but the engine constructor requires an
/// EmbedFn.
fn build_stub_engine() -> CorpusEngine {
    let stub_embed: EmbedFn = Arc::new(|_text| Box::pin(async { Ok(vec![0f32; corpus_engine::DEFAULT_EMBED_DIM]) }));
    let tmp = std::env::temp_dir().join("sovereign-recipe-author-validate");
    CorpusEngine::new(tmp.clone(), tmp, stub_embed)
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
    async fn passes_clean_recipe() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let dir = root.join("clean");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("recipe.toml"),
            r#"
[corpus]
id = "clean"
name = "clean"
license = "MIT"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#,
        )
        .unwrap();

        let tool = RecipeValidateTool::with_recipes_dir(root);
        let out = tool
            .execute(&serde_json::json!({"path": "clean"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert_eq!(v["passed"], true, "got: {v}");
                assert_eq!(v["errors"].as_array().unwrap().len(), 0);
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn flags_undeclared_placeholder() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let dir = root.join("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("recipe.toml"),
            r#"
[corpus]
id = "bad"
name = "bad"

[parameters.entity]
type = "list"
required = true

[acquire]
type = "http_api"
base_url = "https://api.example.com"

[[acquire.requests]]
url = "{base_url}?q={entity}&category={category}"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#,
        )
        .unwrap();

        let tool = RecipeValidateTool::with_recipes_dir(root);
        let out = tool
            .execute(&serde_json::json!({"path": "bad"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert_eq!(v["passed"], false);
                assert!(v["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e.as_str().unwrap().contains("{category}")));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }
}
