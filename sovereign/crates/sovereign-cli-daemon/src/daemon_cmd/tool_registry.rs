// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon MCP tool-registry construction — extracted from `daemon_cmd`
//! (§3.2). Builds the code-intel + notes + work-atlas + lint/test
//! `ToolRegistry` and the merged in-memory SCIP graph it reads from.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use corpus_engine_watchers::{LintResultStore, TestResultStore};
use sovereign_core::ToolRegistry;

pub(super) async fn build_tool_registry(
    data_dir: &std::path::Path,
    engine: Arc<CorpusEngine>,
    notes: Arc<NoteStore>,
    lint_store: Arc<LintResultStore>,
    test_store: Arc<TestResultStore>,
    test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>>,
    watched_lint_scope: Option<String>,
    watched_test_scope: Option<String>,
    watcher_heartbeat: Arc<corpus_engine_watchers::WatcherHeartbeat>,
    workspace_dir: Option<PathBuf>,
    work_atlas_store: Arc<sovereign_work_atlas::WorkAtlasStore>,
    work_atlas_cfg: sovereign_work_atlas::WorkAtlasConfig,
    work_atlas_broadcaster: Arc<sovereign_work_atlas::tools::DeferredBroadcaster>,
    work_atlas_repo_root: Option<PathBuf>,
    work_atlas_repo_id: Option<String>,
    work_atlas_branch: Option<String>,
    solve_jobs: Arc<super::solve_http::SolveJobs>,
    // The shared merged SCIP graph. Created ONCE by the caller and handed to
    // BOTH these tools and the project reindexer, so the reindexer's live
    // updates (the tree-sitter overlay and full rebuilds) are visible to
    // symbols/callers/blast without a daemon restart. Before this was shared,
    // the tools read a frozen startup snapshot while the reindexer updated an
    // orphan graph nothing queried — the root of "always stale."
    merged_graph_handle: sovereign_tools::ScipGraphHandle,
) -> ToolRegistry {
    let indexes_dir = data_dir.join("indexes");

    // Tier 4 — shared tool-result cache. The daemon's registry
    // serves both HTTP API requests AND in-process Runtime calls;
    // per-conversation scoping in `CacheKey` keeps the slices
    // isolated even when two clients hit different conversations
    // simultaneously.
    let tool_cache = std::sync::Arc::new(sovereign_core::tool_result_cache::ToolResultCache::new());
    let mut tools = ToolRegistry::new().with_cache(std::sync::Arc::clone(&tool_cache));

    // Call-graph tools. Merge every `scip_graph.db` under the indexes
    // directory into a single in-memory graph, then register
    // find_callers / find_callees / blast_radius. Without this step
    // agents can't trace references through the daemon — project_serve
    // had these, the daemon didn't.
    //
    // The graph also backs `symbols`/exact-name lookup, so build it
    // BEFORE registering the code-intel tools below.
    // Use the caller-provided shared handle (see the param doc) rather than
    // building a private snapshot — that private snapshot never saw the
    // reindexer's updates and is the staleness this unification fixes.
    let graph_handle = merged_graph_handle;

    // Code intelligence — scoped to discovered corpora under indexes_dir.
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(
        &graph_handle,
    )));
    tools.register(Box::new(
        sovereign_tools::SymbolLookupTool::new(Arc::clone(&engine), Arc::clone(&graph_handle))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(sovereign_tools::CodeSearchTool::new(Arc::clone(
        &engine,
    ))));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(
        Arc::clone(&engine),
    )));
    tools.register(Box::new(
        sovereign_tools::FindCallersTool::new(Arc::clone(&engine), Arc::clone(&graph_handle))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(
        sovereign_tools::FindCalleesTool::new(Arc::clone(&engine), Arc::clone(&graph_handle))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(
        sovereign_tools::BlastRadiusTool::new(Arc::clone(&graph_handle))
            .with_health_checker(Arc::clone(&health_checker))
            .with_atlas(Arc::clone(&work_atlas_store)),
    ));
    // Capability map — derived "what the codebase does" overview. Resolves
    // the per-corpus SCIP graph itself (same indexes dir the runtime uses).
    tools.register(Box::new(sovereign_tools::CapabilityMapTool::new()));
    // Architecture observability (quality program) — the SCIP-observed half
    // of the layer-map story (arch_report) + the cheap persisted-posture
    // reader (arch_posture). The workspace root unlocks the declared-deps /
    // layer-map / filesystem sections and the posture freshness check.
    {
        let mut tool = sovereign_tools::ArchReportTool::new();
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_project_root(ws);
        }
        tools.register(Box::new(tool));
    }
    {
        let mut tool = sovereign_tools::ArchPostureTool::new();
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_project_root(ws);
        }
        tools.register(Box::new(tool));
    }

    // Deterministic land-value-tax analytics over parcel corpora
    // (e.g. sf-assessor-roll) — pre-cited figures for the "no
    // confabulated numbers" demo. Read-only; safe on the MCP surface.
    tools.register(Box::new(
        sovereign_tools::parcel_analytics::ParcelAnalyticsTool::new(Arc::clone(&engine)),
    ));

    // ── Work atlas tools (Phase 2) ──────────────────────────────
    // Always registered so MCP clients see them even on a repo
    // without an origin remote — `declare_scope` rejects with an
    // actionable error in that case. `work_in_flight` is read-only
    // and works without an origin (it just returns what the daemon
    // has heard from peers).
    tools.register(Box::new(
        sovereign_work_atlas::tools::DeclareScopeTool::new(
            Arc::clone(&work_atlas_store),
            work_atlas_cfg.clone(),
            Arc::clone(&work_atlas_broadcaster)
                as Arc<dyn sovereign_work_atlas::tools::ClaimBroadcaster>,
            work_atlas_repo_root
                .clone()
                .unwrap_or_else(|| data_dir.to_path_buf()),
            work_atlas_repo_id.clone().unwrap_or_default(),
            work_atlas_branch.clone(),
        ),
    ));
    tools.register(Box::new(
        sovereign_work_atlas::tools::ReleaseScopeTool::new(
            Arc::clone(&work_atlas_store),
            Arc::clone(&work_atlas_broadcaster)
                as Arc<dyn sovereign_work_atlas::tools::ClaimBroadcaster>,
        ),
    ));
    tools.register(Box::new(
        sovereign_work_atlas::tools::WorkInFlightTool::new(Arc::clone(&work_atlas_store)),
    ));

    // ── Lint / test watcher tools ───────────────────────────────
    // Always registered so MCP clients see a stable tool list. When
    // no watcher is wired (workspace not resolved or sovereign.toml
    // empty), the tools report `never_run` / `watcher_active: false`
    // — accurate, not silently-missing.
    {
        let mut tool = sovereign_tools::LintStatusTool::new(Arc::clone(&lint_store))
            .with_heartbeat(Arc::clone(&watcher_heartbeat));
        if let Some(scope) = watched_lint_scope.clone() {
            tool = tool.with_watched_scope(scope);
        }
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_workspace_root(ws);
        }
        tools.register(Box::new(tool));
    }
    {
        let mut tool = sovereign_tools::DriftPostureTool::new();
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_workspace_root(ws);
        }
        tools.register(Box::new(tool));
    }
    // Findings half of the drift pair. `MCP_TOOLS_ALWAYS` declares
    // `drift_findings`, but the daemon previously registered only
    // `drift_posture` — so the tool was on the manifest yet absent from the
    // live surface (preflight FAIL: "declared but not served"). It reads the
    // canonical `~/.svrnmesh/drift` sidecar via its default `drift_dir`, so a
    // bare `::new()` is correct — mirroring the CapabilityFindingsTool
    // registration below (a posture/findings pair that was wired symmetrically).
    tools.register(Box::new(sovereign_tools::DriftFindingsTool::new()));
    // Code facts — the embed-free structural fact base (tree-sitter fn defs /
    // config construction-fields / string literals), read from
    // `<indexes_dir>/<corpus>/facts.json`. Read-only and model-free, so it never
    // contends with agent inference; every response is freshness-stamped.
    tools.register(Box::new(sovereign_tools::FactsTool::new(
        indexes_dir.clone(),
    )));
    // Capability-reconciliation freshness + findings — siblings to drift_*,
    // over the `enrich capability-reconcile` artifact.
    {
        let mut tool = sovereign_tools::CapabilityPostureTool::new();
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_workspace_root(ws);
        }
        tools.register(Box::new(tool));
    }
    tools.register(Box::new(sovereign_tools::CapabilityFindingsTool::new()));
    {
        let mut tool = sovereign_tools::BuildTool::new(Arc::clone(&lint_store))
            .with_heartbeat(Arc::clone(&watcher_heartbeat));
        if let Some(scope) = watched_lint_scope {
            tool = tool.with_watched_scope(scope);
        }
        tools.register(Box::new(tool));
    }
    tools.register(Box::new(sovereign_tools::GetLintOutputTool::new(
        Arc::clone(&lint_store),
    )));
    {
        let mut tool = sovereign_tools::TestStatusTool::new(Arc::clone(&test_store))
            .with_heartbeat(Arc::clone(&watcher_heartbeat));
        if let Some(scope) = watched_test_scope {
            tool = tool.with_watched_scope(scope);
        }
        tools.register(Box::new(tool));
    }
    tools.register(Box::new(sovereign_tools::GetRunOutputTool::new(
        Arc::clone(&test_store),
    )));
    // `run_tests` is only registered when there's a live test watcher
    // to dispatch into. Without it, agents calling `run_tests` would
    // get a confusing no-op; the absence is the honest signal.
    if let Some(ref w) = test_watcher {
        tools.register(Box::new(sovereign_tools::RunTestsTool::new(Arc::clone(w))));
    }

    // NOTE: knowledge_lookup (Tool-Mastery Phase 5) is wired in
    // chat_cmd/bootstrap.rs where the `inference` + `store`
    // handles are available. The daemon's MCP-only tool registry
    // intentionally does not expose it — the unified knowledge
    // envelope only makes sense inside an active chat
    // conversation (the consumer is the model's synthesis path,
    // not arbitrary MCP clients).

    // Session-orientation brief — the SessionStart hook's renderer on
    // the MCP surface. Threads the live work-atlas store so the
    // "Work in flight" section reflects what this daemon has heard
    // from peers, not a stale read-only snapshot. Registered without
    // a workspace too: it rejects at execute time with the same
    // actionable set-SOVEREIGN_WORKSPACE_DIR message the watcher
    // tools use, rather than being silently absent.
    {
        let mut tool = sovereign_tools::BriefingTool::new(Arc::clone(&notes))
            .with_atlas(Arc::clone(&work_atlas_store));
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_workspace_root(ws);
        }
        tools.register(Box::new(tool));
    }

    // Encode-time session-frame upsert (SESSION_CONTINUITY write-path
    // 1). Workspace optional — without it the frame just skips the
    // head_at_end/branch git stamps.
    {
        let mut tool = sovereign_tools::SessionStateTool::new();
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_workspace_root(ws);
        }
        tools.register(Box::new(tool));
    }

    // Notes tools work regardless of indexing state.
    tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(
        &notes,
    ))));
    {
        // Workspace optional — without it the operational-anchors
        // registry loader falls back to SOVEREIGN_WORKSPACE_DIR, the
        // ~/.svrnmesh/workspace file, then an ascent from the cwd,
        // then the compiled-in floor (read_notes.rs).
        let mut tool = sovereign_tools::ReadNotesTool::new(Arc::clone(&notes));
        if let Some(ws) = workspace_dir.clone() {
            tool = tool.with_workspace_root(ws);
        }
        tools.register(Box::new(tool));
    }
    tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(Arc::clone(
        &notes,
    ))));
    tools.register(Box::new(sovereign_tools::RetireNoteTool::new(Arc::clone(
        &notes,
    ))));
    tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
        Arc::clone(&notes),
    )));

    // ATOS step verification — runs verify commands with
    // hollow/untouched gates to catch silent agent no-ops.
    tools.register(Box::new(sovereign_tools::AtosVerifyTool::new()));

    // Project context — served from `indexes/project_docs.db` if a
    // project has been init'd. Absent on a bare-setup daemon; that's
    // fine, just one fewer tool.
    #[cfg(feature = "atos")]
    if let Ok(ds) =
        corpus_engine_notes::ProjectDocsStore::open(&indexes_dir.join("project_docs.db"))
    {
        tools.register(Box::new(sovereign_tools::ProjectContextTool::new(
            Arc::new(ds),
        )));
    }

    // Doc-path checker — no state dependency.
    tools.register(Box::new(sovereign_tools::CheckDocPathsTool::new()));

    // Wikipedia on-demand fetch — operates against the catalog corpus
    // installed on this daemon. Wired here so `svrn tools call
    // wikipedia_fetch --title=…` and the MCP /mcp surface can drive
    // catalog-hit → fetch end-to-end without a live chat session.
    tools.register(Box::new(sovereign_tools::WikipediaFetchTool::new(
        Arc::clone(&engine),
    )));

    // DESIGN.md structural signals — no state dependency; the tool
    // reads the DESIGN.md path argument at call time. No
    // `with_project_root` in the daemon context because the daemon
    // doesn't know which project the caller means. ATOS-gated.
    #[cfg(feature = "atos")]
    tools.register(Box::new(sovereign_tools::DesignSignalsExtractTool::new()));

    // B:P9d — the corpus/atlas plane (corpus_store, corpus_search, atlas_gaps,
    // atlas_tensions, + the document ExtractTool). `standard_registry` dropped
    // these when the studio bundle carved out corpus-engine (B:P5); the daemon
    // still links corpus-engine, so it registers them HERE to serve them over
    // `/mcp`, letting a corpus-engine-free studio client (B:P9e) run the shipped
    // notebook / summarize workflows remotely. Each is a bare unit struct that
    // opens its corpus/atlas (or document) from disk at call time, so no engine
    // handle is threaded in. `extract` resolves its `path` on THIS host's
    // filesystem — correct for a loopback client, a clean "file not found" for a
    // remote one (see `sovereign_tools::mcp_surface::MCP_TOOLS_ALWAYS`).
    for tool in sovereign_tools::workflow_corpus_tools() {
        tools.register(tool);
    }

    // SOLVE — the daemon-hosted TDD solver (docs/specs/SOLVE_UX.md).
    // Same job table as the /v1/solve/jobs HTTP surface, so MCP
    // agents and curl sessions see the same jobs.
    tools.register(Box::new(super::solve_tools::SolveTool(Arc::clone(
        &solve_jobs,
    ))));
    tools.register(Box::new(super::solve_tools::SolveStatusTool(Arc::clone(
        &solve_jobs,
    ))));
    tools.register(Box::new(super::solve_tools::SolveCancelTool(solve_jobs)));

    tools
}

/// Merge every per-corpus `scip_graph.db` under `indexes_dir` into a
/// single in-memory graph. Same idea as `project_cmd::load_merged_graph`
/// but without the operator-facing stdout printing, since the daemon
/// runs under launchd/systemd.
pub(super) async fn build_merged_scip_graph(
    indexes_dir: &std::path::Path,
) -> corpus_engine_scip::ScipGraph {
    let merged =
        corpus_engine_scip::ScipGraph::open_in_memory("merged").expect("in-memory ScipGraph");
    let Ok(entries) = std::fs::read_dir(indexes_dir) else {
        return merged;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let scip_path = path.join("scip_graph.db");
        if !scip_path.exists() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        match merged.import_from_path(&scip_path).await {
            Ok((syms, refs)) => {
                if syms > 0 || refs > 0 {
                    tracing::info!(
                        corpus = %name,
                        symbols = syms,
                        references = refs,
                        "merged SCIP graph from corpus"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    corpus = %name,
                    error = %e,
                    "could not import SCIP graph — skipping"
                );
            }
        }
    }
    merged
}
