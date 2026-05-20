//! `RegistryBrowseTool` — list recipes the agent can read for
//! shape examples, including the user's locally-published ones.
//!
//! Lightweight, read-only, no network — uses the same bundled
//! snapshot + local-registry-merge path the CLI's `recipe list`
//! uses. Returns one row per recipe with `is_local` so the LLM
//! can prefer locally-authored examples (likely the most
//! relevant) over the upstream catalog.

use async_trait::async_trait;

use corpus_engine::RecipeRegistry;
use sovereign_core::error::Result;
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct RegistryBrowseTool;

#[async_trait]
impl Tool for RegistryBrowseTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "registry_browse".into(),
            name: "RegistryBrowse".into(),
            description:
                "List every recipe in the registry (bundled + local). Use this \
                 first when authoring a new recipe — there's likely an existing \
                 example with a similar acquire/extract shape you can read with \
                 RecipeRead and pattern off."
                    .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "string",
                        "description":
                            "Substring to filter recipe ids/names (case-insensitive)"
                    }
                }
            }),
            examples: vec![ToolExample {
                situation:
                    "Find any existing SEC-shaped recipes before drafting one for \
                     CourtListener."
                        .into(),
                call: serde_json::json!({"filter": "sec"}),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "recipes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "name": { "type": "string" },
                                "description": { "type": "string" },
                                "license": { "type": "string" },
                                "is_local": { "type": "boolean" }
                            }
                        }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::RecipeAuthoring]
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let filter = params
            .get("filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        let local_dir = RecipeRegistry::default_local_recipes_dir();
        let mut registry = RecipeRegistry::from_bundled(local_dir.clone());
        if let Some(d) = &local_dir {
            registry = registry.with_local_registry(&d.join("registry.toml"));
        }

        let mut rows = Vec::new();
        for entry in registry.list_entries() {
            if let Some(needle) = &filter {
                let id = entry.id.to_lowercase();
                let name = entry.name.to_lowercase();
                if !id.contains(needle) && !name.contains(needle) {
                    continue;
                }
            }
            rows.push(serde_json::json!({
                "id": entry.id,
                "name": entry.name,
                "description": entry.description,
                "license": entry.license,
                "size_compressed_gb": entry.size_compressed_gb,
                "size_indexed_gb": entry.size_indexed_gb,
                "enrichment_enabled": entry.enrichment_enabled,
                "is_local": registry.is_local_entry(&entry.id),
            }));
        }
        Ok(StepOutput::Json(serde_json::json!({ "recipes": rows })))
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

    #[tokio::test]
    async fn lists_bundled_recipes() {
        // Use a fresh HOME so no local registry leaks in.
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let out = RegistryBrowseTool
            .execute(&serde_json::json!({}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                let recipes = v["recipes"].as_array().unwrap();
                assert!(!recipes.is_empty(), "bundled snapshot should have entries");
                // Every row should have the required keys.
                for r in recipes {
                    assert!(r["id"].is_string());
                    assert_eq!(r["is_local"], false);
                }
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn filter_narrows_to_substring() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let out = RegistryBrowseTool
            .execute(&serde_json::json!({"filter": "wikipedia"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                let recipes = v["recipes"].as_array().unwrap();
                assert!(recipes.iter().all(|r| r["id"]
                    .as_str()
                    .unwrap()
                    .to_lowercase()
                    .contains("wikipedia")
                    || r["name"]
                        .as_str()
                        .unwrap()
                        .to_lowercase()
                        .contains("wikipedia")));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }
}
