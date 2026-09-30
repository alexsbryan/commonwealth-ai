// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code's face: the one composition of the code program (phase-b
//! pb-code-daemon-exit). It opens code's result stores and builds the
//! Reindexer, the lint/test watchers, the work atlas, code's tool bundles,
//! the MCP dispatcher over them and `/v1/projects/*`. `svrn code mcp` serves
//! it alone; the stock binary composes it into svrn's process and mounts it
//! on svrn's one `:9741/mcp` (FIVE_PROGRAMS §2c; phase-b-30 F2 (a),
//! phase-b-33). The caller owns placement: which directories, which open
//! note store and chunk index, and the transport.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine_notes::NoteStore;
use host_kit::mcp::McpDispatcher;
use sovereign_contracts::ToolRegistry;

pub use code_next_edit::grammar::{Grammar, GrammarLookup};
pub use mcp_host::{CodeCallLog, CodeTools};

// The atlas identity and store dialer the face uses, moved from
// sovereign-cli-dev with the face (pb-code-daemon-exit).
pub mod atlas;
// Code's two MCP ports, moved from sovereign-cli-dev with the face.
mod mcp_host;
// The daemon's self-healing coordinator supervisor, moved whole with the
// watcher runtime (pb-code-daemon-exit).
mod watcher_supervisor;
// The notes rail, moved from the daemon's boot with notes.db (pb-notes-memory).
pub mod notes_rail;
pub use notes_rail::NotesRail;

// The pattern matcher's live-wire e2e, moved from the daemon with the
// matcher (pb-code-daemon-exit).
#[cfg(test)]
mod pattern_observation_tests;

/// Where code's data lives and the handles its host already holds.
pub struct CodeParts {
    /// The per-corpus indexes and SCIP graphs; the Reindexer's registry.
    pub indexes_dir: PathBuf,
    /// Where `test_results.db` and `lint_results.db` live.
    pub stores_dir: PathBuf,
    /// The note store, when the host already holds it open (one writer per
    /// data root); `None` opens `stores_dir/notes.db` here (the stock
    /// binary, whose svrn no longer opens it, pb-notes-memory).
    pub notes: Option<Arc<NoteStore>>,
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
    /// What the host wires the note store with; `Default` is code alone.
    pub notes_rail: NotesRail,
    /// The host's extension → grammar registry for the editor door's syntax
    /// filter and symbol lane (pb-meshapp-rest). The stock binary supplies
    /// corpus-engine's; `None` (code alone) leaves both unjudged, by name.
    pub grammar: Option<GrammarLookup>,
}

/// Code, composed: what a host serves and what it holds for its life.
pub struct CodeFace {
    /// Code's MCP dispatch over its registry and call log.
    pub mcp: McpDispatcher<CodeTools, CodeCallLog>,
    /// The registry behind `mcp`, for `/mcp/stats`.
    pub tools: Arc<ToolRegistry>,
    /// `/v1/projects/*`, the Reindexer's HTTP surface, and `/v1/solve/jobs*`,
    /// the solver's (pb-meshapp-solve).
    pub routes: axum::Router,
    /// The editor door, `/v1/edit_predictions` and its outcome route
    /// (`crate::edit_predictions`, pb-meshapp-rest); a host adds its own
    /// admission gate.
    pub edit_routes: axum::Router,
    /// One line per store, runner and bundle, for the host's banner or log.
    pub banner: Vec<String>,
    /// The Reindexer, the watcher supervisor and the atlas GC: dropping it
    /// stops them.
    pub runtime: CodeRuntime,
}

/// What keeps code's background work alive.
pub struct CodeRuntime {
    _reindexer: Arc<corpus_engine_watchers::reindexer::Reindexer>,
    _watcher_monitor: Option<tokio::task::JoinHandle<()>>,
    _atlas_gc: tokio::task::JoinHandle<()>,
    lint_watcher: Option<Arc<corpus_engine_watchers::LintWatcher>>,
    test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>>,
}

