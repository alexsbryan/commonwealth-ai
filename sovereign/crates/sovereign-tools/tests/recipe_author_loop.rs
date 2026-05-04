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
        Box::new(sovereign_tools::CheckpointTool::new()),
        Box::new(sovereign_tools::DecisionLogTool::new()),
        Box::new(sovereign_tools::CapabilityRequestTool::new()),
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

/// Drives the new tools end-to-end against a tempdir-anchored
/// `RecipeProject`: provision → checkpoint at creation → log
/// decisions covering all five `decision_kind` variants → capability
/// request gated on partner_confirmed → restore from the creation
/// checkpoint. Mirrors the M1 acceptance scenario in the plan file.
#[tokio::test]
async fn recipe_author_project_lifecycle_end_to_end() {
    use std::sync::Arc;

    use corpus_engine::{FeatureStore, NoteScope, NoteStore, ScopeFilter};
    use sovereign_tools::recipe_author::{
        capability_request::CapabilityRequest,
        checkpoint::{do_create as checkpoint_create, restore_checkpoint},
        decision_log::{DecisionAttribution, DecisionKind, DecisionPayload},
        situated_context, CapabilityRequestTool, DecisionLogTool,
    };
    use sovereign_tools::RecipeProject;

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    let recipes_dir = home.path().join(".sovereign/recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let notes = Arc::new(NoteStore::open(&home.path().join("notes.db")).unwrap());
    let features =
        Arc::new(FeatureStore::open(&home.path().join("features.db")).unwrap());

    let project = RecipeProject::new(
        "Federal case law (CourtListener)",
        "Build a corpus of federal published opinions over CourtListener \
         with a citation graph and a counsel-of-record investigation.",
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .await
    .unwrap();

    // Project-creation checkpoint — recipe path absent because the
    // recipe hasn't been drafted yet.
    let creation = checkpoint_create(
        &project,
        "creation",
        "Project just provisioned.",
        "project_creation",
        None,
        Some(&recipes_dir),
        "session-1",
        None,
    )
    .await
    .unwrap();
    assert!(creation.snapshot_path.exists());

    // Five decisions — one per kind. The DecisionLogTool wraps
    // the NoteStore write path; calling it directly through the tool
    // exercises the JSON entry surface the live agent would use.
    let dl = DecisionLogTool::with_notes(Arc::clone(&notes));
    let kinds = [
        ("source_choice", DecisionAttribution::Partner),
        ("extraction_choice", DecisionAttribution::AgentDefault),
        ("schema_choice", DecisionAttribution::Partner),
        ("domain_clarification", DecisionAttribution::Partner),
        ("deferred_question", DecisionAttribution::Deferred),
    ];
    for (k, attribution) in &kinds {
        let attr_str = match attribution {
            DecisionAttribution::Partner => "partner",
            DecisionAttribution::AgentDefault => "agent_default",
            DecisionAttribution::Deferred => "deferred",
        };
        dl.execute(
            &serde_json::json!({
                "feature_id": project.feature_id(),
                "kind": k,
                "summary": format!("a {k}"),
                "attribution": attr_str,
            }),
            &ctx(),
        )
        .await
        .unwrap_or_else(|e| panic!("{k}: {e}"));
    }
    // All five `decision_kind` variants survive the round-trip.
    let scope = ScopeFilter {
        scopes: vec![NoteScope::Feature],
        feature_id: Some(project.feature_id().to_string()),
    };
    let decision_rows = notes
        .read_notes_scoped(
            None,
            &[],
            &[],
            &["decision".to_string()],
            100,
            false,
            &scope,
        )
        .await
        .unwrap();
    assert_eq!(decision_rows.len(), 5);
    let mut kinds_seen = std::collections::HashSet::new();
    for r in &decision_rows {
        let payload: DecisionPayload =
            serde_json::from_str(r.payload_json.as_deref().unwrap()).unwrap();
        kinds_seen.insert(payload.decision_kind);
    }
    for expected in [
        DecisionKind::SourceChoice,
        DecisionKind::ExtractionChoice,
        DecisionKind::SchemaChoice,
        DecisionKind::DomainClarification,
        DecisionKind::DeferredQuestion,
    ] {
        assert!(kinds_seen.contains(&expected), "missing {expected:?}");
    }

    // Capability request — refusal path first, then the confirmed
    // path. Persistence side-effect on the inbox lives in a tempdir.
    let inbox = tempfile::tempdir().unwrap();
    let cap = CapabilityRequestTool::with_stores(
        Arc::clone(&notes),
        Arc::clone(&features),
    )
    .with_inbox_dir(inbox.path().to_path_buf());
    let refuse = cap
        .execute(
            &serde_json::json!({
                "feature_id": project.feature_id(),
                "format_or_source": "PACER docket XML",
                "analysis": "existing xml extractor flattens the structure",
                "partner_confirmed": false,
            }),
            &ctx(),
        )
        .await
        .unwrap_err();
    assert!(format!("{refuse}").contains("partner_confirmed"));

    let submitted = cap
        .execute(
            &serde_json::json!({
                "feature_id": project.feature_id(),
                "format_or_source": "PACER docket XML",
                "analysis": "Need an XML extractor that preserves nested \
                             docket-entry hierarchy.",
                "existing_extractors_tried": ["xml", "html"],
                "failure_modes": ["xml flattens", "html splits boundaries"],
                "blocked_recipe_parts": ["extract"],
                "partner_confirmed": true,
            }),
            &ctx(),
        )
        .await
        .unwrap();
    let inbox_path = match &submitted {
        StepOutput::Json(v) => v["inbox_path"].as_str().unwrap().to_string(),
        other => panic!("expected Json, got {other:?}"),
    };
    let cap_request: CapabilityRequest =
        serde_json::from_str(&std::fs::read_to_string(&inbox_path).unwrap()).unwrap();
    assert_eq!(cap_request.status, "submitted");
    assert_eq!(cap_request.feature_id, project.feature_id());

    // Now write a real recipe + checkpoint it, then mutate, then
    // restore. Confirms the restore wires the checkpoint snapshot
    // back to the live recipe path.
    std::fs::create_dir_all(recipes_dir.join("trial")).unwrap();
    std::fs::write(
        recipes_dir.join("trial/recipe.toml"),
        "[corpus]\nid=\"trial\"\nname=\"v1\"\n",
    )
    .unwrap();
    let v1 = checkpoint_create(
        &project,
        "v1 settled",
        "first working draft",
        "auto_strategy_change",
        Some("trial"),
        Some(&recipes_dir),
        "session-1",
        None,
    )
    .await
    .unwrap();
    std::fs::write(
        recipes_dir.join("trial/recipe.toml"),
        "[corpus]\nid=\"trial\"\nname=\"v2-broken\"\n",
    )
    .unwrap();
    let _restored = restore_checkpoint(
        &project,
        &v1.checkpoint_id,
        Some("trial"),
        Some(&recipes_dir),
        "session-1",
    )
    .await
    .unwrap();
    let restored_text =
        std::fs::read_to_string(recipes_dir.join("trial/recipe.toml")).unwrap();
    assert!(restored_text.contains("v1"));
    assert!(!restored_text.contains("v2-broken"));

    // Situated-context render covers the project after all the
    // above: charter, recent decisions (newest first), pending
    // capability request, and the restore-anchor checkpoint should
    // all be reachable from the rendered block.
    let block = situated_context::render(&project).await.unwrap();
    assert!(block.contains("Federal case law"));
    assert!(block.contains("Recent decisions"));
    assert!(block.contains("Pending capability requests"));
    assert!(block.contains("PACER"));
}
