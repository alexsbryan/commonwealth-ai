// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project serve` — the lightweight MCP server for locally-indexed
//! projects (no model required). Its graph stays fresh through the code
//! program's Reindexer (`sovereign_code::freshness`), whose `/v1/projects/*`
//! it mounts beside `/mcp`. Split out of
//! `project_cmd` (2026-07-13); pure move. Shared plumbing via `use super::*`.

use super::*;
use host_kit::shell::guard::LoopbackRouter as _;

const HELP_SERVE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn code mcp",
    summary: "Serve code intelligence over MCP for locally-indexed projects: no model, no \
              daemon, no mesh. Also spelled `svrn serve` and `svrn project serve`.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn code mcp [--port <port>] [--data-dir <dir>]\n    \
             [--sovereign-dir <dir>]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--port <port>", "Listen port (default: 9741)"),
            (
                "--data-dir <dir>",
                "Index directory (default: ~/.svrnmesh/indexes)",
            ),
            (
                "--sovereign-dir <dir>",
                "Path to .sovereign/ (default: nearest ancestor with .sovereign/)",
            ),
        ]),
    ],
};

// ─── Serve ───────────────────────────────────────────────────

pub(crate) async fn cmd_serve(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_SERVE);
        return 0;
    }

    let mut port: u16 = 9741;
    let mut data_dir: Option<PathBuf> = None;
    let mut sovereign_dir_arg: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<u16>() {
                        Ok(p) => port = p,
                        Err(_) => {
                            eprintln!("error: --port must be a number");
                            return 1;
                        }
                    }
                }
            }
            "--data-dir" => {
                i += 1;
                data_dir = args.get(i).map(PathBuf::from);
            }
            "--sovereign-dir" => {
                i += 1;
                sovereign_dir_arg = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    if !data_dir.exists() {
        eprintln!(
            "error: index directory does not exist: {}",
            data_dir.display()
        );
        eprintln!("Run `svrn project init` in at least one project first.");
        return 1;
    }

    eprintln!("  Sovereign Code Intelligence MCP Server");
    eprintln!("  {}", "─".repeat(54));

    // ── Build CorpusEngine (zero-vector, no model) ──────────────

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async {
            Ok::<Vec<f32>, corpus_index::Error>(vec![0.0; corpus_index::types::DEFAULT_EMBED_DIM])
        })
    });
    let recipes_dir = data_dir.clone();
    let engine = Arc::new(
        CorpusEngine::new(recipes_dir, data_dir.clone(), embed)
            .with_embedding_model(&configured_embed_model_name()),
    );

    // List discovered indexes.
    match engine.installed_indexes().await {
        Ok(indexes) => {
            let code_indexes: Vec<_> = indexes
                .iter()
                // Accept any index with content — the model string is
                // informational after setup no longer locks everyone to a
                // single default.
                .filter(|i| i.chunk_count > 0)
                .collect();
            if code_indexes.is_empty() {
                eprintln!("  warning: no indexes found in {}", data_dir.display());
            } else {
                eprintln!("  Corpora:");
                for info in &code_indexes {
                    eprintln!(
                        "    \u{2713} {} ({} symbols)",
                        info.corpus_id, info.chunk_count
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("  warning: could not list indexes: {e}");
        }
    }

    // ── Discover and merge SCIP graphs ──────────────────────────

    // Loaded when a tool first reads it (phase-b pb-code-freshness).
    eprintln!();
    eprintln!(
        "  Call graph:       loads on first read from {}",
        data_dir.display()
    );
    let merged_graph = sovereign_code::LazyScipGraph::deferred(data_dir.clone());


    // ── Repo root + sovereign config ────────────────────────────
    //
    // Priority: nearest ancestor with .sovereign/ > git root > cwd.
    // This allows `svrn project serve` to be launched from a monorepo
    // root that is not itself a git repository.

    let cwd = std::env::current_dir()
        .ok()
        .unwrap_or_else(|| PathBuf::from("."));
    let sovereign_dir = sovereign_dir_arg
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .or_else(|| find_sovereign_dir(&cwd))
        .or_else(|| find_repo_root().map(|r| r.join(".sovereign")))
        .unwrap_or_else(|| cwd.join(".sovereign"));
    let repo_root = sovereign_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cwd.clone());
    let sovereign_cfg = corpus_engine::SovereignConfig::load_or_default(&sovereign_dir);

    // ── Open result stores (SQLite, always-on) ──────────────────
    eprintln!();
    eprintln!("  Stores:");

    // A store that does not open is refused by path, never replaced by an
    // in-memory one whose writes vanish at exit (principle 6).
    let test_results_path = data_dir.join("test_results.db");
    let test_store = match corpus_engine_watchers::TestResultStore::open(&test_results_path) {
        Ok(s) => {
            eprintln!("  test_results.db  ✓");
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", test_results_path.display());
            return 1;
        }
    };

    let lint_results_path = data_dir.join("lint_results.db");
    let lint_store = match corpus_engine_watchers::LintResultStore::open(&lint_results_path) {
        Ok(s) => {
            eprintln!("  lint_results.db  ✓");
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", lint_results_path.display());
            return 1;
        }
    };

    // ── Notes store ─────────────────────────────────────────────

    let notes_db_path = sovereign_dir.join("notes.db");
    let notes_store = match corpus_engine_notes::NoteStore::open(&notes_db_path) {
        Ok(s) => {
            eprintln!("  notes.db         ✓");
            // Write a pointer file so `svrn reflect` can find this
            // database from any working directory, regardless of where the
            // user invokes it from.
            let pointer_dir = sovereign_cli_shared::dirs::sovereign_root();
            let _ = std::fs::create_dir_all(&pointer_dir);
            let _ = std::fs::write(
                pointer_dir.join("active_notes_db"),
                notes_db_path.to_string_lossy().as_bytes(),
            );
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", notes_db_path.display());
            return 1;
        }
    };

    // ── Freshness: the Reindexer ────────────────────────────────
    // The one freshness path (phase-b pb-code-freshness): per-project FS
    // watchers, git-HEAD polls and the rebuild queue write into the graph
    // the tools read. It replaced the 30 s `scip_graph.db` mtime poll.
    let (reindexer, registry) = sovereign_code::freshness::start_reindexer(
        data_dir.clone(),
        &merged_graph,
        Arc::clone(&notes_store),
    )
    .await;
    eprintln!(
        "  Reindexer        ✓  {} registered project(s)",
        registry.entries().len()
    );

    // Print any open todos from previous sessions at startup.
    if let Ok(todos) = notes_store.open_todos(5).await {
        if !todos.is_empty() {
            eprintln!();
            eprintln!(
                "  {} open todo{} from previous sessions:",
                todos.len(),
                if todos.len() == 1 { "" } else { "s" }
            );
            for t in &todos {
                let preview: String = t.content.chars().take(80).collect();
                eprintln!("    [todo] {preview}");
            }
            eprintln!("  Use read_notes to retrieve full context.");
        }
    }

    // ── Project docs store ───────────────────────────────────────

    let docs_store =
        match corpus_engine_notes::ProjectDocsStore::open(&data_dir.join("project_docs.db")) {
            Ok(s) => {
                let store = Arc::new(s);
                // Index on first run without blocking serve startup.
                if store.is_empty().await.unwrap_or(true) {
                    let s2 = Arc::clone(&store);
                    let root = repo_root.clone();
                    tokio::spawn(async move {
                        let files = corpus_engine_notes::find_markdown_files(&root);
                        let mut count = 0usize;
                        for f in &files {
                            count += s2.index_file(f, &root).await.unwrap_or(0);
                        }
                        if count > 0 {
                            tracing::info!(
                                "indexed {} doc chunks from {} md files",
                                count,
                                files.len()
                            );
                        }
                    });
                }
                eprintln!("  project_docs.db  ✓");
                Some(store)
            }
            Err(e) => {
                eprintln!("  warning: could not open project docs DB: {e}");
                None
            }
        };

    // ── Build background watchers ───────────────────────────────

    // Shared run slot so lint + test cargo invocations serialize
    // instead of double-spawning on every debounced edit flush.
    let run_slot = Arc::new(tokio::sync::Semaphore::new(1));

    let test_watcher: Option<Arc<corpus_engine_watchers::TestWatcher>> =
        sovereign_cfg.test_runner.as_ref().map(|cfg| {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() {
                    p
                } else {
                    repo_root.join(p)
                }
            });
            eprintln!(
                "  test_runner      ✓  {}",
                cfg.command.chars().take(60).collect::<String>()
            );
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
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() {
                    p
                } else {
                    repo_root.join(p)
                }
            });
            eprintln!(
                "  lint_runner      ✓  {}",
                cfg.command.chars().take(60).collect::<String>()
            );
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

    if test_watcher.is_none() && lint_watcher.is_none() {
        eprintln!(
            "  warning: no watchers configured — add [test_runner] / [lint_runner] \
             to {}",
            sovereign_dir.join("sovereign.toml").display()
        );
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
    let (atlas_repo_root, atlas_repo_id) =
        match sovereign_work_atlas::resolve_repo_id_allowing_local(&repo_root) {
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
                (repo_root.clone(), String::new())
            }
        };
    let atlas_branch = crate::code_cmd::current_branch(&atlas_repo_root);
    // GC loop. Holds onto the handle so dropping it aborts cleanly
    // when serve terminates.
    let _atlas_gc_handle =
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
        repo_root.clone(),
    )
    .with_scopes(test_watched_scope, lint_watched_scope);
    if let Some(ref watcher) = test_watcher {
        watcher_tools = watcher_tools.with_test_watcher(Arc::clone(watcher));
    }
    let bundles: Vec<Box<dyn sovereign_contracts::tool_bundle::ToolBundle>> = vec![
        Box::new(
            sovereign_code::bundle::CodeIntelTools::new(
                Arc::clone(&engine) as Arc<dyn sovereign_code::CodeIndexSource>,
                merged_graph.clone(),
            )
            .with_project_root(repo_root.clone())
            .with_peer_work(Arc::clone(&atlas_store) as Arc<dyn sovereign_code::PeerWork>),
        ),
        Box::new(sovereign_code::bundle::ArchTools::new(repo_root.clone())),
        Box::new(watcher_tools),
        Box::new(sovereign_code::bundle::NotesTools::new(Arc::clone(
            &notes_store,
        ))),
        Box::new(sovereign_work_atlas::tools::WorkAtlasTools::new(
            atlas_store,
            atlas_cfg,
            Arc::new(sovereign_work_atlas::tools::NullBroadcaster),
            atlas_repo_root,
            atlas_repo_id,
            atlas_branch,
        )),
    ];
    let mut tools = sovereign_contracts::ToolRegistry::new();
    for report in sovereign_contracts::tool_bundle::install(&mut tools, &bundles).await {
        eprintln!("  {}", report.summary());
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
    if let Some(ref ds) = docs_store {
        let pw =
            corpus_engine_watchers::ProjectIndexWatcher::new(Arc::clone(ds), repo_root.clone());
        coordinator.register(Arc::new(pw) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>);
    }

    let _coordinator_handle = if !coordinator.registered_ids().is_empty() {
        match coordinator.start(vec![repo_root.clone()]).await {
            Ok(handle) => {
                eprintln!("  Watcher started (watching {})", repo_root.display());
                watcher_active_flag.store(true, std::sync::atomic::Ordering::Release);
                Some(handle)
            }
            Err(e) => {
                eprintln!("  warning: could not start watcher: {e}");
                None
            }
        }
    } else {
        None
    };

    let tools = Arc::new(tools);
    eprintln!();
    eprintln!("  Tools: {} registered", tools.count());

    // ── Start MCP HTTP server ───────────────────────────────────

    // `:9741/mcp` is the one MCP address (phase-b-33). Whichever of this
    // server and the svrn daemon binds it second refuses by name; neither
    // stops the other (principle 12).
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = match host_kit::shell::bind_with_retry(addr, "code MCP").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            if e.kind() == std::io::ErrorKind::AddrInUse {
                eprintln!(
                    "  Port {port} is taken — the svrn daemon serves /mcp there too. Use it, \
                     stop it, or pass --port."
                );
            }
            return 1;
        }
    };

    let bind_addr = format!("127.0.0.1:{port}");
    eprintln!("  Listening on http://{bind_addr}/mcp");
    eprintln!();
    eprintln!("  {}", "─".repeat(54));
    eprintln!("  Ready. Configure Claude Code with:");
    eprintln!();
    eprintln!("    {{");
    eprintln!("      \"mcpServers\": {{");
    eprintln!("        \"sovereign\": {{");
    eprintln!("          \"type\": \"http\",");
    eprintln!("          \"url\": \"http://localhost:{port}/mcp\"");
    eprintln!("        }}");
    eprintln!("      }}");
    eprintln!("    }}");
    eprintln!();

    // Stable session ID for this server run — used by tool_call_log to group
    // calls from the same server invocation.
    let mcp_session_id = format!("serve-{}", uuid::Uuid::new_v4());

    // The notifier behind `GET /mcp`, wired to a SpecWatcher rooted at
    // repo_root: when the user creates `.sovereign/features/foo/spec.md`
    // (or edits ARCHITECTURE.md), every connected MCP agent sees
    // `notifications/tools/list_changed` within ~100ms and refetches
    // `tools/list` — surfacing `spec` and `drift` without a restart.
    let notifier = host_kit::mcp::http::McpNotifier::new();
    let watcher_notifier = notifier.clone();
    let _spec_watcher =
        match sovereign_tools::spec_watcher::SpecWatcher::start(&repo_root, move || {
            watcher_notifier.notify_tools_list_changed()
        }) {
            Ok(w) => Some(w),
            Err(e) => {
                // Don't fail the whole serve over a non-critical watcher;
                // fall back to TTL-only cache freshness. Log so the
                // operator sees why list_changed events aren't firing.
                tracing::warn!(
                    error = %e,
                    root = %repo_root.display(),
                    "spec_watcher: failed to start; falling back to 1s TTL — \
                     spec edits will surface within a second instead of \
                     immediately"
                );
                None
            }
        };
    // The kit's dispatcher over code's registry and call log, behind the
    // kit's HTTP+SSE framing. `listChanged` is advertised because the spec
    // watcher above pushes it.
    let dispatcher = host_kit::mcp::McpDispatcher::new(
        "sovereign-code",
        env!("CARGO_PKG_VERSION"),
        super::mcp_host::CodeTools {
            tools: Arc::clone(&tools),
            session_id: mcp_session_id.clone(),
            feature_root: repo_root.clone(),
        },
        super::mcp_host::CodeCallLog {
            notes: Arc::clone(&notes_store),
            session_id: Arc::new(mcp_session_id),
            matcher: Arc::new(
                corpus_engine_notes::mining::patterns::ToolPatternMatcher::new(Arc::clone(
                    &notes_store,
                )),
            ),
        },
    )
    .list_changed(true);
    let app = host_kit::mcp::http::routes(Arc::new(dispatcher), notifier)
        .route("/mcp/stats", axum::routing::get(super::mcp_host::mcp_stats))
        .merge(sovereign_code::project_http::project_router(reindexer))
        .localhost_only()
        .layer(axum::Extension(tools))
        .layer(tower_http::cors::CorsLayer::permissive());

    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    if let Err(e) = axum::serve(listener, service).await {
        eprintln!("error: server failed: {e}");
        return 1;
    }
    // _spec_watcher dropped here on serve exit — releases the FS
    // backend and stops the dispatch task. Made explicit by
    // shadowing in the let-binding above.
    drop(_spec_watcher);

    0
}

// ─── sovereign project found (Phase 6: retired) ─────────────
//
// Phase 6 of the CLI refactor retires the structured "founding"
// conversation. The default flow is now:
//
//   sovereign init   →   write `.sovereign/features/<id>/spec.md`
//                    →   git commit  (= approval; see approval_gate)
//                    →   work
//
// Founding is implicit — the first `init` + commit is sufficient.
// `svrn charter` remains as the explicit team-conventions
// surface for projects that want one. The legacy questionnaire
// flow (Stage 1/2 elicitation, fault-line selection, charter
// composition, approval gate) lives on under
// [`crate::found`] for `svrn project amend` and the audit's
// charter-hash check; only the user-facing entry point is gone.
//
