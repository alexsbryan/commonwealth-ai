// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RecipeWriteTool` — create or update a recipe TOML, scoped to
//! the local recipes directory.
//!
//! The agent calls this to draft a new recipe (or iterate on an
//! existing local one). Writes are atomic: a tmp file gets the
//! bytes first, then renamed into place, so a crashed write never
//! leaves a partially-written recipe on disk that the validator
//! would reject mysteriously.

use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

use super::resolve_recipe_path;

#[derive(Default)]
pub struct RecipeWriteTool {
    recipes_dir: Option<PathBuf>,
}

impl RecipeWriteTool {
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
impl Tool for RecipeWriteTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recipe_write".into(),
            name: "RecipeWrite".into(),
            description: "Write a recipe TOML to ~/.svrnmesh/recipes/<id>/recipe.toml. \
                 Creates parent directories if needed. ALWAYS read an existing \
                 example recipe first (RecipeRead) so the new recipe matches \
                 the schema; the validator will reject malformed shapes."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description":
                            "Recipe id (writes to <id>/recipe.toml) or \
                             relative path under ~/.svrnmesh/recipes/",
                    },
                    "content": {
                        "type": "string",
                        "description": "Full TOML document to write",
                    }
                },
                "required": ["path", "content"],
            }),
            examples: vec![ToolExample {
                situation: "Draft a SEC investigation recipe based on patterns observed \
                     in an existing one."
                    .into(),
                call: serde_json::json!({
                    "path": "sec-ai-investigation",
                    "content": "[corpus]\nid = \"sec-ai-investigation\"\n…"
                }),
            }],
            effect: Effect::ReadWrite,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "bytes_written": { "type": "integer" }
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
            .ok_or_else(|| Error::InvalidInput("RecipeWriteTool requires `path`".into()))?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("RecipeWriteTool requires `content`".into()))?;

        let resolved = resolve_recipe_path(raw_path, self.recipes_dir.as_ref())?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::InvalidInput(format!("failed to create parent {}: {e}", parent.display()))
            })?;
        }
        let part = resolved.with_extension("toml.part");
        fs::write(&part, content)
            .map_err(|e| Error::InvalidInput(format!("failed to write {}: {e}", part.display())))?;
        fs::rename(&part, &resolved).map_err(|e| {
            Error::InvalidInput(format!(
                "failed to commit {} → {}: {e}",
                part.display(),
                resolved.display(),
            ))
        })?;

        Ok(StepOutput::Json(serde_json::json!({
            "path": resolved.display().to_string(),
            "bytes_written": content.len(),
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
    async fn writes_under_recipes_dir_and_creates_parent() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());

        let body = "[corpus]\nid = \"demo\"\nname = \"demo\"\n";
        let tool = RecipeWriteTool::with_recipes_dir(root);
        let out = tool
            .execute(
                &serde_json::json!({"path": "demo", "content": body}),
                &ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                let p = PathBuf::from(v["path"].as_str().unwrap());
                assert!(p.exists());
                assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_path_outside_recipes_dir() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let tool = RecipeWriteTool::with_recipes_dir(root);
        let err = tool
            .execute(
                &serde_json::json!({"path": "/tmp/elsewhere.toml", "content": ""}),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("outside"));
    }
}
