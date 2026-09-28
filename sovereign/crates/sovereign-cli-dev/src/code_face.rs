// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code's face: the one composition of the code program (phase-b
//! pb-code-daemon-exit). It opens code's result stores and builds the
//! Reindexer, the lint/test watchers, the work atlas, code's tool bundles,
//! the MCP dispatcher over them and `/v1/projects/*`. `svrn code mcp` serves
//! it alone; the stock binary composes it into svrn's process and mounts it
//! on svrn's one `:9741/mcp` (FIVE_PROGRAMS §2c; phase-b-30 F2 (a),
//! phase-b-33). The caller owns placement: which directories, which open
//! note store and chunk index, and the transport.

use std::path::PathBuf;
use std::sync::Arc;

use corpus_engine_notes::NoteStore;
use host_kit::mcp::McpDispatcher;
use sovereign_contracts::ToolRegistry;

pub use crate::project_cmd::mcp_host::{CodeCallLog, CodeTools};

/// Where code's data lives and the handles its host already holds.
pub struct CodeParts {
    /// The per-corpus indexes and SCIP graphs; the Reindexer's registry.
    pub indexes_dir: PathBuf,
    /// Where `test_results.db` and `lint_results.db` live.
    pub stores_dir: PathBuf,
    /// The note store, already open: one writer per data root.
    pub notes: Arc<NoteStore>,
    /// The chunk index `symbols`, `code_search` and `recent_changes` read.
    pub index: Arc<dyn corpus_index::source::IndexSource>,
    /// The repo code watches and scopes the work atlas to; `None` runs no
    /// watcher and leaves claims unscoped.
    pub workspace: Option<PathBuf>,
    /// The `.sovereign/` whose `sovereign.toml` names the runners; `None`
    /// is the workspace's.
    pub sovereign_dir: Option<PathBuf>,
    /// Groups this composition's calls in the note store's call log.
    pub session_prefix: &'static str,
    /// Watchers the host adds to code's coordinator.
    pub extra_watchers: Vec<Arc<dyn corpus_engine_watchers::BackgroundWatcher>>,
}

/// Code, composed: what a host serves and what it holds for its life.
pub struct CodeFace {
    /// Code's MCP dispatch over its registry and call log.
    pub mcp: McpDispatcher<CodeTools, CodeCallLog>,
    /// The registry behind `mcp`, for `/mcp/stats`.
    pub tools: Arc<ToolRegistry>,
    /// `/v1/projects/*`, the Reindexer's HTTP surface.
    pub routes: axum::Router,
    /// One line per store, runner and bundle, for the host's banner or log.
    pub banner: Vec<String>,
    /// The Reindexer, the watcher coordinator and the atlas GC: dropping it
    /// stops them.
    pub runtime: CodeRuntime,
}

/// What keeps code's background work alive.
pub struct CodeRuntime {
    _reindexer: Arc<corpus_engine_watchers::reindexer::Reindexer>,
    _coordinator: Option<corpus_engine_watchers::CoordinatorHandle>,
    _atlas_gc: tokio::task::JoinHandle<()>,
}

