// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for the recipe-authoring tools that drive the *real*
//! `RecipeTester` (backed by `corpus_engine` via `CorpusEngineRecipeTester`).
//!
//! These moved out of the `sovereign-recipe-author` package's inline unit tests
//! in B:P6: that package's dependency budget deliberately excludes corpus-engine,
//! so the tools take an injected `Arc<dyn RecipeTester>` and are unit-tested there
//! against an in-memory stub. The cases that assert on the *tester's* real
//! validation output (schema + placeholder cross-reference + regex compile, all
//! offline via the validation-only path) need the concrete adapter — so they live
//! here, in `sovereign-tools`, which links corpus-engine and re-exports the tools
//! at their old paths.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use sovereign_contracts::recipe::testing::RecipeTester;
use sovereign_core::traits::Tool;
use sovereign_core::types::{ConversationId, StepOutput, ToolContext};
use sovereign_tools::recipe_tester_adapter::CorpusEngineRecipeTester;
use sovereign_tools::{RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool};

fn tester() -> Arc<dyn RecipeTester> {
    Arc::new(CorpusEngineRecipeTester::new())
}

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

// ── RecipeValidateTool ──────────────────────────────────────────────────────

#[tokio::test]
async fn validate_passes_clean_recipe() {
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

    let tool = RecipeValidateTool::with_recipes_dir(tester(), root);
    let out = tool
        .execute(&json!({"path": "clean"}), &ctx())
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
async fn validate_flags_undeclared_placeholder() {
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

    let tool = RecipeValidateTool::with_recipes_dir(tester(), root);
    let out = tool.execute(&json!({"path": "bad"}), &ctx()).await.unwrap();
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

// ── RecipeTestTool ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_tool_validation_only_run_returns_passed_for_clean_recipe() {
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

    // sample_size = 0 = validation-only path through test_recipe.
    let tool = RecipeTestTool::with_recipes_dir(tester(), root);
    let out = tool
        .execute(
            &json!({"path": "clean", "sample_size": 0, "offline": true}),
            &ctx(),
        )
        .await
        .unwrap();
    match out {
        StepOutput::Json(v) => {
            assert_eq!(v["passed"], true, "got: {v}");
            assert_eq!(v["validation"]["errors"].as_array().unwrap().len(), 0);
        }
        other => panic!("expected Json, got {other:?}"),
    }
}

// ── RecipeWriteStructuredTool ───────────────────────────────────────────────

#[tokio::test]
async fn write_structured_writes_clean_recipe_from_structured_input() {
    let home = tempfile::tempdir().unwrap();
    let root = make_root(home.path());
    let tool = RecipeWriteStructuredTool::with_recipes_dir(tester(), root.clone());
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
async fn write_structured_nested_arrays_become_double_bracket_blocks() {
    let home = tempfile::tempdir().unwrap();
    let root = make_root(home.path());
    let tool = RecipeWriteStructuredTool::with_recipes_dir(tester(), root.clone());
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
async fn write_structured_accepts_flat_args_recipe_shape() {
    // Tolerant shape: agent emits recipe fields directly at args root instead of
    // under `recipe`. Tool should still produce a valid recipe TOML.
    let home = tempfile::tempdir().unwrap();
    let root = make_root(home.path());
    let tool = RecipeWriteStructuredTool::with_recipes_dir(tester(), root.clone());
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
async fn write_structured_null_optional_key_is_dropped_not_fatal() {
    // F1: a null-valued optional key (the 35B's `attribute: null` / `max_pages:
    // null` artifact) is SANITIZED (dropped) before conversion rather than
    // hard-failing — the recipe writes and the on-disk validator handles the rest,
    // so the agent never needs the raw-recipe_write fallback.
    let home = tempfile::tempdir().unwrap();
    let root = make_root(home.path());
    let tool = RecipeWriteStructuredTool::with_recipes_dir(tester(), root.clone());
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
    assert!(
        !body.contains("max_chars"),
        "null max_chars dropped: {body}"
    );
}

#[tokio::test]
async fn write_structured_recovers_malformed_comparison_key_artifact() {
    // F1: the recurring `comparison": ` escaped-quote key artifact is repaired to
    // `comparison` so the threshold pattern survives recipe_write_structured
    // (previously a hard conversion failure).
    let home = tempfile::tempdir().unwrap();
    let root = make_root(home.path());
    let tool = RecipeWriteStructuredTool::with_recipes_dir(tester(), root.clone());
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
    assert!(
        body.contains("comparison = \"greater_than\""),
        "recovered key: {body}"
    );
    assert!(
        !body.contains("comparison\\\""),
        "no escaped-quote key remains: {body}"
    );
}
