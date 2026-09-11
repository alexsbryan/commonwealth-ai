// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three D8 surfaces this batch opens: living governance
//! (`governance_http`), the daemon's own MCP configuration
//! (`mcp_config_http`), and the recipe-author project COMPOSITION
//! (`recipe_project_http`).
//!
//! The fourth family in the batch — `POST /v1/insights/by-id` — is tested
//! beside its five siblings in `loopback_parity.rs`, which already carries
//! the `InsightService` fixture. A second copy of that fixture here would
//! be the twin this campaign deletes; `d6_surface_e2e` made the same call
//! for the sink route and the reason has not changed.
//!
//! Exercised against real artefacts — a real atlas dir with a real
//! `governance_oplog.jsonl`, a real `notes.db` and `features.db`, a real
//! `recipes/` tree — opened the way production opens them. The fault this
//! rung exists to remove is TWO PROCESSES appending to one JSONL log with
//! one mutex each, and a stubbed oplog cannot exhibit it.
//!
//! # Red-watch (2026-09-10, run, not asserted)
//!
//! All three routers were emptied — `Router::new()` with its two layers
//! returned in place of the route list, handlers and DTOs left where they
//! were so the suite still built — and every case below re-run against the
//! routerless daemon. ALL FIFTEEN failed, `pass: 0 fail: 15`:
//!
//! ```text
//! governance_view_serves_the_daemons_own_atlas_dir       :198  404 vs 200
//! governance_view_on_an_unenriched_corpus_…              :241  decode EOF
//! governance_resolve_then_undo_round_trips_…             :278  404 vs 200
//! governance_accept_refuses_an_empty_rationale           :350  404 vs 400
//! governance_adjudicating_an_unknown_tension_…           :394  decode EOF
//! governance_seed_is_idempotent_across_calls             :425  decode EOF
//! governance_write_recipe_lands_under_…                  :470  404 vs 200
//! governance_without_a_corpus_engine_is_the_named_503    :503  404 vs 503
//! mcp_servers_lists_the_config_and_folds_…               :611  404 vs 200
//! mcp_servers_without_a_mount_says_so_…                  :690  decode EOF
//! mcp_test_connection_reports_an_unreachable_server_…    :724  404 vs 502
//! mcp_token_round_trips_and_a_blank_token_clears_it      :781  decode EOF
//! recipe_project_create_then_list_then_dashboard_…       :850  404 vs 201
//! recipe_project_save_toml_refuses_a_broken_recipe_…     :963  decode EOF
//! recipe_projects_without_the_feature_store_…           :1049  404 vs 503
//! ```
//!
//! The two that ASSERT a 404 need their caveat stated here and not only in
//! a log. `governance_view_on_an_unenriched_corpus_…` and
//! `governance_adjudicating_an_unknown_tension_…` both expect 404, and a
//! routerless daemon 404s too — so their STATUS lines passed under
//! sabotage. Both went red on the line AFTER: the body must parse and must
//! name the path or the id, and an empty 404 body cannot. The status alone
//! is not a gate (ARCH §18.1), which is why every absence case here is
//! written with a body assertion under it.

use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Claim};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType, EdgesFile};
use corpus_engine::enrichment::pipeline::atlas::{
    ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
};
use corpus_engine::enrichment::GovernanceOpKind;
use corpus_engine::oplog::{Op, Oplog};
use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use sovereign_contracts::mcp_config::{McpAuthConfig, McpServerConfig, McpTransportConfig};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::types::{Effect, Idempotency, Latency, Scope, StepOutput, ToolDescriptor};
use sovereign_core::{Tool, ToolContext, ToolRegistry};
use sovereign_mesh::daemon::EmbeddedDaemon;
use sovereign_mesh::governance_http::governance_router;
use sovereign_mesh::mcp_config_http::mcp_config_router;
use sovereign_mesh::recipe_project_http::recipe_project_router;
use sovereign_store::recipe_project_store::RecipeProjectStore;

use crate::common;
use crate::common::spawn_router;

/// Fixture construction: a failure here is a broken fixture, not a
/// finding, and must abort loudly. The scoped allow is
/// `atlas_surface_e2e`'s.
#[allow(clippy::unwrap_used)]
mod fixture {
    use super::*;
    pub use crate::common::engine_at;

    pub const CORPUS: &str = "house-rules";

    /// A daemon whose index root, recipes dir, `notes.db` and
    /// `features.db` all live under one temp dir — the same relationship
    /// production has, which is what makes `engine.index_dir()` the right
    /// thing for the handlers to resolve against.
    pub struct Fx {
        pub daemon: Arc<EmbeddedDaemon>,
        pub engine: Arc<CorpusEngine>,
        pub _tmp: tempfile::TempDir,
    }