impl CodeRuntime {
    /// Hands the lint and test watchers the host's foreground signal, so
    /// their cargo runs stand aside while a person waits on the host (the
    /// stock binary's chat slot). A setter because the host builds its
    /// signal after code is composed.
    pub fn yield_setter(
        &self,
    ) -> Box<dyn Fn(Arc<dyn corpus_engine_yield::YieldHook>) + Send + Sync> {
        let (lint, test) = (self.lint_watcher.clone(), self.test_watcher.clone());
        Box::new(move |hook| {
            if let Some(w) = &lint {
                w.set_yield_hook(Arc::clone(&hook));
                tracing::info!("foreground-yield: hook installed on lint watcher");
            }
            if let Some(w) = &test {
                w.set_yield_hook(Arc::clone(&hook));
                tracing::info!("foreground-yield: hook installed on test watcher");
            }
        })
    }
}

/// Compose code. An `Err` names the store that did not open: a store is
/// refused by path, never replaced by an in-memory one whose writes vanish
/// at exit (principle 6).
pub async fn compose(parts: CodeParts) -> Result<CodeFace, String> {
    let CodeParts {
        indexes_dir,
        stores_dir,
        notes,
        index,
        workspace,
        sovereign_dir,
        session_prefix,
        extra_watchers,
        notes_rail,
        grammar,
    } = parts;
    let mut banner = Vec::new();
    let notes_store = match notes {
        Some(notes) => notes,
        None => {
            let path = stores_dir.join("notes.db");
            let store = NoteStore::open(&path)
                .map_err(|e| format!("cannot open notes db {}: {e}", path.display()))?;
            banner.push(format!("notes: {}", path.display()));
            Arc::new(store)
        }
    };
    notes_rail::wire(&notes_store, notes_rail);

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

    // Wipe orphan rows left by a previous process that was SIGKILLed
    // mid-run. Without this, `lint_status` / `test_status` can return
    // `running` indefinitely against a row whose owning process is long
    // dead. Best-effort — cleanup failure shouldn't block startup.
    if let Ok(n) = lint_store.clear_orphan_runs().await {
        if n > 0 {
            tracing::info!(
                purged = n,
                "lint_results: cleared orphan rows from prior process"
            );
        }
    }
    if let Ok(n) = test_store.clear_orphan_runs().await {
        if n > 0 {
            tracing::info!(
                purged = n,
                "test_results: cleared orphan rows from prior process"
            );
        }
    }

    // ── Freshness: the Reindexer ────────────────────────────────
    // The one freshness path (phase-b pb-code-freshness): per-project FS
    // watchers, git-HEAD polls and the rebuild queue write into the graph
    // the tools read. It replaced the 30 s `scip_graph.db` mtime poll.
    // Loaded when a tool first reads it (phase-b pb-code-freshness).
    let merged_graph = crate::LazyScipGraph::deferred(indexes_dir.clone());
    let (reindexer, registry) = crate::freshness::start_reindexer(
        indexes_dir.clone(),
        &merged_graph,
        Arc::clone(&notes_store),
    )
    .await;
    banner.push(format!(
        "Reindexer        ✓  {} registered project(s)",
        registry.entries().len()
    ));
    warn_orphaned_indexes(&indexes_dir, &registry);

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

    // Shared liveness beacon: the coordinator loop stamps it, the
    // status tools read it. Replaces the old one-shot `watcher_active`
    // bool, which could not detect a watcher that died after starting.
    // Mirrors to a sidecar file so a separate CLI process (which reads
    // the same SQLite stores) sees the same liveness.
    let watcher_heartbeat = corpus_engine_watchers::WatcherHeartbeat::with_sidecar(
        stores_dir.join("watcher-heartbeat"),
    );

    // ── Work atlas ──────────────────────────────────────────────
    // Coordination layer for agents sharing this repo. The store is
    // cw-rails', dialed (pb-atlas-kv) — there is no repo-local mesh.db;
    // with cw-rails down every claim operation reports the absence by
    // name. Per spec §10 the origin-remote MUST gate is checked at *boot*:
    // a repo with no origin still gets a serve, but every `declare_scope`
    // call fails with an actionable error rather than silently writing
    // partial state.
    let atlas_mesh_store: Arc<dyn sovereign_work_atlas::ReplicatedKv> = Arc::new(atlas::atlas_kv());
    let atlas_node_id = atlas::atlas_node_id();
    let atlas_store = Arc::new(sovereign_work_atlas::WorkAtlasStore::new(
        Arc::clone(&atlas_mesh_store),
        atlas_node_id,
    ));
    let atlas_cfg_path = sovereign_contracts::rebrand::work_atlas_toml();
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
    let (atlas_repo_root, atlas_repo_id, repo_resolved) =
        match sovereign_work_atlas::resolve_repo_id_allowing_local(&atlas_root) {
            Ok((root, id, source)) => {
                if let Some(caveat) = source.caveat() {
                    tracing::info!(caveat, "work_atlas: using a machine-local repo id");
                }
                (root, id, true)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "work_atlas:repo_id_missing — not inside a git repo, so claims \
                     cannot be scoped to one"
                );
                (atlas_root.clone(), String::new(), false)
            }
        };
    let atlas_branch = sovereign_contracts::git::current_branch(&atlas_repo_root);
    // Work-atlas observer (Phase 2): turns the watched workspace's edits
    // into `work_in_flight` observations. Needs a `repo_id` to scope
    // observations to. An `origin` remote yields the cross-node id;
    // without one the workspace gets a machine-local id instead of being
    // dropped, so a housemate's own project still gets an atlas — the
    // observations simply do not travel, which is the truth about a repo
    // no peer can name. Only "not a git repo at all" leaves the observer
    // unwired.
    let atlas_observer = match (&workspace, repo_resolved) {
        (Some(ws), true) => {
            banner.push(format!("work-atlas observer wired on {}", ws.display()));
            Some(Arc::new(sovereign_work_atlas::AtlasObserver::new(
                Arc::clone(&atlas_store),
                atlas_cfg.clone(),
                Arc::new(sovereign_work_atlas::tools::NullBroadcaster)
                    as Arc<dyn sovereign_work_atlas::tools::ClaimBroadcaster>,
                atlas_repo_root.clone(),
                atlas_repo_id.clone(),
                atlas_branch.clone(),
            )))
        }
        _ => {
            tracing::debug!("work_atlas: no workspace repo, so no atlas observer");
            None
        }
    };
    // GC loop. Its handle is held on the runtime, so dropping the runtime
    // aborts it cleanly when the host stops.
    let atlas_gc =
        sovereign_work_atlas::gc::WorkAtlasGc::new(Arc::clone(&atlas_store), atlas_cfg.clone())
            .spawn();

    // ── Tool set: code's bundles ────────────────────────────────
    // No model is loaded, so `code_search` answers from full text and
    // `read_note_digest` in its header-only fallback, whose banner names
    // the degraded state.
    let mut watcher_tools = crate::bundle::WatcherTools::new(
        Arc::clone(&test_store),
        Arc::clone(&lint_store),
        Arc::clone(&watcher_heartbeat),
    )
    .with_scopes(test_watched_scope, lint_watched_scope);
    if let Some(ref watcher) = test_watcher {
        watcher_tools = watcher_tools.with_test_watcher(Arc::clone(watcher));
    }
    let mut code_intel = crate::bundle::CodeIntelTools::new(index, merged_graph.clone())
        .with_peer_work(Arc::clone(&atlas_store) as Arc<dyn crate::PeerWork>);
    let mut notes_tools = crate::bundle::NotesTools::new(Arc::clone(&notes_store));
    if let Some(ws) = &workspace {
        watcher_tools = watcher_tools.with_workspace_root(ws.clone());
        code_intel = code_intel.with_project_root(ws.clone());
        notes_tools = notes_tools.with_workspace_root(ws.clone());
    }
    // ── The solver ──────────────────────────────────────────────
    // One job table behind `/v1/solve/jobs*` and the three MCP tools
    // (pb-meshapp-solve). Its chat goes to svrn's `/v1/chat/completions`,
    // the base the daemon handed it before the move.
    let solve_jobs = Arc::new(crate::solve_http::SolveJobs::new(
        sovereign_contracts::setup_config::client_daemon_base(),
    ));
    let bundles: Vec<Box<dyn sovereign_contracts::tool_bundle::ToolBundle>> = vec![
        Box::new(crate::bundle::SolveTools::new(Arc::clone(&solve_jobs))),
        Box::new(code_intel),
        Box::new(crate::bundle::ArchTools::new(workspace.clone())),
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

    // ── Start the supervised watcher coordinator ────────────────
    // Collect the registered watchers once; the supervisor holds
    // them so it can rebuild the coordinator on restart without
    // re-deriving anything.
    let mut watchers: Vec<Arc<dyn corpus_engine_watchers::BackgroundWatcher>> = Vec::new();
    if let Some(ref w) = lint_watcher {
        watchers.push(Arc::clone(w) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
    }
    if let Some(ref w) = test_watcher {
        watchers.push(Arc::clone(w) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
    }
    if let Some(obs) = atlas_observer {
        watchers.push(obs as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
    }
    watchers.extend(extra_watchers);

    let watcher_monitor = match (&workspace, watchers.is_empty()) {
        (Some(ws), false) => {
            let debounce_ms = sovereign_cfg
                .lint_runner
                .as_ref()
                .and_then(|c| c.debounce_ms)
                .or_else(|| {
                    sovereign_cfg
                        .test_runner
                        .as_ref()
                        .and_then(|c| c.debounce_ms)
                })
                .unwrap_or(800);
            // The supervisor performs the initial start AND self-heals:
            // if the coordinator loop dies or its heartbeat freezes, it
            // rebuilds and restarts (bounded backoff). Holding the monitor
            // task handle keeps the watcher alive for the host's life.
            let supervisor = watcher_supervisor::WatcherSupervisor::new(
                watchers,
                vec![ws.clone()],
                debounce_ms,
                Arc::clone(&watcher_heartbeat),
            );
            let monitor = supervisor.spawn();
            match &monitor {
                Some(_) => banner.push(format!(
                    "Watcher supervisor live on {} (self-healing)",
                    ws.display()
                )),
                None => banner.push(format!(
                    "warning: could not start watcher on {}",
                    ws.display()
                )),
            }
            monitor
        }
        _ => {
            tracing::debug!(
                "code: no workspace or no watcher registered — lint/test watcher disabled"
            );
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
    // The editor door reads the graphs under code's indexes root and dials
    // serve on this host for the model lane.
    let edit_routes = crate::edit_predictions::router(crate::edit_predictions::EditDoor::new(
        grammar,
        indexes_dir,
        sovereign_turn_client::serve_self::default_serve_base(),
    ));
    Ok(CodeFace {
        mcp,
        tools,
        routes: crate::project_http::project_router(Arc::clone(&reindexer))
            .merge(crate::solve_http::solve_router(solve_jobs)),
        edit_routes,
        banner,
        runtime: CodeRuntime {
            _reindexer: reindexer,
            _watcher_monitor: watcher_monitor,
            _atlas_gc: atlas_gc,
            lint_watcher,
            test_watcher,
        },
    })
}
/// Surface orphaned per-corpus SCIP indexes at startup.
///
/// On an upgrade from a pre-registry sovereign, `~/.svrnmesh/
/// indexes/<corpus>/scip_graph.db` will often exist even though
/// `projects.json` is empty. The daemon can't safely auto-register
/// those — we don't know which filesystem path each one came
/// from, and guessing could point the FS watcher at the wrong
/// directory. Instead, log a one-shot hint so the operator knows
/// to re-register each repo manually.
fn warn_orphaned_indexes(
    indexes_dir: &Path,
    registry: &sovereign_contracts::watcher_projects::Registry,
) {
    let Ok(entries) = std::fs::read_dir(indexes_dir) else {
        return;
    };
    let registered: std::collections::HashSet<&str> = registry
        .entries()
        .iter()
        .map(|e| e.corpus_id.as_str())
        .collect();
    let mut orphans: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        // Skip flat files (project_docs.db, lint_results.db, etc.).
        if !entry.path().is_dir() {
            continue;
        }
        let scip = entry.path().join("scip_graph.db");
        if !scip.exists() {
            continue;
        }
        if registered.contains(name.as_str()) {
            continue;
        }
        orphans.push(name);
    }
    if orphans.is_empty() {
        return;
    }
    eprintln!();
    eprintln!(
        "  \u{26a0} Found {} SCIP index(es) on disk with no registry entry:",
        orphans.len()
    );
    for o in &orphans {
        eprintln!("      {o}");
    }
    eprintln!(
        "  Run `svrn project register` in each repo to resume watching.\n\
         (The daemon won't guess the filesystem path for you — bad guesses\n\
         point the FS watcher at the wrong directory.)"
    );
    eprintln!();
}