/// Compose code. An `Err` names the store that did not open: a store is
/// refused by path, never replaced by an in-memory one whose writes vanish
/// at exit (principle 6).
pub async fn compose(parts: CodeParts) -> Result<CodeFace, String> {
    let CodeParts {
        indexes_dir,
        stores_dir,
        notes: notes_store,
        index,
        workspace,
        sovereign_dir,
        session_prefix,
        extra_watchers,
    } = parts;
    let mut banner = Vec::new();

    // ── Open result stores (SQLite, always-on) ──────────────────
    let test_results_path = stores_dir.join("test_results.db");
    let test_store = match corpus_engine_watchers::TestResultStore::open(&test_results_path) {
        Ok(s) => {
            banner.push("test_results.db  ✓".to_string());
            Arc::new(s)
        }
        Err(e) => return Err(format!("cannot open {}: {e}", test_results_path.display())),
    };

    let lint_results_path = stores_dir.join("lint_results.db");
    let lint_store = match corpus_engine_watchers::LintResultStore::open(&lint_results_path) {
        Ok(s) => {
            banner.push("lint_results.db  ✓".to_string());
            Arc::new(s)
        }
        Err(e) => return Err(format!("cannot open {}: {e}", lint_results_path.display())),
    };

    // ── Freshness: the Reindexer ────────────────────────────────
    // The one freshness path (phase-b pb-code-freshness): per-project FS
    // watchers, git-HEAD polls and the rebuild queue write into the graph
    // the tools read. It replaced the 30 s `scip_graph.db` mtime poll.
    // Loaded when a tool first reads it (phase-b pb-code-freshness).
    let merged_graph = sovereign_code::LazyScipGraph::deferred(indexes_dir.clone());
    let (reindexer, registry) = sovereign_code::freshness::start_reindexer(
        indexes_dir.clone(),
        &merged_graph,
        Arc::clone(&notes_store),
    )
    .await;
    banner.push(format!(
        "Reindexer        ✓  {} registered project(s)",
        registry.entries().len()
    ));

    let sovereign_cfg = match (&sovereign_dir, &workspace) {
        (Some(dir), _) => sovereign_contracts::config::SovereignConfig::load_or_default(dir),
        (None, Some(ws)) => {
            sovereign_contracts::config::SovereignConfig::load_or_default(&ws.join(".sovereign"))
        }
        (None, None) => sovereign_contracts::config::SovereignConfig::default(),
    };

    // ── Build background watchers ───────────────────────────────

    // Shared run slot so lint + test cargo invocations serialize
    // instead of double-spawning on every debounced edit flush.
    let run_slot = Arc::new(tokio::sync::Semaphore::new(1));
    let resolve = |d: &String| {
        let p = PathBuf::from(d);
        match (&workspace, p.is_absolute()) {
            (Some(ws), false) => ws.join(p),
            _ => p,
        }
    };

    let test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>> =
        sovereign_cfg.test_runner.as_ref().map(|cfg| {
            let working_dir = cfg.working_dir.as_ref().map(resolve);
            banner.push(format!(
                "test_runner      ✓  {}",
                cfg.command.chars().take(60).collect::<String>()
            ));
            Arc::new(
                corpus_engine_watchers::TestWatcher::new(
                    &cfg.command,
                    working_dir,
                    cfg.timeout_secs.unwrap_or(300),
                    Arc::clone(&test_store),
                )
                .with_run_slot(Arc::clone(&run_slot)),
            )
        });

    let lint_watcher: Option<Arc<corpus_engine_watchers::LintWatcher>> =
        sovereign_cfg.lint_runner.as_ref().map(|cfg| {
            let working_dir = cfg.working_dir.as_ref().map(resolve);
            banner.push(format!(
                "lint_runner      ✓  {}",
                cfg.command.chars().take(60).collect::<String>()
            ));
            Arc::new(
                corpus_engine_watchers::LintWatcher::new(
                    &cfg.command,
                    working_dir,
                    cfg.timeout_secs.unwrap_or(120),
                    Arc::clone(&lint_store),
                )
                .with_run_slot(Arc::clone(&run_slot)),
            )
        });

    let config_dir = sovereign_dir
        .clone()
        .or_else(|| workspace.as_ref().map(|ws| ws.join(".sovereign")));
    match (
        &config_dir,
        test_watcher.is_none() && lint_watcher.is_none(),
    ) {
        (Some(dir), true) => banner.push(format!(
            "warning: no watchers configured — add [test_runner] / [lint_runner] to {}",
            dir.join("sovereign.toml").display()
        )),
        (None, _) => tracing::debug!("code: no workspace, so no lint/test watcher"),
        (Some(_), false) => {}
    }

    // Scope strings for lint/test status tools — shown to agents so they can
    // confirm the watcher covers the crates they just edited.
    let test_watched_scope: Option<String> = sovereign_cfg
        .test_runner
        .as_ref()
        .map(|c| c.command.clone());
    let lint_watched_scope: Option<String> = sovereign_cfg
        .lint_runner
        .as_ref()
        .map(|c| c.command.clone());

    // Shared flag: set to true after coordinator.start() succeeds. Tools expose
    // this as watcher_active so agents know the FS watcher is live.
    let watcher_active_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // ── Work atlas ──────────────────────────────────────────────
    // Coordination layer for agents sharing this repo. The store is
    // cw-rails', dialed (pb-atlas-kv) — there is no repo-local mesh.db;
    // with cw-rails down every claim operation reports the absence by
    // name. Per spec §10 the origin-remote MUST gate is checked at *boot*:
    // a repo with no origin still gets a serve, but every `declare_scope`
    // call fails with an actionable error rather than silently writing
    // partial state.
    let atlas_mesh_store: Arc<dyn sovereign_work_atlas::ReplicatedKv> =
        Arc::new(crate::mesh_kv_client::atlas_kv());
    let atlas_node_id = crate::atlas_identity::atlas_node_id();
    let atlas_store = Arc::new(sovereign_work_atlas::WorkAtlasStore::new(
        Arc::clone(&atlas_mesh_store),
        atlas_node_id,
    ));
    let atlas_cfg_path = sovereign_cli_shared::dirs::work_atlas_toml();
    let atlas_cfg = sovereign_work_atlas::WorkAtlasConfig::load_or_default(&atlas_cfg_path)
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                path = %atlas_cfg_path.display(),
                "work_atlas: failed to load config, falling back to defaults"
            );
            sovereign_work_atlas::WorkAtlasConfig::defaults()
        });
    let atlas_root = workspace.clone().unwrap_or_else(|| stores_dir.clone());
    let (atlas_repo_root, atlas_repo_id) =
        match sovereign_work_atlas::resolve_repo_id_allowing_local(&atlas_root) {
            Ok((root, id, source)) => {
                if let Some(caveat) = source.caveat() {
                    tracing::info!(caveat, "work_atlas: using a machine-local repo id");
                }
                (root, id)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "work_atlas:repo_id_missing — not inside a git repo, so claims \
                     cannot be scoped to one"
                );
                (atlas_root.clone(), String::new())
            }
        };
    let atlas_branch = crate::code_cmd::current_branch(&atlas_repo_root);
    // GC loop. Its handle is held on the runtime, so dropping the runtime
    // aborts it cleanly when the host stops.
    let atlas_gc =
        sovereign_work_atlas::gc::WorkAtlasGc::new(Arc::clone(&atlas_store), atlas_cfg.clone())
            .spawn();

    // ── Tool set: code's bundles ────────────────────────────────
    // No model is loaded, so `code_search` answers from full text and
    // `read_note_digest` in its header-only fallback, whose banner names
    // the degraded state.
    let mut watcher_tools = sovereign_code::bundle::WatcherTools::new(
        Arc::clone(&test_store),
        Arc::clone(&lint_store),
        Arc::clone(&watcher_active_flag),
        atlas_root.clone(),
    )
    .with_scopes(test_watched_scope, lint_watched_scope);
    if let Some(ref watcher) = test_watcher {
        watcher_tools = watcher_tools.with_test_watcher(Arc::clone(watcher));
    }
    let mut code_intel = sovereign_code::bundle::CodeIntelTools::new(index, merged_graph.clone())
        .with_peer_work(Arc::clone(&atlas_store) as Arc<dyn sovereign_code::PeerWork>);
    let mut notes_tools = sovereign_code::bundle::NotesTools::new(Arc::clone(&notes_store));
    if let Some(ws) = &workspace {
        code_intel = code_intel.with_project_root(ws.clone());
        notes_tools = notes_tools.with_workspace_root(ws.clone());
    }
    let bundles: Vec<Box<dyn sovereign_contracts::tool_bundle::ToolBundle>> = vec![
        Box::new(code_intel),
        Box::new(sovereign_code::bundle::ArchTools::new(atlas_root.clone())),
        Box::new(watcher_tools),
        Box::new(notes_tools),
        Box::new(sovereign_work_atlas::tools::WorkAtlasTools::new(
            atlas_store,
            atlas_cfg,
            Arc::new(sovereign_work_atlas::tools::NullBroadcaster),
            atlas_repo_root,
            atlas_repo_id,
            atlas_branch,
        )),
    ];
    let mut tools = ToolRegistry::new();
    for report in sovereign_contracts::tool_bundle::install(&mut tools, &bundles).await {
        banner.push(report.summary());
    }

    // ── Start watcher coordinator ───────────────────────────────

    let debounce_ms = sovereign_cfg
        .test_runner
        .as_ref()
        .and_then(|c| c.debounce_ms)
        .or_else(|| {
            sovereign_cfg
                .lint_runner
                .as_ref()
                .and_then(|c| c.debounce_ms)
        })
        .unwrap_or(500);

    let mut coordinator = corpus_engine_watchers::WatcherCoordinator::new(debounce_ms);
    if let Some(ref w) = test_watcher {
        coordinator.register(Arc::clone(w) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
    }
    if let Some(ref w) = lint_watcher {
        coordinator.register(Arc::clone(w) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
    }
    for w in extra_watchers {
        coordinator.register(w);
    }

    let coordinator_handle = match (&workspace, coordinator.registered_ids().is_empty()) {
        (Some(ws), false) => match coordinator.start(vec![ws.clone()]).await {
            Ok(handle) => {
                banner.push(format!("Watcher started (watching {})", ws.display()));
                watcher_active_flag.store(true, std::sync::atomic::Ordering::Release);
                Some(handle)
            }
            Err(e) => {
                banner.push(format!("warning: could not start watcher: {e}"));
                None
            }
        },
        _ => {
            tracing::debug!("code: no workspace or no watcher registered, coordinator not started");
            None
        }
    };

    let tools = Arc::new(tools);
    // Stable session ID for this composition — used by tool_call_log to group
    // its calls.
    let session_id = format!("{session_prefix}-{}", uuid::Uuid::new_v4());
    let mcp = McpDispatcher::new(
        "sovereign-code",
        env!("CARGO_PKG_VERSION"),
        CodeTools {
            tools: Arc::clone(&tools),
            session_id: session_id.clone(),
            feature_root: workspace.clone(),
        },
        CodeCallLog {
            matcher: Arc::new(
                corpus_engine_notes::mining::patterns::ToolPatternMatcher::new(Arc::clone(
                    &notes_store,
                )),
            ),
            notes: notes_store,
            session_id: Arc::new(session_id),
        },
    );
    Ok(CodeFace {
        mcp,
        tools,
        routes: sovereign_code::project_http::project_router(Arc::clone(&reindexer)),
        banner,
        runtime: CodeRuntime {
            _reindexer: reindexer,
            _coordinator: coordinator_handle,
            _atlas_gc: atlas_gc,
        },
    })
}