    /// A serving daemon with both stores and an empty tool registry.
    pub fn daemon_with_stores(with_features: bool) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(&tmp);
        let notes = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let features = with_features
            .then(|| Arc::new(RecipeProjectStore::open(&tmp.path().join("features.db")).unwrap()));
        let daemon = EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            SetupConfig::unconfigured(),
            common::desktop_services_with_note_and_feature_stores(
                Arc::clone(&engine),
                notes,
                features,
            ),
        );
        Fx {
            daemon,
            engine,
            _tmp: tmp,
        }
    }

    /// The two-rule, one-tension governance atlas the corpus-engine view
    /// tests use, laid down under the DAEMON's own index dir.
    ///
    /// Both rules are asserted by the log, so the view reports two active
    /// rules and one OPEN tension — the state a steward opens the panel to.
    pub fn write_atlas(engine: &CorpusEngine, corpus: &str) -> std::path::PathBuf {
        let dir = engine.index_dir().join(corpus).join("atlas");
        std::fs::create_dir_all(&dir).unwrap();

        let make_claim = |n: usize, content: &str| Claim {
            attributes: Default::default(),
            subject: None,
            id: AtomId::claim(n),
            content: content.into(),
            discourse_act: DiscourseAct::Enact,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Contextual,
            evidence: vec![ChunkRef::new(format!("sec_{n:05}"), None)],
            quotable_excerpt: None,
            attributed_to: Some(AtomId::entity(1)),
            confidence: None,
            anchor: None,
            claim_kind: Some("forbids".into()),
            concession_outcome: None,
            evidence_kind: None,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let atoms = AtomsFile::new(vec![
            AtomEnvelope::Claim(make_claim(1, "quiet hours start at 22:00")),
            AtomEnvelope::Claim(make_claim(2, "quiet hours start at 23:00")),
        ]);
        std::fs::write(dir.join("atoms.json"), serde_json::to_vec(&atoms).unwrap()).unwrap();

        let edges = EdgesFile::new(vec![Edge {
            id: EdgeId::new(1),
            edge_type: EdgeType::Tension,
            source: AtomId::claim(1),
            target: AtomId::claim(2),
            evidence: Vec::new(),
            trigger_event: None,
            sub_question: Some("which hour governs?".into()),
            confidence: 0.8,
            provenance: EdgeProvenance::Derived,
        }]);
        std::fs::write(dir.join("edges.json"), serde_json::to_vec(&edges).unwrap()).unwrap();

        let log = Oplog::<GovernanceOpKind>::new(&dir);
        for n in 1..=2 {
            log.append(&Op::new(
                GovernanceOpKind::AssertRule {
                    rule: AtomId::claim(n),
                    source_doc: None,
                },
                1_000 + n as i64,
                "seed",
            ))
            .unwrap();
        }
        dir
    }

    pub fn tension_id() -> String {
        EdgeId::new(1).as_str().to_string()
    }
}

// ── Governance (D8) ───────────────────────────────────────────────

/// The whole panel payload comes off the DAEMON's atlas dir: the joined
/// view, the decision metadata the oplog carries, and the staleness flag.
///
/// The decisions map is asserted, not just the rule count: it is the half
/// a `GovernanceView` alone cannot carry (the rationale lives on the op,
/// not on the graph), and it is the half a handler could most plausibly
/// forget to join.
///
/// RED under sabotage at the `status` line: 404 vs 200.
#[tokio::test]
async fn governance_view_serves_the_daemons_own_atlas_dir() {
    let fx = fixture::daemon_with_stores(false);
    fixture::write_atlas(&fx.engine, fixture::CORPUS);
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;

    let resp = reqwest::get(format!(
        "http://{addr}/internal/governance/{}/view",
        fixture::CORPUS
    ))
    .await
    .expect("governance_router reachable");
    assert_eq!(resp.status(), 200, "an enriched corpus has a view");
    let body: serde_json::Value = resp.json().await.expect("the view decodes");

    assert_eq!(
        body["view"]["rules"].as_array().map(Vec::len),
        Some(2),
        "both Claim atoms are governed rules"
    );
    assert_eq!(
        body["view"]["tensions"].as_array().map(Vec::len),
        Some(1),
        "the one Tension edge surfaces"
    );
    // The two AssertRule ops, joined off the oplog by op id.
    let decisions = body["decisions"].as_object().expect("a decisions map");
    assert_eq!(decisions.len(), 2, "one entry per op in the log");
    let first = decisions.values().next().expect("at least one op");
    assert_eq!(
        first["actor"].as_str(),
        Some("seed"),
        "the baseline is seeded, not human-authored"
    );
    assert_eq!(
        body["docs_changed_since_build"].as_bool(),
        Some(false),
        "no chapters.json means the staleness heuristic reads not-stale"
    );
}

