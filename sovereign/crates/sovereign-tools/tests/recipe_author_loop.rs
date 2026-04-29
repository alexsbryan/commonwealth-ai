//! End-to-end test of the recipe-authoring tool loop.
//!
//! Drives the five tools in the order an LLM would call them:
//!
//! 1. `RegistryBrowseTool` — survey existing recipes.
//! 2. `RecipeReadTool` — read a known recipe for shape patterns.
//! 3. `RecipeWriteTool` — draft a new recipe with a deliberate bug
//!    (undeclared placeholder).
//! 4. `RecipeValidateTool` — confirm validation FLAGS the bug
//!    (so the loop has something to react to).
//! 5. `RecipeWriteTool` — fix the bug.
//! 6. `RecipeValidateTool` — confirm it now passes.
//!
//! Exercises the tools' descriptors + structured-JSON returns so a
//! real planner-driven flow can compose them deterministically.
//! Lives at the integration-test level so we catch any "tools work
//! solo but not together" regressions.

use std::path::PathBuf;

use sovereign_core::types::{ConversationId, StepOutput, ToolContext};
use sovereign_core::traits::Tool;
use sovereign_tools::{
    RecipeReadTool, RecipeValidateTool, RecipeWriteTool, RegistryBrowseTool,
};

fn ctx() -> ToolContext {
    ToolContext {
        conversation_id: ConversationId::new(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
    }
}

fn make_root(home: &std::path::Path) -> PathBuf {
    let recipes = home.join(".sovereign/recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    recipes
}

#[tokio::test]
async fn full_author_loop_exercises_all_tools() {
    let home = tempfile::tempdir().unwrap();
    let root = make_root(home.path());

    // Step 1: browse the registry. With a fresh HOME, only bundled
    // recipes show; that's enough for the LLM to pick a shape.
    let browse = RegistryBrowseTool;
    let listed = browse
        .execute(&serde_json::json!({}), &ctx())
        .await
        .unwrap();
    let bundled_count = match listed {
        StepOutput::Json(v) => v["recipes"].as_array().unwrap().len(),
        other => panic!("expected Json, got {other:?}"),
    };
    assert!(bundled_count > 0, "bundled snapshot should list recipes");

    // Step 2: read tool surface — point at a non-existent local
    // recipe (the agent often does this first) and confirm
    // `exists: false` is the structured signal.
    let read = RecipeReadTool::with_recipes_dir(root.clone());
    let probe = read
        .execute(&serde_json::json!({"path": "sec-investigation"}), &ctx())
        .await
        .unwrap();
    match probe {
        StepOutput::Json(v) => {
            assert_eq!(v["exists"], false);
            assert_eq!(v["content"], "");
        }
        other => panic!("expected Json, got {other:?}"),
    }

    // Step 3: draft a recipe with a deliberate bug — a `{category}`
    // placeholder in the URL that's not in `[recipe.parameters]`.
    // The validator should flag it.
    let buggy_toml = r#"
[corpus]
id = "sec-investigation"
name = "SEC investigation (draft)"

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
"#;
    let write = RecipeWriteTool::with_recipes_dir(root.clone());
    let written = write
        .execute(
            &serde_json::json!({
                "path": "sec-investigation",
                "content": buggy_toml,
            }),
            &ctx(),
        )
        .await
        .unwrap();
    match written {
        StepOutput::Json(v) => {
            assert!(v["bytes_written"].as_u64().unwrap() > 0);
        }
        other => panic!("expected Json, got {other:?}"),
    }

    // Step 4: validate — should FAIL with `{category}` flagged.
    let validate = RecipeValidateTool::with_recipes_dir(root.clone());
    let bad_validation = validate
        .execute(
            &serde_json::json!({"path": "sec-investigation"}),
            &ctx(),
        )
        .await
        .unwrap();
    match bad_validation {
        StepOutput::Json(v) => {
            assert_eq!(v["passed"], false, "validator should flag the bug");
            assert!(
                v["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e.as_str().unwrap().contains("{category}")),
                "expected `{{category}}` error, got: {}",
                v["errors"],
            );
        }
        other => panic!("expected Json, got {other:?}"),
    }

    // Step 5: fix — declare the missing parameter.
    let fixed_toml = r#"
[corpus]
id = "sec-investigation"
name = "SEC investigation"

[parameters.entity]
type = "list"
required = true

[parameters.category]
type = "string"
default = "10-K"

[acquire]
type = "http_api"
base_url = "https://api.example.com"

[[acquire.requests]]
url = "{base_url}?q={entity}&category={category}"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#;
    write
        .execute(
            &serde_json::json!({
                "path": "sec-investigation",
                "content": fixed_toml,
            }),
            &ctx(),
        )
        .await
        .unwrap();

    // Step 6: re-validate — should pass now.
    let good_validation = validate
        .execute(
            &serde_json::json!({"path": "sec-investigation"}),
            &ctx(),
        )
        .await
        .unwrap();
    match good_validation {
        StepOutput::Json(v) => {
            assert_eq!(v["passed"], true, "fix should validate clean: {v}");
            assert_eq!(v["errors"].as_array().unwrap().len(), 0);
        }
        other => panic!("expected Json, got {other:?}"),
    }

    // Sanity: the file we wrote is on disk where the publish CLI
    // expects it (`<id>/recipe.toml` under the recipes root).
    let on_disk = root.join("sec-investigation/recipe.toml");
    assert!(on_disk.is_file(), "recipe should land at {}", on_disk.display());
    assert!(std::fs::read_to_string(&on_disk)
        .unwrap()
        .contains("[parameters.category]"));
}

/// Confirms the descriptor-level metadata on every tool is
/// what the planner / approval-gate code reads. Catches "I
/// renamed a tool id and 12 places broke" silently.
#[test]
fn tool_descriptors_carry_recipe_authoring_permission() {
    use sovereign_core::types::Permission;

    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(RecipeReadTool::new()),
        Box::new(RecipeWriteTool::new()),
        Box::new(RecipeValidateTool::new()),
        Box::new(sovereign_tools::RecipeTestTool::new()),
        Box::new(RegistryBrowseTool),
    ];
    for tool in &tools {
        assert!(
            tool.required_permissions()
                .contains(&Permission::RecipeAuthoring),
            "tool `{}` is missing Permission::RecipeAuthoring",
            tool.descriptor().id
        );
    }
}
