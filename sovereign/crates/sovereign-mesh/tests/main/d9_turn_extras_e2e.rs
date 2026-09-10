// SPDX-License-Identifier: AGPL-3.0-or-later
//! `turn_extras_http` end to end — the two reads a turn leaves behind
//! (sv-surface D9).
//!
//! Both routes answer a question about the process that RAN the turn,
//! and both were answered on the desktop by reading the DESKTOP's
//! `Runtime`. Since R5 every turn rides the wire in both boot modes, so
//! in attach that register is structurally empty and the two commands
//! reported the emptiness as an answer. These cases exercise the routes
//! against a real serving commission carrying a real registry and a
//! real captured frame — a stub runtime with an empty `SkillRegistry`
//! (what the three sibling fixtures mint) cannot exhibit the fault.
//!
//! # Red-watch (2026-09-10, run)
//!
//! Both routes moved off their paths (`/v1/skills-planted`,
//! `.../provenance-planted`) with the handlers, the DTOs and the two
//! loopback layers untouched, so the crate still built and the router
//! still existed — the sabotage is "the door is not where the caller
//! knocks", which is the drift a route census cannot see. `pass: 0
//! fail: 4`:
//!
//! ```text
//! skills_route_serves_the_serving_runtimes_own_registry   :151
//! skills_route_reports_the_active_set_of_that_runtime     :184
//! provenance_route_serves_the_frame_that_runtime_captured :218
//! turn_extras_without_a_runtime_is_the_named_503          :254
//! ```
//!
//! The first three go red on the BODY (`.json()` on a 404 has no
//! `skills` / `provenance` key), which is the assertion that carries
//! the claim. The fourth goes red on its status line (404 vs 503) and
//! that alone would not be a gate (ARCH §18.1) — a routerless daemon
//! 404s too — which is why the assertion under it requires the body to
//! parse and to NAME the missing `Runtime`. Restored: 4/4 green.

use std::sync::Arc;

use sovereign_contracts::skills::{parse_skill_toml, SkillRegistry};
use sovereign_core::runtime::TurnProvenance;
use sovereign_core::setup_config::SetupConfig;
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::turn_extras_http::turn_extras_router;

use crate::common::{
    desktop_services_with_runtime, mesh_admin_services, spawn_router, stub_runtime_with_skills,
    TestProvider,
};

/// A real `CorpusEngine` over a tempdir — `ServingCore.corpus_engine` is
/// not an `Option`, and neither route reads it.
fn engine_at(tmp: &tempfile::TempDir) -> Arc<corpus_engine::CorpusEngine> {
    let indexes = tmp.path().join("indexes");
    let recipes = tmp.path().join("recipes");
    std::fs::create_dir_all(&indexes).unwrap();
    std::fs::create_dir_all(&recipes).unwrap();
    Arc::new(corpus_engine::CorpusEngine::new(
        recipes,
        indexes,
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) })),
    ))
}

/// One skill, built the way production builds one: from a manifest.
/// Hand-constructing a `Skill` here would be a second way to make the
/// object the loader makes, and the loader is the decider.
fn skill(id: &str, name: &str, description: &str) -> sovereign_contracts::skills::Skill {
    parse_skill_toml(&format!(
        r#"
[skill]
id = "{id}"
name = "{name}"
version = "1.0.0"
description = "{description}"
"#
    ))
    .expect("the fixture manifest parses")
}

/// A serving daemon whose runtime carries `registered`, with `active`
/// activated.
fn daemon_with_skills(
    registered: Vec<(&str, &str, &str)>,
    active: &[&str],
) -> (tempfile::TempDir, tempfile::TempDir, Arc<EmbeddedDaemon>) {
    let engine_tmp = tempfile::tempdir().unwrap();
    let engine = engine_at(&engine_tmp);
    let mut registry = SkillRegistry::new();
    for (id, name, desc) in registered {
        registry.register(skill(id, name, desc));
    }
    for id in active {
        registry.activate(id);
    }
    let runtime = stub_runtime_with_skills(Arc::new(TestProvider::new()), Arc::new(registry));
    let root = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        root.path().to_path_buf(),
        SetupConfig::unconfigured(),
        desktop_services_with_runtime(engine, runtime),
    );
    (engine_tmp, root, daemon)
}