/// A corpus with no atlas is a 404 that NAMES the path, never an empty
/// view — "not enriched yet" and "no conflicts" drive different banners.
///
/// RED under sabotage at the BODY line, not the status: a routerless
/// daemon 404s too, and what it cannot do is name the missing directory.
#[tokio::test]
async fn governance_view_on_an_unenriched_corpus_is_a_404_naming_the_path() {
    let fx = fixture::daemon_with_stores(false);
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;

    let resp = reqwest::get(format!(
        "http://{addr}/internal/governance/never-built/view"
    ))
    .await
    .expect("governance_router reachable");
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.expect("the 404 carries a reason");
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(
        msg.contains("never-built") && msg.contains("not been enriched"),
        "the 404 must name the corpus and the reason, got {msg:?}"
    );
}

/// Resolve, then undo, both landing in the daemon's own oplog and both
/// visible in the next view read.
///
/// The write is asserted by RE-READING through the view route, not by
/// trusting the op ids the write answered: a handler that minted plausible
/// ids without appending would pass a status check and a length check.
///
/// RED under sabotage at the resolve `status` line: 404 vs 200.
#[tokio::test]
async fn governance_resolve_then_undo_round_trips_through_the_oplog() {
    let fx = fixture::daemon_with_stores(false);
    fixture::write_atlas(&fx.engine, fixture::CORPUS);
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;
    let http = reqwest::Client::new();
    let tension = fixture::tension_id();
    let base = format!(
        "http://{addr}/internal/governance/{}/tensions/{tension}",
        fixture::CORPUS
    );

    let resp = http
        .post(format!("{base}/resolve"))
        .json(&serde_json::json!({
            "keep_rule_id": AtomId::claim(2).as_str(),
            "rationale": "the 2026 meeting moved it to 23:00",
        }))
        .send()
        .await
        .expect("governance_router reachable");
    assert_eq!(resp.status(), 200, "a resolve answers its op ids");
    let body: serde_json::Value = resp.json().await.unwrap();
    let op_ids = body["op_ids"].as_array().expect("op_ids is a list");
    assert_eq!(
        op_ids.len(),
        2,
        "a resolve appends the Supersede AND the ResolveTension that records it"
    );

    // The decision is in the DAEMON's log, not merely in the response.
    let view: serde_json::Value = reqwest::get(format!(
        "http://{addr}/internal/governance/{}/view",
        fixture::CORPUS
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let disposition = &view["view"]["tensions"][0]["disposition"];
    assert!(
        serde_json::to_string(disposition)
            .unwrap()
            .contains("esolved"),
        "the tension reads Resolved after the append, got {disposition:?}"
    );

    // Undo reverts the whole bundle, and the tension is Open again.
    let resp = http
        .post(format!("{base}/undo"))
        .send()
        .await
        .expect("governance_router reachable");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["op_id"].as_str().is_some_and(|s| !s.is_empty()),
        "an undo answers the revert op id"
    );

    let view: serde_json::Value = reqwest::get(format!(
        "http://{addr}/internal/governance/{}/view",
        fixture::CORPUS
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let disposition = &view["view"]["tensions"][0]["disposition"];
    assert!(
        serde_json::to_string(disposition).unwrap().contains("pen"),
        "the revert puts the conflict back on the agenda, got {disposition:?}"
    );
}

/// An accepted conflict with no rationale is refused at 400 — the one
/// adjudication a later reader could not act on.
///
/// This is the check with a NAMED failing input (ARCH §18.1): the empty
/// string. Sabotage of the route makes it 404; sabotage of the guard alone
/// makes it 200.
#[tokio::test]
async fn governance_accept_refuses_an_empty_rationale() {
    let fx = fixture::daemon_with_stores(false);
    fixture::write_atlas(&fx.engine, fixture::CORPUS);
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;
    let tension = fixture::tension_id();

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/governance/{}/tensions/{tension}/accept",
            fixture::CORPUS
        ))
        .json(&serde_json::json!({ "rationale": "   " }))
        .send()
        .await
        .expect("governance_router reachable");
    assert_eq!(
        resp.status(),
        400,
        "a blank rationale is the caller's error"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("why both rules stand"),
        "the 400 says what is missing"
    );

    // A dismissal, by contrast, takes no note at all — that is the
    // distinction between the two acts and it must survive the crossing.
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/governance/{}/tensions/{tension}/dismiss",
            fixture::CORPUS
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("governance_router reachable");
    assert_eq!(resp.status(), 200, "a dismissal needs no rationale");
}

/// An unknown tension id is a 404 that repeats the id back.
///
/// RED under sabotage at the BODY line: an empty 404 body cannot name the
/// id the caller sent.
#[tokio::test]
async fn governance_adjudicating_an_unknown_tension_is_a_404_naming_it() {
    let fx = fixture::daemon_with_stores(false);
    fixture::write_atlas(&fx.engine, fixture::CORPUS);
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/governance/{}/tensions/edge_99999/accept",
            fixture::CORPUS
        ))
        .json(&serde_json::json!({ "rationale": "both stand" }))
        .send()
        .await
        .expect("governance_router reachable");
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.expect("the 404 carries a reason");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("edge_99999"),
        "the 404 names the conflict the caller asked for"
    );
}

