// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RecipeReadTool` — read a recipe TOML.
//!
//! Mirrors `FileTool { action: "read" }` but scoped to the
//! local recipes directory so the LLM can survey existing
//! recipes for shape without holding broad `FileRead` permission.

use std::path::PathBuf;

use async_trait::async_trait;

use sovereign_contracts::error::Result;
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

use super::resolve_recipe_path;

#[derive(Default)]
pub struct RecipeReadTool {
    /// Override the default `~/.svrnmesh/recipes/` root. Used by
    /// tests to avoid mutating process-global `HOME`; production
    /// callers leave this `None`.
    recipes_dir: Option<PathBuf>,
}

impl RecipeReadTool {
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
impl Tool for RecipeReadTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recipe_read".into(),
            name: "RecipeRead".into(),
            description: "Read a recipe TOML file from ~/.svrnmesh/recipes/. Use this to \
                 survey an existing recipe's shape before drafting a new one. \
                 Pass the recipe id (e.g. \"sec-investigation\") or a relative \
                 path under ~/.svrnmesh/recipes/."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description":
                            "Recipe id (loads <id>/recipe.toml) or relative path \
                             under ~/.svrnmesh/recipes/",
                    }
                },
                "required": ["path"],
            }),
            examples: vec![ToolExample {
                situation: "Survey the existing SEC investigation recipe before \
                         drafting one for CourtListener."
                    .into(),
                call: serde_json::json!({"path": "sec-investigation"}),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "exists": { "type": "boolean" },
                    "content": { "type": "string" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            sovereign_contracts::error::Error::InvalidInput(
                "RecipeReadTool requires a `path` parameter".into(),
            )
        })?;
        let resolved = resolve_recipe_path(raw_path, self.recipes_dir.as_ref())?;

        let (exists, content) = match std::fs::read_to_string(&resolved) {
            Ok(s) => (true, s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, String::new()),
            Err(e) => {
                return Err(sovereign_contracts::error::Error::InvalidInput(format!(
                    "failed to read {}: {e}",
                    resolved.display()
                )))
            }
        };
        Ok(StepOutput::Json(serde_json::json!({
            "path": resolved.display().to_string(),
            "exists": exists,
            "content": content,
        })))
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
            ..Default::default()
        }
    }

    fn make_root(home: &std::path::Path) -> PathBuf {
        let recipes = home.join(".sovereign/recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        recipes
    }

    #[tokio::test]
    async fn reads_existing_recipe() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let demo = root.join("demo");
        std::fs::create_dir_all(&demo).unwrap();
        std::fs::write(demo.join("recipe.toml"), "id = \"demo\"").unwrap();

        let tool = RecipeReadTool::with_recipes_dir(root);
        let out = tool
            .execute(&serde_json::json!({"path": "demo"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert_eq!(v["exists"], true);
                assert!(v["content"].as_str().unwrap().contains("\"demo\""));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_recipe_reports_exists_false() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeReadTool::with_recipes_dir(root);
        let out = tool
            .execute(&serde_json::json!({"path": "nope"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert_eq!(v["exists"], false);
                assert_eq!(v["content"], "");
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_traversal_path() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeReadTool::with_recipes_dir(root);
        let err = tool
            .execute(&serde_json::json!({"path": "../etc/passwd"}), &ctx())
            .await
            .unwrap_err();
        assert!(format!("{err}").contains(".."));
    }
}
