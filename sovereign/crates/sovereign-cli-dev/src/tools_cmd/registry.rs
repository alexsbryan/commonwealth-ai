//! Build a `ToolRegistry` with enough infrastructure for agent-driven
//! `sovereign tools <cmd>` invocations.
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
//! without a running `sovereign daemon`) still register cleanly —
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
use corpus_engine_notes::{NoteStore, ProjectDocsStore};
use corpus_engine_atos::{FeatureStore};
use sovereign_cli_shared::{
    dirs::default_data_dir,
    repo::find_sovereign_dir,
    scip::load_merged_graph,
};
use sovereign_core::registry::ToolRegistry;

/// Small bundle of handles held open across a single `sovereign tools`
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
    // `sovereign daemon` writes to. Resolving them under `data_dir`
    // (= `~/.sovereign/indexes/`) makes the CLI tool read from a
    // stale orphan DB and report `running` indefinitely while the
    // daemon's actual store reflects fresh results — observed
    // 2026-05-06 with rows untouched since Apr 21.
    let flat_stores_dir = crate::util::dirs::sovereign_root();

    // Zero-vector embed function. Descriptor() + every tool here does
    // a pure SQL/FTS lookup — embeddings are consulted inside
    // `code_search` when present, but the tool falls back to FTS on
    // empty vectors so this default is safe.
    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
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
    let notes_store = Arc::new(
        NoteStore::open(&sovereign_dir.join("notes.db"))
            .or_else(|_| NoteStore::open(std::path::Path::new(":memory:")))
            .map_err(|e| format!("notes store: {e}"))?,
    );
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
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(
        Arc::clone(&merged_graph),
    ));

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
    // them in `sovereign tools list`; declare_scope just rejects at
    // execute time. work_in_flight is independent of repo_id.
    let (atlas_repo_root, atlas_repo_id) =
        sovereign_work_atlas::resolve_repo_id(&repo_root)
            .unwrap_or_else(|_| (repo_root.clone(), String::new()));
    let current_branch = current_branch_for(&atlas_repo_root);
    tools.register(Box::new(sovereign_work_atlas::tools::DeclareScopeTool::new(
        Arc::clone(&atlas_store),
        atlas_cfg.clone(),
        Arc::clone(&atlas_broadcaster),
        atlas_repo_root.clone(),
        atlas_repo_id.clone(),
        current_branch.clone(),
    )));
    tools.register(Box::new(sovereign_work_atlas::tools::ReleaseScopeTool::new(
        Arc::clone(&atlas_store),
        Arc::clone(&atlas_broadcaster),
    )));
    tools.register(Box::new(sovereign_work_atlas::tools::WorkInFlightTool::new(
        Arc::clone(&atlas_store),
    )));

    tools.register(Box::new(
        sovereign_tools::BlastRadiusTool::new(Arc::clone(&merged_graph))
            .with_project_root(repo_root.clone())
            .with_health_checker(Arc::clone(&health_checker))
            .with_atlas(Arc::clone(&atlas_store)),
    ));

    // Watcher tools — no `with_watcher_active` here. The CLI is a
    // reader over the daemon's shared store; the tools' built-in
    // freshness heuristic decides `watcher_active` from data age.
    // `with_workspace_root` enables per-file freshness queries
    // (`lint_status --files <paths>` and `lint_status --changed`).
    tools.register(Box::new(
        sovereign_tools::LintStatusTool::new(Arc::clone(&lint_store))
            .with_workspace_root(repo_root.clone()),
    ));
    // Architectural-drift freshness gate — sibling to lint_status.
    // Replaces the launchd-cron trigger model: the brief / pre-push
    // hook query this; the orchestrator writes the fingerprint after
    // a successful run.
    tools.register(Box::new(
        sovereign_tools::DriftPostureTool::new()
            .with_workspace_root(repo_root.clone()),
    ));
    // Point-of-edit drift query — sibling to drift_posture. Lets
    // an agent ask "what does the narrative say about THIS symbol
    // or THIS file?" without re-running drift detect, mirroring
    // how `callers(name)` answers the code-side question. Reads
    // the canonical ~/.sovereign/drift/latest.md.json the
    // orchestrator now mirrors after every run.
    tools.register(Box::new(sovereign_tools::DriftFindingsTool::new()));
    tools.register(Box::new(sovereign_tools::GetLintOutputTool::new(
        Arc::clone(&lint_store),
    )));
    // `build` — single-call lint-status + top-error view. Wraps
    // the same lint store as `lint_status`; the agent sees one
    // canonical tool while the legacy ids stay reachable during
    // the alias window.
    tools.register(Box::new(sovereign_tools::BuildTool::new(Arc::clone(
        &lint_store,
    ))));
    tools.register(Box::new(sovereign_tools::TestStatusTool::new(Arc::clone(
        &test_store,
    ))));
    tools.register(Box::new(sovereign_tools::GetRunOutputTool::new(
        Arc::clone(&test_store),
    )));

    // Notes + ATOS lifecycle tools.
    tools.register(Box::new(sovereign_tools::WriteNoteTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::ReadNoteByIdTool::new(Arc::clone(
        &notes_store,
    ))));
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
    Some(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim(),
    ))
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