/// Seeding is idempotent: the second call asserts nothing new. That is
/// what makes it safe to re-run after every rebuild, and it is a property
/// a handler that simply re-appended would fail.
#[tokio::test]
async fn governance_seed_is_idempotent_across_calls() {
    let fx = fixture::daemon_with_stores(false);
    let dir = fixture::write_atlas(&fx.engine, fixture::CORPUS);
    // Start from an atlas whose rules are NOT yet asserted, so the first
    // seed has work to do.
    std::fs::remove_file(dir.join("governance_oplog.jsonl")).expect("fixture oplog exists");
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;
    let http = reqwest::Client::new();
    let url = format!("http://{addr}/internal/governance/{}/seed", fixture::CORPUS);

    let first: serde_json::Value = http
        .post(&url)
        .send()
        .await
        .expect("governance_router reachable")
        .json()
        .await
        .unwrap();
    assert_eq!(
        first["seeded"].as_u64(),
        Some(2),
        "one AssertRule per Claim atom"
    );

    let second: serde_json::Value = http
        .post(&url)
        .send()
        .await
        .expect("governance_router reachable")
        .json()
        .await
        .unwrap();
    assert_eq!(
        second["seeded"].as_u64(),
        Some(0),
        "a re-seed skips rules the log already governs"
    );
}

/// The recipe lands under the DAEMON's `recipes_dir()` and round-trips
/// through the parser as a custom-ontology recipe.
///
/// The directory assertion is the substance of the route: a desktop-side
/// write in attach mode lands where the daemon never looks, and the corpus
/// then enriches down the literary pipeline with no error anywhere.
#[tokio::test]
async fn governance_write_recipe_lands_under_the_daemons_own_recipes_dir() {
    let fx = fixture::daemon_with_stores(false);
    let addr = spawn_router(governance_router(Arc::clone(&fx.daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/internal/governance/{}/recipe",
            fixture::CORPUS
        ))
        .json(&serde_json::json!({
            "display_name": "House Rules",
            "source_path": "/tmp/house-rules",
        }))
        .send()
        .await
        .expect("governance_router reachable");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let path = std::path::PathBuf::from(body["path"].as_str().expect("a path"));

    assert!(
        path.starts_with(fx.engine.recipes_dir()),
        "the recipe must land under the DAEMON's recipes dir ({}), got {}",
        fx.engine.recipes_dir().display(),
        path.display()
    );
    assert!(path.exists(), "the file is really on disk");
    let recipe = corpus_engine::Recipe::from_file(&path).expect("the template parses");
    assert!(
        recipe.custom_ontology().is_some(),
        "without a custom ontology the corpus enriches down the literary pipeline"
    );
}

/// Every governance route on a daemon with no corpus engine is a named
/// 503 — a different fact from an unmounted router's 404.
#[tokio::test]
async fn governance_without_a_corpus_engine_is_the_named_503() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        common::mesh_admin_services(),
    );
    let addr = spawn_router(governance_router(daemon)).await;

    let resp = reqwest::get(format!("http://{addr}/internal/governance/anything/view"))
        .await
        .expect("governance_router reachable");
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.expect("the 503 carries a reason");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no corpus engine"),
        "the 503 names what is missing"
    );
}

// ── MCP configuration (D8) ────────────────────────────────────────

struct StubTool {
    descriptor: ToolDescriptor,
}

