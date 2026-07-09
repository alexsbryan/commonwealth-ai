// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build a `ToolRegistry` with enough infrastructure for agent-driven
//! `svrn tools <cmd>` invocations.
//!
//! Distilled from `project_cmd::cmd_serve` — keeps the tool
//! construction identical so both surfaces produce the same
//! `ToolDescriptor`s (important for the agent-facing manifest) but
//! drops the MCP HTTP server, the FS watcher coordinator, and the
//! long-running tokio tasks. The goal is fast startup: opening the
//! stores + constructing the tool instances takes tens of
//! milliseconds, not seconds.
//!
//! Tools whose *execute* path needs a live watcher (e.g. `lint_status`
//! without a running `svrn daemon`) still register cleanly —
//! they just report `never_run` / `stale` when called, which is
//! exactly the existing behaviour.
//!
//! Per ARCH_PRINCIPLES §3.2, this is the seam where a shared
//! registry-setup helper will eventually live. The
//! highest-divergence-risk piece — the MCP allowlist + alias map
//! — was centralised into [`sovereign_tools::mcp_surface`] in the
//! Phase 2 refactor, so the daemon's `mcp_router` and the
//! standalone `routes_mcp` server now agree on exactly the same
//! exposed surface without a manual sync.
//!
//! What remains duplicated: the tool construction calls below
//! (`tools.register(Box::new(...))` for each tool) are mirrored in
//! `project_cmd::cmd_serve`. Both paths build the same
//! `ToolRegistry` shape; if either grows new tools without the
//! other, descriptors drift. Extracting that into a shared
//! `sovereign-tools::registry_builder` is tracked as a follow-up.
//! The path-resolution helpers / SCIP loader prerequisite landed
//! with the `sovereign-cli-shared` crate split — `load_merged_graph`,
//! `find_sovereign_dir`, and `default_data_dir` now live there and
//! are imported below.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;

use corpus_engine::{CorpusEngine, EmbedFn, LintResultStore, TestResultStore};
use corpus_engine_atos::FeatureStore;
use corpus_engine_notes::{NoteStore, ProjectDocsStore};
use sovereign_cli_shared::{
    dirs::default_data_dir, repo::find_sovereign_dir, scip::load_merged_graph,
};
use sovereign_core::registry::ToolRegistry;

/// Small bundle of handles held open across a single `svrn tools`
/// invocation. Built once in `open_tools_registry`, shared across the
/// three subcommands so none of them re-opens SQLite.
pub(super) struct ToolsEnv {
    pub registry: ToolRegistry,
}

