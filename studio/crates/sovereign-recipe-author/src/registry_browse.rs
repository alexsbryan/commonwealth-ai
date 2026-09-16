// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RegistryBrowseTool` — list recipes the agent can read for
//! shape examples, including the user's locally-published ones.
//!
//! Lightweight, read-only, no network — merges the bundled registry catalog
//! (`sovereign_contracts::recipe::registry::merged_catalog`) with the user's
//! `~/.svrnmesh/recipes/registry.toml`, replicating the engine's
//! bundled-snapshot + local-registry merge without a corpus-engine dependency.
//! Returns one row per recipe with `is_local` so the LLM can prefer
//! locally-authored examples (likely the most relevant) over the upstream
//! catalog.
//!
//! The bundled TOML is INJECTED at construction (the monolith supplies
//! `corpus_engine::registry::BUNDLED_REGISTRY_TOML`): this crate may not name
//! `corpus-engine`, and the shared leaf may not embed the artifact because that
//! `include_str!` escapes its crate root.

use async_trait::async_trait;

use sovereign_contracts::error::Result;
use sovereign_contracts::recipe::registry::merged_catalog;
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

pub struct RegistryBrowseTool {
    /// The bundled `sovereign-recipes/registry.toml` snapshot, injected by the
    /// monolith. The user's local registry is still read from `HOME`.
    bundled_registry_toml: &'static str,
}

impl RegistryBrowseTool {
    pub fn new(bundled_registry_toml: &'static str) -> Self {
        Self {
            bundled_registry_toml,
        }
    }
}

#[async_trait]
impl Tool for RegistryBrowseTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "registry_browse".into(),
            name: "RegistryBrowse".into(),
            description: "List every recipe in the registry (bundled + local). Use this \
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
                situation: "Find any existing SEC-shaped recipes before drafting one for \
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

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let filter = params
            .get("filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());

        // Bundled catalog merged with the user's `~/.svrnmesh/recipes/
        // registry.toml`, local entries winning by id (same precedence as the
        // engine's `RecipeRegistry::list_entries`). No network refresh.
        let mut rows = Vec::new();
        for row in merged_catalog(self.bundled_registry_toml) {
            let entry = &row.entry;
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
                "is_local": row.is_local,
            }));
        }
        Ok(StepOutput::Json(serde_json::json!({ "recipes": rows })))
    }
}

#[cfg(test)]
mod tests {
    // The crate-wide HOME test lock intentionally spans awaits: the guard
    // must cover the whole test body (HOME is process-global), and each
    // #[tokio::test] owns its runtime, so a contending sibling parks a
    // thread — serialization, never deadlock (P0.3 lock audit, 2026-07-12).
    #![allow(clippy::await_holding_lock)]

    use super::*;

    /// A minimal bundled snapshot, injected the way the monolith injects the
    /// real one. The tool's merge/filter logic is what these tests exercise;
    /// the REAL catalog's non-emptiness is asserted by
    /// `sovereign-tools/tests/recipe_author_loop.rs`, which has the real bytes.
    const BUNDLED: &str = r#"
[[recipes]]
id = "wikipedia"
name = "Wikipedia (English)"
description = "The encyclopedia."
license = "CC-BY-SA-4.0"

[[recipes]]
id = "sec-filings-company"
name = "SEC filings"
description = "Company filings."
license = "Public domain"
"#;

    fn tool() -> RegistryBrowseTool {
        RegistryBrowseTool::new(BUNDLED)
    }

    #[tokio::test]
    async fn lists_injected_registry() {
        // Use a fresh HOME so no local registry leaks in. HOME is
        // process-global — hold the crate-wide lock for the test's
        // lifetime (see `recipe_author::home_test_lock`).
        let _guard = crate::home_test_lock();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let out = tool()
            .execute(&serde_json::json!({}), &ToolContext::default())
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
        let _guard = crate::home_test_lock();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let out = tool()
            .execute(
                &serde_json::json!({"filter": "wikipedia"}),
                &ToolContext::default(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                let recipes = v["recipes"].as_array().unwrap();
                assert_eq!(recipes.len(), 1, "only wikipedia matches");
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