#[async_trait::async_trait]
impl Tool for StubTool {
    fn descriptor(&self) -> ToolDescriptor {
        self.descriptor.clone()
    }
    async fn execute(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput, sovereign_core::Error> {
        Ok(StepOutput::Text("stub".into()))
    }
    fn required_permissions(&self) -> Vec<sovereign_core::types::Permission> {
        Vec::new()
    }
}

fn stub_tool(id: &str) -> Box<dyn Tool> {
    Box::new(StubTool {
        descriptor: ToolDescriptor {
            id: id.into(),
            name: id.into(),
            description: format!("stub for {id}"),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: None,
        },
    })
}

fn http_server(name: &str, url: &str, bearer: bool) -> McpServerConfig {
    McpServerConfig {
        name: name.into(),
        description: Some(format!("{name} test server")),
        enabled: true,
        transport: McpTransportConfig::Http {
            url: url.into(),
            auth: if bearer {
                McpAuthConfig::Bearer
            } else {
                McpAuthConfig::None
            },
        },
        global: true,
    }
}

/// The configured servers come off the daemon's OWN `SetupConfig`, and
/// each one's live count is folded off the tool ids in the daemon's OWN
/// registry — the registry an answer actually plans against, which is the
/// half `state.mcp_servers` could never be for an attached desktop.
///
/// The `vision` server's count is the load-bearing assertion: two of the
/// four registered tools carry its prefix, one carries another server's,
/// and one is a native tool with no `mcp_` prefix at all. A fold that
/// counted the registry, or counted any `mcp_` id, passes neither.
///
/// RED under sabotage at the `status` line: 404 vs 200.
#[tokio::test]
async fn mcp_servers_lists_the_config_and_folds_the_live_tool_counts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine = fixture::engine_at(&tmp);
    let notes = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).expect("notes.db"));

    let mut tools = ToolRegistry::new();
    tools.register(stub_tool("mcp_vision_describe"));
    tools.register(stub_tool("mcp_vision_ocr"));
    tools.register(stub_tool("mcp_weather_forecast"));
    tools.register(stub_tool("corpus_search"));

    let mut cfg = SetupConfig::unconfigured();
    cfg.mcp_servers = vec![
        http_server("vision", "https://vision.example/mcp", true),
        http_server("weather", "https://weather.example/mcp", false),
        http_server("silent", "https://silent.example/mcp", false),
    ];

    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        cfg,
        common::desktop_services_with_tool_registry(engine, notes, None, Arc::new(tools)),
    );
    let addr = spawn_router(mcp_config_router(daemon)).await;

    let resp = reqwest::get(format!("http://{addr}/v1/mcp/servers"))
        .await
        .expect("mcp_config_router reachable");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.expect("the listing decodes");

    let servers = body["servers"].as_array().expect("a servers array");
    assert_eq!(servers.len(), 3, "every configured server is listed");
    let by_name = |n: &str| {
        servers
            .iter()
            .find(|s| s["name"].as_str() == Some(n))
            .unwrap_or_else(|| panic!("`{n}` is in the listing"))
            .clone()
    };

    let vision = by_name("vision");
    assert_eq!(
        vision["live_tool_count"].as_u64(),
        Some(2),
        "only the two mcp_vision_* tools count toward vision"
    );
    assert_eq!(vision["bearer"].as_bool(), Some(true));
    assert_eq!(
        vision["token_env"].as_str(),
        Some("SOVEREIGN_MCP_TOKEN_VISION"),
        "the headless override is named, so a CI operator can set it"
    );
    assert_eq!(
        by_name("weather")["live_tool_count"].as_u64(),
        Some(1),
        "one tool, and the native corpus_search is not one of them"
    );
    assert_eq!(
        by_name("silent")["live_tool_count"].as_u64(),
        Some(0),
        "a configured server that registered nothing counts zero — and is \
         NOT reported as disconnected, which this host cannot know"
    );
    assert_eq!(by_name("weather")["bearer"].as_bool(), Some(false));
    assert!(
        by_name("weather")["token_env"].is_null(),
        "a no-auth server names no token env var"
    );

    assert_eq!(body["mount"]["mounted"].as_bool(), Some(true));
    assert_eq!(
        body["mount"]["total_tools"].as_u64(),
        Some(4),
        "the denominator is the whole registry, MCP and native alike"
    );
    let reason = body["mount"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("connect status") && reason.contains("McpServerManager"),
        "the absence of a connect flag is stated in words on every response, got {reason:?}"
    );
}