/// Open a `ToolRegistry` configured with the full set of native
/// code-intelligence tools. Fails loudly if the `.sovereign/`
/// directory isn't reachable; other missing pieces (watchers, doc
/// index) degrade to in-memory defaults so the CLI stays usable
/// offline.
pub(super) async fn open_tools_registry() -> Result<ToolsEnv, String> {
    // Resolve repo root + .sovereign/. Priority matches project_cmd:
    // nearest-ancestor `.sovereign/` > git root > cwd.
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let sovereign_dir = find_sovereign_dir(&cwd)
        .or_else(|| find_git_root(&cwd).map(|r| r.join(".sovereign")))
        .unwrap_or_else(|| cwd.join(".sovereign"));
    let repo_root = sovereign_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cwd.clone());
    let data_dir = default_data_dir().unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));
    // Flat-file stores (lint_results.db, test_results.db) live at
    // `~/.sovereign/` directly — the canonical path the running
    // `svrn daemon` writes to. Resolving them under `data_dir`
    // (= `~/.sovereign/indexes/`) makes the CLI tool read from a
    // stale orphan DB and report `running` indefinitely while the
    // daemon's actual store reflects fresh results — observed
    // 2026-05-06 with rows untouched since Apr 21.
    let flat_stores_dir = sovereign_cli_shared::dirs::sovereign_root();

    // Embed function: prefer the running daemon's embed slot so
    // `tools call notes` benefits from T1 semantic blend on the
    // CLI side just like the MCP-over-HTTP path does. Falls back
    // to zero-vector when the daemon is unreachable — every tool
    // here either ignores embeddings (pure SQL/FTS) or treats
    // zero vectors as "no semantic signal" and returns FTS-only
    // results, so the offline mode stays correct.
    let daemon_url = std::env::var("SOVEREIGN_DAEMON_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9741".to_string());
    let embed: EmbedFn = build_daemon_embed_fn_or_zero(&daemon_url).await;
    let notes_embed = build_daemon_notes_embed_fn_or_none(&daemon_url).await;
    let engine = Arc::new(CorpusEngine::new(data_dir.clone(), data_dir.clone(), embed));

    // Stores — open each; degrade to in-memory on error so the CLI
    // still works in a cold repo.
    let test_store = Arc::new(
        TestResultStore::open(&flat_stores_dir.join("test_results.db"))
            .or_else(|_| TestResultStore::open(std::path::Path::new(":memory:")))
            .map_err(|e| format!("test results store: {e}"))?,
    );
    let lint_store = Arc::new(
        LintResultStore::open(&flat_stores_dir.join("lint_results.db"))
            .or_else(|_| LintResultStore::open(std::path::Path::new(":memory:")))
            .map_err(|e| format!("lint results store: {e}"))?,
    );
    // NoteStore lives at `~/.sovereign/notes.db` — the same path
    // the daemon writes to, NOT the project-local `<repo>/.sovereign/`.
    // Notes are agent-global working memory (per ATOS), not
    // per-repo state. Two physical DBs split the corpus + leave
    // the CLI reading 15-note fragments while the daemon's
    // canonical store holds the full 298+ history. Other CLI
    // surfaces (audit_recover.rs, code_cmd.rs, reflect_cmd.rs)
    // already use `home_dir().join(".sovereign").join("notes.db")`;
    // registry.rs was the lone outlier. Aligning here unifies
    // both CLI tool invocations + the daemon-side MCP path on a
    // single physical SQLite file (WAL-mode concurrent-safe).
    let notes_store = {
        let inner = NoteStore::open(&flat_stores_dir.join("notes.db"))
            .or_else(|_| NoteStore::open(std::path::Path::new(":memory:")))
            .map_err(|e| format!("notes store: {e}"))?;
        let inner = match notes_embed {
            Some(f) => inner.with_embed_fn(f),
            None => inner,
        };
        Arc::new(inner)
    };
    let features_store = Arc::new(
        FeatureStore::open(&sovereign_dir.join("features.db"))
            .or_else(|_| FeatureStore::open(std::path::Path::new(":memory:")))
            .map_err(|e| format!("feature store: {e}"))?,
    );
    let docs_store = ProjectDocsStore::open(&data_dir.join("project_docs.db"))
        .ok()
        .map(Arc::new);

    // SCIP call graph — empty default if no graph files exist yet.
    // Tools like find_callers gracefully report empty when unmerged.
    let (initial_graph, _summary) = load_merged_graph(&data_dir, false).await;
    let merged_graph: sovereign_tools::ScipGraphHandle =
        Arc::new(ArcSwap::from_pointee(initial_graph));
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(
        &merged_graph,
    )));

    // No `watcher_active` flag wired here: the CLI binary isn't
    // running a watcher of its own — it's a thin reader over the
    // daemon's shared lint_results.db / test_results.db. The
    // watcher tools fall back to a freshness heuristic (data
    // updated within the last ~10 minutes → presumed live) when
    // no explicit flag is supplied. See `derive_watcher_active`
    // in `sovereign-tools/src/code/lint_status.rs`.

    let mut tools = ToolRegistry::new();

    // Code index (LanceDB-backed): identical construction to
    // project_cmd so ids, descriptors, examples all match.
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(
        Arc::clone(&engine),
        Arc::clone(&merged_graph),
    )));
    tools.register(Box::new(sovereign_tools::CodeSearchTool::new(Arc::clone(
        &engine,
    ))));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&engine),
    )));
    tools.register(Box::new(
        sovereign_tools::FindCalleesTool::new(Arc::clone(&engine), Arc::clone(&merged_graph))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(
        sovereign_tools::FindCallersTool::new(Arc::clone(&engine), Arc::clone(&merged_graph))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    // Capability map — derived "what the codebase does" overview.
    tools.register(Box::new(sovereign_tools::CapabilityMapTool::new()));
    // Work atlas — coordination layer for agents sharing the repo.
    // Best-effort: the canonical mesh.db (the same one the daemon
    // writes to) lives at `.sovereign/mesh.db`. Falling back to
    // in-memory keeps the CLI usable in a fresh checkout, with the
    // understanding that nothing the CLI writes will be visible to
    // a separately-running daemon.
    let mesh_db = sovereign_dir.join("mesh.db");
    if let Some(parent) = mesh_db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mesh_store = Arc::new(
        commonwealth_state::MeshStore::open(&mesh_db)
            .or_else(|_| commonwealth_state::MeshStore::in_memory())
            .map_err(|e| format!("work atlas mesh store: {e}"))?,
    );
    let node_id = sovereign_mesh::persist::load_or_generate_self_node_id(&data_dir);
    let atlas_store = Arc::new(sovereign_work_atlas::WorkAtlasStore::new(
        Arc::clone(&mesh_store),
        node_id,
    ));
    let atlas_cfg = sovereign_work_atlas::WorkAtlasConfig::defaults();
    let atlas_broadcaster: Arc<dyn sovereign_work_atlas::tools::ClaimBroadcaster> =
        Arc::new(sovereign_work_atlas::tools::NullBroadcaster);
    // repo_id resolution can hard-fail (no origin remote). The CLI
    // tools-call path still wants the tools registered so users see
    // them in `svrn tools list`; declare_scope just rejects at
    // execute time. work_in_flight is independent of repo_id.
    let (atlas_repo_root, atlas_repo_id) = sovereign_work_atlas::resolve_repo_id(&repo_root)
        .unwrap_or_else(|_| (repo_root.clone(), String::new()));
    let current_branch = current_branch_for(&atlas_repo_root);
    tools.register(Box::new(
        sovereign_work_atlas::tools::DeclareScopeTool::new(
            Arc::clone(&atlas_store),
            atlas_cfg.clone(),
            Arc::clone(&atlas_broadcaster),
            atlas_repo_root.clone(),
            atlas_repo_id.clone(),
            current_branch.clone(),
        ),
    ));
    tools.register(Box::new(
        sovereign_work_atlas::tools::ReleaseScopeTool::new(
            Arc::clone(&atlas_store),
            Arc::clone(&atlas_broadcaster),
        ),
    ));
    tools.register(Box::new(
        sovereign_work_atlas::tools::WorkInFlightTool::new(Arc::clone(&atlas_store)),
    ));

    tools.register(Box::new(
        sovereign_tools::BlastRadiusTool::new(Arc::clone(&merged_graph))
            .with_project_root(repo_root.clone())
            .with_health_checker(Arc::clone(&health_checker))
            .with_atlas(Arc::clone(&atlas_store)),
    ));

    // Watcher tools. The CLI is a separate process from the daemon, so
    // it can't see the daemon's in-memory heartbeat — but the daemon
    // mirrors it to a sidecar file. A reader heartbeat over that path
    // gives the CLI's status tools the SAME liveness the daemon reports,
    // so `svrn tools call test_status` distinguishes live / dead /
    // not-configured precisely instead of guessing from data age.
    // `with_workspace_root` enables per-file freshness queries
    // (`lint_status --files <paths>` and `lint_status --changed`).
    let heartbeat_reader =
        corpus_engine::WatcherHeartbeat::reader(flat_stores_dir.join("watcher-heartbeat"));
    // Read the same runner config the daemon does so `configured` (and
    // the displayed scope) is correct per tool — the heartbeat is shared
    // across lint+test, so a live coordinator alone doesn't tell us
    // whether THIS tool's runner exists.
    let sov_cfg = corpus_engine::SovereignConfig::load_or_default(&repo_root.join(".sovereign"));
    let lint_scope = sov_cfg.lint_runner.as_ref().map(|c| c.command.clone());
    let test_scope = sov_cfg.test_runner.as_ref().map(|c| c.command.clone());
    let mut lint_status = sovereign_tools::LintStatusTool::new(Arc::clone(&lint_store))
        .with_workspace_root(repo_root.clone())
        .with_heartbeat(Arc::clone(&heartbeat_reader));
    if let Some(ref scope) = lint_scope {
        lint_status = lint_status.with_watched_scope(scope.clone());
    }
    tools.register(Box::new(lint_status));
    // Architectural-drift freshness gate — sibling to lint_status.
    // Replaces the launchd-cron trigger model: the brief / pre-push
    // hook query this; the orchestrator writes the fingerprint after
    // a successful run.
    tools.register(Box::new(
        sovereign_tools::DriftPostureTool::new().with_workspace_root(repo_root.clone()),
    ));
    // Point-of-edit drift query — sibling to drift_posture. Lets
    // an agent ask "what does the narrative say about THIS symbol
    // or THIS file?" without re-running drift detect, mirroring
    // how `callers(name)` answers the code-side question. Reads
    // the canonical ~/.sovereign/drift/latest.md.json the
    // orchestrator now mirrors after every run.
    tools.register(Box::new(sovereign_tools::DriftFindingsTool::new()));
    // Capability-reconciliation freshness + findings — siblings to drift_*,
    // over the `enrich capability-reconcile` artifact (corroborated /
    // undocumented / drifted, derived capabilities vs the architecture docs).
    tools.register(Box::new(
        sovereign_tools::CapabilityPostureTool::new().with_workspace_root(repo_root.clone()),
    ));
    tools.register(Box::new(sovereign_tools::CapabilityFindingsTool::new()));
    tools.register(Box::new(sovereign_tools::GetLintOutputTool::new(
        Arc::clone(&lint_store),
    )));
    // `build` — single-call lint-status + top-error view. Wraps
    // the same lint store as `lint_status`; the agent sees one
    // canonical tool while the legacy ids stay reachable during
    // the alias window.
    let mut build_tool = sovereign_tools::BuildTool::new(Arc::clone(&lint_store))
        .with_heartbeat(Arc::clone(&heartbeat_reader));
    if let Some(ref scope) = lint_scope {
        build_tool = build_tool.with_watched_scope(scope.clone());
    }
    tools.register(Box::new(build_tool));
    let mut test_status = sovereign_tools::TestStatusTool::new(Arc::clone(&test_store))
        .with_heartbeat(Arc::clone(&heartbeat_reader));
    if let Some(ref scope) = test_scope {
        test_status = test_status.with_watched_scope(scope.clone());
    }
    tools.register(Box::new(test_status));
    tools.register(Box::new(sovereign_tools::GetRunOutputTool::new(
        Arc::clone(&test_store),
    )));

    // Notes + ATOS lifecycle tools.
    tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::RetireNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::ReadNoteByIdTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::PromoteNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::ReadNoteDigestTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::ProvisionFeatureTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::ArchiveFeatureTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::RecordAtosEventTool::new(
        Arc::clone(&features_store),
    )));
    // `atos_plan_emit` was added then withdrawn the same session
    // after a first-principles check: forcing the agent through a
    // structured-JSON tool for plan emission solved a problem we
    // didn't actually have. PLAN.md as the source of truth (the
    // agent's `write` tool, markdown the model is fluent in) won
    // out. The tool's source stays in `sovereign-tools` as an
    // escape hatch for future work where rigid structure matters,
    // but it is intentionally NOT registered with the live MCP
    // surface so opencode stops advertising it.
    tools.register(Box::new(sovereign_tools::WriteRedteamFindingTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
        Arc::clone(&notes_store),
    )));

    // Project context + doc health — both require the docs store.
    if let Some(ref ds) = docs_store {
        tools.register(Box::new(
            sovereign_tools::ProjectContextTool::new(Arc::clone(ds))
                .with_features(Arc::clone(&features_store)),
        ));
    }
    tools.register(Box::new(
        sovereign_tools::CheckDocPathsTool::new().with_project_root(repo_root.clone()),
    ));

    // `spec` — single-call active-spec + ARCHITECTURE.md +
    // CHARTER.md reader. Wraps the same docs store as
    // `project_context` so future Phase 5 polish can fold in
    // search-style related-doc excerpts without another
    // registration site.
    {
        let mut tool = sovereign_tools::SpecTool::new();
        if let Some(ref ds) = docs_store {
            tool = tool.with_docs(Arc::clone(ds));
        }
        tools.register(Box::new(tool));
    }
    // `drift` — calls `sovereign_atos::approval::detect_drift`
    // for every feature directory. Stateless; no store needed.
    tools.register(Box::new(sovereign_tools::DriftTool::new()));

    Ok(ToolsEnv { registry: tools })
}