/// A provenance frame with only the fields this route has to carry
/// across intact. `TurnProvenance` has no `Default`, deliberately —
/// every field is evidence about a turn and a defaulted one would be a
/// frame nobody captured.
fn frame(conversation_id: &str, system_prompt: &str) -> TurnProvenance {
    TurnProvenance {
        conversation_id: conversation_id.to_string(),
        message_id: "m-1".to_string(),
        captured_at: 1_757_000_000,
        register: "witness".to_string(),
        user_message: "what did you see".to_string(),
        system_prompt: system_prompt.to_string(),
        system_prompt_chars: system_prompt.chars().count(),
        recalled_memories: Vec::new(),
        history_summary: sovereign_core::runtime::HistorySummaryProv {
            total_messages: 4,
            user_count: 2,
            assistant_count: 2,
            sent_to_model: Vec::new(),
        },
        history_recall: Vec::new(),
        temporal_tensions: Vec::new(),
        contradiction: None,
        current_goal: None,
        recent_topic: None,
        last_assistant_excerpt: None,
        model_id: Some("fixture-4b".to_string()),
        max_tokens: Some(2048),
        enable_thinking: Some(false),
        pass_a_ms: Some(41),
        recall_verification: None,
    }
}

#[tokio::test]
async fn skills_route_serves_the_serving_runtimes_own_registry() {
    let (_e, _r, daemon) = daemon_with_skills(
        vec![
            ("inner-work", "Inner Work", "the reflective surface"),
            ("recipe-author", "Recipe Author", "authoring workspace"),
        ],
        &[],
    );
    let addr = spawn_router(turn_extras_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/v1/skills"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let skills = body["skills"].as_array().expect("a `skills` array");
    assert_eq!(skills.len(), 2, "both registered skills cross: {body}");
    let ids: Vec<&str> = skills.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["inner-work", "recipe-author"]);
    assert_eq!(skills[0]["name"], "Inner Work");
    assert_eq!(skills[0]["description"], "the reflective surface");
    // The lowercased debug spelling the desktop used to render
    // privately, decided on this side now (§10.6). An unsigned manifest
    // is `TrustLevel::Unsigned`.
    assert_eq!(
        skills[0]["trust_level"], "unsigned",
        "trust_level is the host's word, lowercased: {body}"
    );
}

#[tokio::test]
async fn skills_route_reports_the_active_set_of_that_runtime() {
    let (_e, _r, daemon) = daemon_with_skills(
        vec![
            ("inner-work", "Inner Work", "the reflective surface"),
            ("recipe-author", "Recipe Author", "authoring workspace"),
        ],
        &["recipe-author"],
    );
    let addr = spawn_router(turn_extras_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/v1/skills"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let skills = body["skills"].as_array().unwrap();
    let active: Vec<&str> = skills
        .iter()
        .filter(|s| s["active"].as_bool().unwrap())
        .map(|s| s["id"].as_str().unwrap())
        .collect();

    // THE fact this route exists for: the active set belongs to the
    // runtime that answers the turn, not to whichever process asked.
    assert_eq!(
        active,
        vec!["recipe-author"],
        "exactly the activated skill reads active: {body}"
    );
}

#[tokio::test]
async fn provenance_route_serves_the_frame_that_runtime_captured() {
    let (_e, _r, daemon) = daemon_with_skills(vec![], &[]);
    // Seed the register the expressive handler writes on a witness turn.
    {
        let runtime = daemon.runtime().expect("the serving commission has one");
        let mut guard = runtime.turn_provenance.write().unwrap();
        guard.insert("conv-a".to_string(), frame("conv-a", "SYSTEM: be careful"));
    }
    let addr = spawn_router(turn_extras_router(daemon)).await;

    let hit: serde_json::Value =
        reqwest::get(format!("http://{addr}/v1/conversations/conv-a/provenance"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let prov = &hit["provenance"];
    assert!(!prov.is_null(), "the captured frame crosses: {hit}");
    assert_eq!(prov["conversation_id"], "conv-a");
    assert_eq!(prov["system_prompt"], "SYSTEM: be careful");
    assert_eq!(prov["system_prompt_chars"], 18);
    assert_eq!(prov["model_id"], "fixture-4b");
    assert_eq!(prov["pass_a_ms"], 41);

    // A conversation with no witness turn is a 200 carrying `null`, not
    // a 404: "no frame yet" is an ANSWER the inner-work pane renders,
    // and folding it into "no such conversation" loses a fact
    // (ARCH §18.3).
    let miss = reqwest::get(format!("http://{addr}/v1/conversations/conv-b/provenance"))
        .await
        .unwrap();
    assert_eq!(miss.status(), 200);
    let miss: serde_json::Value = miss.json().await.unwrap();
    assert!(
        miss["provenance"].is_null(),
        "an uncaptured conversation answers null, not an error: {miss}"
    );
}

#[tokio::test]
async fn turn_extras_without_a_runtime_is_the_named_503() {
    let root = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        root.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    let addr = spawn_router(turn_extras_router(daemon)).await;

    for path in ["/v1/skills", "/v1/conversations/conv-a/provenance"] {
        let resp = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(
            resp.status(),
            503,
            "{path}: a mesh-admin daemon serves no turns"
        );
        // The status alone is not the gate — a routerless daemon is
        // also "not 200". The body must NAME the missing object.
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("Runtime"),
            "{path}: the 503 must say WHICH object is absent, got {body}"
        );
    }
}