/// A daemon with NO tool mount answers 200 and says `mounted: false`.
///
/// The distinction is the point: every `live_tool_count` is zero either
/// way, and only `mount.mounted` separates "these servers registered
/// nothing" from "there was nothing to count against" (ARCH §18.3).
#[tokio::test]
async fn mcp_servers_without_a_mount_says_so_rather_than_reporting_zeros() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine = fixture::engine_at(&tmp);
    let mut cfg = SetupConfig::unconfigured();
    cfg.mcp_servers = vec![http_server("vision", "https://vision.example/mcp", true)];

    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        cfg,
        common::desktop_services_with_engine(engine),
    );
    let addr = spawn_router(mcp_config_router(daemon)).await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/v1/mcp/servers"))
        .await
        .expect("mcp_config_router reachable")
        .json()
        .await
        .expect("the listing decodes");

    assert_eq!(
        body["mount"]["mounted"].as_bool(),
        Some(false),
        "no mount is an ANSWER to this question, not a failure to answer it"
    );
    assert_eq!(body["mount"]["total_tools"].as_u64(), Some(0));
    assert_eq!(
        body["servers"][0]["live_tool_count"].as_u64(),
        Some(0),
        "zero, and `mounted: false` is what tells the caller why"
    );
}

/// A probe against an address nothing serves is a 502 carrying the
/// remote's fault, not a 500 blaming this host.
#[tokio::test]
async fn mcp_test_connection_reports_an_unreachable_server_as_a_gateway_error() {
    let fx = fixture::daemon_with_stores(false);
    let addr = spawn_router(mcp_config_router(Arc::clone(&fx.daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/mcp/servers/test"))
        .json(&serde_json::json!({
            // Port 1 on loopback: nothing listens, and the connect fails
            // fast rather than hanging on a routed-but-dead address.
            "name": "nowhere",
            "url": "http://127.0.0.1:1/mcp",
            "bearer": false,
        }))
        .send()
        .await
        .expect("mcp_config_router reachable");
    assert_eq!(
        resp.status(),
        502,
        "an unreachable MCP server is the REMOTE's failure — a 500 would \
         send the operator to this host's logs"
    );
    let body: serde_json::Value = resp.json().await.expect("the 502 carries a reason");
    assert!(
        !body["error"].as_str().unwrap_or_default().is_empty(),
        "the probe failure is named, not swallowed"
    );

    // An empty URL never leaves the host.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/mcp/servers/test"))
        .json(&serde_json::json!({ "name": "n", "url": "  ", "bearer": false }))
        .send()
        .await
        .expect("mcp_config_router reachable");
    assert_eq!(resp.status(), 400, "an empty URL is the caller's error");
}

/// A bearer secret round-trips through the routes and shows up in the
/// listing's `has_token`, and a BLANK token clears it.
///
/// `HOME` is redirected for the duration: `secret_store` resolves
/// `~/.svrnmesh/secrets/mcp` from the rebrand root, and a test that wrote
/// into the developer's real secrets dir would be a defect of its own.
/// The guard is `project_http`'s, process-global and mutex-held.
#[tokio::test]
async fn mcp_token_round_trips_and_a_blank_token_clears_it() {
    let home = tempfile::tempdir().expect("tempdir");
    let _guard = HomeGuard::set(home.path());

    let tmp = tempfile::tempdir().expect("tempdir");
    let engine = fixture::engine_at(&tmp);
    let notes = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).expect("notes.db"));
    let mut cfg = SetupConfig::unconfigured();
    cfg.mcp_servers = vec![http_server("vision", "https://vision.example/mcp", true)];
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        cfg,
        common::desktop_services_with_tool_registry(
            engine,
            notes,
            None,
            Arc::new(ToolRegistry::new()),
        ),
    );
    let addr = spawn_router(mcp_config_router(daemon)).await;
    let http = reqwest::Client::new();
    let listing = || async {
        let b: serde_json::Value = reqwest::get(format!("http://{addr}/v1/mcp/servers"))
            .await
            .expect("mcp_config_router reachable")
            .json()
            .await
            .expect("the listing decodes");
        b["servers"][0]["has_token"].as_bool()
    };

    assert_eq!(listing().await, Some(false), "no token to start with");

    let resp = http
        .put(format!("http://{addr}/v1/mcp/servers/vision/token"))
        .json(&serde_json::json!({ "token": "sk-test-123" }))
        .send()
        .await
        .expect("mcp_config_router reachable");
    assert_eq!(resp.status(), 204, "a stored secret answers no content");
    assert_eq!(
        listing().await,
        Some(true),
        "the listing reads the secret store, not the request that wrote it"
    );

    // A BLANK token clears — the secret store's own contract, crossed
    // rather than re-decided at the route.
    let resp = http
        .put(format!("http://{addr}/v1/mcp/servers/vision/token"))
        .json(&serde_json::json!({ "token": "   " }))
        .send()
        .await
        .expect("mcp_config_router reachable");
    assert_eq!(resp.status(), 204);
    assert_eq!(listing().await, Some(false), "a blank token clears it");

    // DELETE on an already-empty slot is still 204: nothing to delete is
    // not a failed delete.
    let resp = http
        .delete(format!("http://{addr}/v1/mcp/servers/vision/token"))
        .send()
        .await
        .expect("mcp_config_router reachable");
    assert_eq!(resp.status(), 204);
}