// ─── Path resolution helpers (duplicated from project_cmd) ──────────

fn find_git_root(start: &std::path::Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Build a `corpus_engine::EmbedFn` backed by the running daemon's
/// `/v1/embeddings`. Probes once; if the daemon's offline at CLI
/// startup, returns the zero-vector fallback so SQL/FTS tools
/// stay correct and embedding-sensitive tools degrade to FTS-only
/// behavior. Per call, the closure retries (no probe latch).
async fn build_daemon_embed_fn_or_zero(daemon_url: &str) -> EmbedFn {
    let reachable = probe_daemon(daemon_url).await;
    if !reachable {
        return Arc::new(|_text: &str| {
            Box::pin(async {
                Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; corpus_engine::DEFAULT_EMBED_DIM])
            })
        });
    }
    let url = format!("{}/v1/embeddings", daemon_url);
    let model = "qwen-embedding-0.6b".to_string();
    Arc::new(move |text: &str| {
        let url = url.clone();
        let model = model.clone();
        let input = text.to_string();
        Box::pin(async move {
            let resp = reqwest::Client::new()
                .post(&url)
                .json(&serde_json::json!({ "model": model, "input": input }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| corpus_engine::Error::Embed(format!("daemon: {e}")))?;
            if !resp.status().is_success() {
                return Err(corpus_engine::Error::Embed(format!(
                    "daemon HTTP {}",
                    resp.status()
                )));
            }
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| corpus_engine::Error::Embed(format!("daemon parse: {e}")))?;
            body.get("data")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("embedding"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .ok_or_else(|| {
                    corpus_engine::Error::Embed("daemon: no embedding in response".into())
                })
        })
    })
}