// ── Recipe-author projects (D8) ───────────────────────────────────

/// Create → list → dashboard, over ONE composition on ONE host.
///
/// The create is asserted by reading it back through the LIST route and
/// then through the DASHBOARD route, because the fault this rung removes
/// is a project whose row is in one place and whose summary is in
/// another: a handler that provisioned the row and skipped the artifact
/// tree would answer a plausible entry and then fail the dashboard.
///
/// RED under sabotage at the create `status` line: 404 vs 201.
#[tokio::test]
async fn recipe_project_create_then_list_then_dashboard_over_one_composition() {
    let home = tempfile::tempdir().expect("tempdir");
    let _guard = HomeGuard::set(home.path());

    let fx = fixture::daemon_with_stores(true);
    let addr = spawn_router(recipe_project_router(Arc::clone(&fx.daemon))).await;
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("http://{addr}/v1/recipe-projects"))
        .json(&serde_json::json!({
            "title": "Roman coin hoards",
            "charter_md": "Catalogue the hoards and date them.",
        }))
        .send()
        .await
        .expect("recipe_project_router reachable");
    assert_eq!(resp.status(), 201, "a provision answers CREATED");
    let created: serde_json::Value = resp.json().await.expect("the entry decodes");
    let feature_id = created["feature_id"]
        .as_str()
        .expect("the host minted an id")
        .to_string();
    assert_eq!(
        created["artifact_kind"].as_str(),
        Some("recipe"),
        "an omitted kind defaults to recipe, as it did before the tag existed"
    );

    // The artifact tree really exists on THIS host, under the redirected
    // root — the half a store-only route could not do.
    let project_dir = home
        .path()
        .join(".svrnmesh")
        .join("recipe-projects")
        .join(&feature_id);
    assert!(
        project_dir.exists(),
        "the project directory is laid down at {}",
        project_dir.display()
    );

    // The LIST route sees it.
    let listed: serde_json::Value = reqwest::get(format!("http://{addr}/v1/recipe-projects"))
        .await
        .expect("recipe_project_router reachable")
        .json()
        .await
        .expect("the listing decodes");
    let rows = listed.as_array().expect("a list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["feature_id"].as_str(), Some(feature_id.as_str()));
    assert_eq!(
        rows[0]["charter_excerpt"].as_str(),
        Some("Catalogue the hoards and date them."),
        "a short charter crosses whole; the ellipsis is for long ones"
    );

    // The DASHBOARD route composes over both stores plus the tree.
    let dash: serde_json::Value = reqwest::get(format!(
        "http://{addr}/v1/recipe-projects/{feature_id}/dashboard"
    ))
    .await
    .expect("recipe_project_router reachable")
    .json()
    .await
    .expect("the dashboard decodes");
    assert_eq!(dash["title"].as_str(), Some("Roman coin hoards"));
    assert!(
        dash["recipe_toml"].is_null(),
        "nothing drafted yet, so there is no TOML to read"
    );
    assert_eq!(
        dash["validation"]["no_recipe"].as_bool(),
        Some(true),
        "`nothing drafted` is a VERDICT this host reaches without a parser"
    );
    assert!(
        dash["validation_unavailable"].is_null(),
        "a recipe project is judgeable here, so no unavailability is reported"
    );
    assert!(
        dash["checkpoints"].as_array().is_some(),
        "a fresh project's checkpoint list is present and may be empty"
    );

    // The prelude renders off the same composition.
    let prelude: serde_json::Value = reqwest::get(format!(
        "http://{addr}/v1/recipe-projects/{feature_id}/prelude"
    ))
    .await
    .expect("recipe_project_router reachable")
    .json()
    .await
    .expect("the prelude decodes");
    let text = prelude["prelude"].as_str().unwrap_or_default();
    assert!(
        text.starts_with("[Project state]") && text.ends_with("[Partner says]\n"),
        "the block is the agent's splice envelope, got {:?}",
        &text.chars().take(40).collect::<String>()
    );
    assert!(
        text.contains("no recipe drafted yet"),
        "the agent is told explicitly that there is nothing to read"
    );
}

/// An unparseable recipe is answered with the verdict and NOTHING is
/// written — validate-first crosses with the write.
///
/// The on-disk assertion is the gate: a handler that wrote first and
/// validated second would return the same body and leave a broken
/// `recipe.toml` behind, breaking both the build and the next prelude.
#[tokio::test]
async fn recipe_project_save_toml_refuses_a_broken_recipe_and_writes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let _guard = HomeGuard::set(home.path());

    let fx = fixture::daemon_with_stores(true);
    let addr = spawn_router(recipe_project_router(Arc::clone(&fx.daemon))).await;
    let http = reqwest::Client::new();

    let created: serde_json::Value = http
        .post(format!("http://{addr}/v1/recipe-projects"))
        .json(&serde_json::json!({ "title": "T", "charter_md": "C" }))
        .send()
        .await
        .expect("recipe_project_router reachable")
        .json()
        .await
        .expect("the entry decodes");
    let feature_id = created["feature_id"].as_str().expect("an id").to_string();

    // No artifact linked yet: the host refuses rather than inventing an id.
    let resp = http
        .put(format!(
            "http://{addr}/v1/recipe-projects/{feature_id}/toml"
        ))
        .json(&serde_json::json!({ "edited_toml": "[corpus]\nid = \"x\"\n" }))
        .send()
        .await
        .expect("recipe_project_router reachable");
    assert_eq!(resp.status(), 400, "there is no recipe to overwrite yet");
    let body: serde_json::Value = resp.json().await.expect("the 400 carries a reason");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("draft one with the agent"),
        "the refusal tells the caller what to do next"
    );

    // Link an artifact by laying one down and asking the host to notice it
    // — the same path an agent turn takes.
    let recipes = home.path().join(".svrnmesh").join("recipes").join("r-1");
    std::fs::create_dir_all(&recipes).expect("recipes dir");
    std::fs::write(recipes.join("recipe.toml"), "# placeholder\n").expect("seed artifact");
    let linked: serde_json::Value = http
        .post(format!(
            "http://{addr}/v1/recipe-projects/{feature_id}/link-recent-artifact"
        ))
        .json(&serde_json::json!({ "since_unix": 0 }))
        .send()
        .await
        .expect("recipe_project_router reachable")
        .json()
        .await
        .expect("the link decodes");
    assert_eq!(
        linked["artifact_id"].as_str(),
        Some("r-1"),
        "the host links the artifact written in the window"
    );

    let before = std::fs::read_to_string(recipes.join("recipe.toml")).expect("artifact readable");

    let resp = http
        .put(format!(
            "http://{addr}/v1/recipe-projects/{feature_id}/toml"
        ))
        .json(&serde_json::json!({ "edited_toml": "this is not toml = = =" }))
        .send()
        .await
        .expect("recipe_project_router reachable");
    assert_eq!(
        resp.status(),
        200,
        "a negative verdict is a successful validation, not a failed request"
    );
    let report: serde_json::Value = resp.json().await.expect("the report decodes");
    assert_eq!(report["ok"].as_bool(), Some(false));
    assert!(
        report["errors"].as_array().map(Vec::len).unwrap_or(0) > 0,
        "a failing parse always ships at least one message: {report}"
    );

    let after = std::fs::read_to_string(recipes.join("recipe.toml")).expect("artifact readable");
    assert_eq!(
        before, after,
        "an artifact that does not parse is NEVER persisted"
    );
    assert!(
        !recipes.join("recipe.toml.part").exists(),
        "and no half-written .part is left behind"
    );
}

/// A daemon with no `features.db` answers 503 naming THAT store — not
/// `notes.db`, and not a generic "workspace unavailable".
///
/// The two files are opened by different code with different failure
/// modes; one message covering both sends an operator to the wrong file.
#[tokio::test]
async fn recipe_projects_without_the_feature_store_is_the_named_503() {
    let fx = fixture::daemon_with_stores(false);
    let addr = spawn_router(recipe_project_router(Arc::clone(&fx.daemon))).await;

    let resp = reqwest::get(format!("http://{addr}/v1/recipe-projects"))
        .await
        .expect("recipe_project_router reachable");
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.expect("the 503 carries a reason");
    let msg = body["error"].as_str().unwrap_or_default();
    assert!(
        msg.contains("features.db"),
        "the 503 names WHICH store is missing, got {msg:?}"
    );
}

// ── HOME redirection ──────────────────────────────────────────────
//
// `recipe_author::{projects_root_dir, local_recipes_dir}` and
// `mcp::secret_store` all resolve from `sovereign_contracts::rebrand::
// svrnmesh_root()`, which reads `HOME`. Copied from `project_http`'s own
// tests, mutex and all: `set_var` is process-global, and two tests
// swapping HOME concurrently would poison whichever ran second.

static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct HomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        let lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self { _lock: lock, prev }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