/// Build a `corpus_engine_notes::EmbedFn` adapter for the daemon's
/// embed slot. Returns `None` when the daemon is offline so the
/// NoteStore stays in baseline-FTS5 mode rather than burning
/// per-write embed budget on a closure that always errors.
async fn build_daemon_notes_embed_fn_or_none(
    daemon_url: &str,
) -> Option<corpus_engine_notes::EmbedFn> {
    if !probe_daemon(daemon_url).await {
        return None;
    }
    let url = format!("{}/v1/embeddings", daemon_url);
    let model = "qwen-embedding-0.6b".to_string();
    Some(Arc::new(move |text: &str| {
        let url = url.clone();
        let model = model.clone();
        let input = text.to_string();
        Box::pin(async move {
            let resp = reqwest::Client::new()
                .post(&url)
                .json(&serde_json::json!({ "model": model, "input": input }))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| {
                    corpus_engine_notes::Error::Io(std::io::Error::other(format!(
                        "daemon notes embed: {e}"
                    )))
                })?;
            if !resp.status().is_success() {
                return Err(corpus_engine_notes::Error::Io(std::io::Error::other(
                    format!("daemon notes embed HTTP {}", resp.status()),
                )));
            }
            let body: serde_json::Value = resp.json().await.map_err(|e| {
                corpus_engine_notes::Error::Io(std::io::Error::other(format!(
                    "daemon notes embed parse: {e}"
                )))
            })?;
            body.get("data")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("embedding"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<f32>>()
                })
                .ok_or_else(|| {
                    corpus_engine_notes::Error::Io(std::io::Error::other(
                        "daemon notes embed: no embedding in response",
                    ))
                })
        })
    }))
}

async fn probe_daemon(daemon_url: &str) -> bool {
    let url = format!("{}/v1/models", daemon_url);
    matches!(
        reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await,
        Ok(r) if r.status().is_success()
    )
}

/// Resolve the current branch for `repo_root`, or `None` if not a git
/// repo / unborn HEAD. Best-effort — atlas sessions just leave the
/// field empty when unresolvable.
fn current_branch_for(repo_root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s)
    }
}
