// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project serve` — the lightweight MCP server for locally-indexed
//! projects (no model required), plus `scip_graph_reloader`, the 30s poller
//! that hot-swaps the SCIP call graph on disk changes. `load_merged_graph` /
//! `snapshot_graph_mtimes` stay re-exported from `super`. Split out of
//! `project_cmd` (2026-07-13); pure move. Shared plumbing via `use super::*`.

use super::*;

const HELP_SERVE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project serve",
    summary: "Start a lightweight MCP server for locally-indexed projects (no model required).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn project serve [--port <port>] [--data-dir <dir>]\n    \
             [--sovereign-dir <dir>]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--port <port>", "Listen port (default: 9741)"),
            (
                "--data-dir <dir>",
                "Index directory (default: ~/.sovereign/indexes)",
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

    // Check whether the daemon already owns :9741. If so, running
    // the legacy in-process server on top collides (silent bind
    // failure) and degrades the user's MCP surface. Refuse to
    // start and redirect to the daemon workflow.
    if daemon_is_running().await {
        eprintln!();
        eprintln!("  `svrn project serve` is superseded.");
        eprintln!();
        eprintln!("  The running sovereign daemon already serves MCP on :9741 and");
        eprintln!("  owns freshness (FS watcher + git HEAD poll + startup catch-up).");
        eprintln!("  There's no need to run a second server on top of it.");
        eprintln!();
        eprintln!("  To have the daemon watch this project:");
        eprintln!("    sovereign project register");
        eprintln!();
        eprintln!("  To inspect watcher state:");
        eprintln!("    sovereign project watch status");
        eprintln!();
        eprintln!("  If you really want the legacy in-process server (e.g. the daemon");
        eprintln!("  is broken and you need a fallback), stop the daemon first:");
        eprintln!("    launchctl stop com.svrnmesh.daemon   # macOS");
        eprintln!("    systemctl --user stop sovereign       # Linux");
        return 1;
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
            Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; corpus_engine::DEFAULT_EMBED_DIM])
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

    eprintln!();
    eprintln!("  Call graph:");

    let (initial_graph, _summary) = load_merged_graph(&data_dir, true).await;
    let merged_graph: sovereign_tools::ScipGraphHandle =
        Arc::new(ArcSwap::from_pointee(initial_graph));
    let health_checker = Arc::new(sovereign_tools::IndexHealthChecker::new(Arc::clone(
        &merged_graph,
    )));

    // Spawn the background reloader: every 30s, stat each scip_graph.db,
    // and if any mtime changed (or a file appeared/disappeared) rebuild the
    // merged graph and swap it in atomically. Tools grab `load_full()` per
    // query so the swap is lock-free.
    {
        let handle = Arc::clone(&merged_graph);
        let dir = data_dir.clone();
        tokio::spawn(async move {
            scip_graph_reloader(handle, dir).await;
        });
    }

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

    let test_store =
        match corpus_engine_watchers::TestResultStore::open(&data_dir.join("test_results.db")) {
            Ok(s) => {
                eprintln!("  test_results.db  ✓");
                Arc::new(s)
            }
            Err(e) => {
                eprintln!("  warning: could not open test results DB: {e}");
                Arc::new(
                    corpus_engine_watchers::TestResultStore::open(std::path::Path::new(":memory:"))
                        .expect("in-memory test store"),
                )
            }
        };

    let lint_store =
        match corpus_engine_watchers::LintResultStore::open(&data_dir.join("lint_results.db")) {
            Ok(s) => {
                eprintln!("  lint_results.db  ✓");
                Arc::new(s)
            }
            Err(e) => {
                eprintln!("  warning: could not open lint results DB: {e}");
                Arc::new(
                    corpus_engine_watchers::LintResultStore::open(std::path::Path::new(":memory:"))
                        .expect("in-memory lint store"),
                )
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
            let pointer_dir = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".sovereign");
            let _ = std::fs::create_dir_all(&pointer_dir);
            let _ = std::fs::write(
                pointer_dir.join("active_notes_db"),
                notes_db_path.to_string_lossy().as_bytes(),
            );
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("  warning: could not open notes DB: {e}");
            Arc::new(
                corpus_engine_notes::NoteStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory notes store"),
            )
        }
    };

    // ── Feature store (ATOS charters + milestones) ─────────────
    let features_db_path = sovereign_dir.join("features.db");
    let features_store = match corpus_engine_atos::FeatureStore::open(&features_db_path) {
        Ok(s) => {
            eprintln!("  features.db      ✓");
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("  warning: could not open features DB: {e}");
            Arc::new(
                corpus_engine_atos::FeatureStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory features store"),
            )
        }
    };

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

    // ── Register tools ──────────────────────────────────────────

    let mut tools = sovereign_core::ToolRegistry::new();
    tools.register(Box::new(
        sovereign_tools::SymbolLookupTool::new(Arc::clone(&engine), Arc::clone(&merged_graph))
            .with_health_checker(Arc::clone(&health_checker)),
    ));
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
    // Architecture observability (quality program) — report + posture.
    tools.register(Box::new(sovereign_tools::ArchReportTool::new()));
    tools.register(Box::new(sovereign_tools::ArchPostureTool::new()));

    // ── Test / lint watcher tools ───────────────────────────────

    {
        let mut tool = sovereign_tools::TestStatusTool::new(Arc::clone(&test_store))
            .with_watcher_active(Arc::clone(&watcher_active_flag));
        if let Some(scope) = test_watched_scope {
            tool = tool.with_watched_scope(scope);
        }
        tools.register(Box::new(tool));
    }
    if let Some(ref watcher) = test_watcher {
        tools.register(Box::new(sovereign_tools::RunTestsTool::new(Arc::clone(
            watcher,
        ))));
    }
    tools.register(Box::new(sovereign_tools::GetRunOutputTool::new(
        Arc::clone(&test_store),
    )));

    {
        let mut tool = sovereign_tools::LintStatusTool::new(Arc::clone(&lint_store))
            .with_watcher_active(Arc::clone(&watcher_active_flag))
            .with_workspace_root(repo_root.clone());
        if let Some(scope) = lint_watched_scope {
            tool = tool.with_watched_scope(scope);
        }
        tools.register(Box::new(tool));
    }
    tools.register(Box::new(
        sovereign_tools::DriftPostureTool::new().with_workspace_root(repo_root.clone()),
    ));
    tools.register(Box::new(sovereign_tools::GetLintOutputTool::new(
        Arc::clone(&lint_store),
    )));

    // ── Agent partnership tools (notes, blast radius, project context) ──

    tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(
        &notes_store,
    ))));
    tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    // Work atlas — coordination layer for agents sharing this repo.
    // The serve path runs the GC loop and exposes the three claim
    // tools alongside the code-intel surface. Per spec §10 the
    // origin-remote MUST gate is checked at *boot*: a repo with no
    // origin still gets a serve, but every `declare_scope` call
    // fails with an actionable error rather than silently writing
    // partial state.
    let atlas_mesh_db = sovereign_dir.join("mesh.db");
    if let Some(parent) = atlas_mesh_db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let atlas_mesh_store = Arc::new(
        commonwealth_state::MeshStore::open(&atlas_mesh_db)
            .or_else(|_| commonwealth_state::MeshStore::in_memory())
            .expect("work atlas mesh store"),
    );
    let atlas_node_id = sovereign_mesh::persist::load_or_generate_self_node_id(&data_dir);
    let atlas_store = Arc::new(sovereign_work_atlas::WorkAtlasStore::new(
        Arc::clone(&atlas_mesh_store),
        atlas_node_id,
    ));
    let atlas_cfg_path = dirs::home_dir()
        .map(|h| h.join(".sovereign").join("work-atlas.toml"))
        .unwrap_or_else(|| sovereign_dir.join("work-atlas.toml"));
    let atlas_cfg = sovereign_work_atlas::WorkAtlasConfig::load_or_default(&atlas_cfg_path)
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                path = %atlas_cfg_path.display(),
                "work_atlas: failed to load config, falling back to defaults"
            );
            sovereign_work_atlas::WorkAtlasConfig::defaults()
        });
    let atlas_broadcaster: Arc<dyn sovereign_work_atlas::tools::ClaimBroadcaster> =
        Arc::new(sovereign_work_atlas::tools::NullBroadcaster);
    let (atlas_repo_root, atlas_repo_id) = match sovereign_work_atlas::resolve_repo_id(&repo_root) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "work_atlas:repo_id_missing — declare_scope will reject calls"
            );
            (repo_root.clone(), String::new())
        }
    };
    let atlas_branch = crate::code_cmd::current_branch(&atlas_repo_root);
    tools.register(Box::new(
        sovereign_work_atlas::tools::DeclareScopeTool::new(
            Arc::clone(&atlas_store),
            atlas_cfg.clone(),
            Arc::clone(&atlas_broadcaster),
            atlas_repo_root.clone(),
            atlas_repo_id.clone(),
            atlas_branch.clone(),
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
    // GC loop. Holds onto the handle so dropping it aborts cleanly
    // when serve terminates.
    let _atlas_gc_handle =
        sovereign_work_atlas::gc::WorkAtlasGc::new(Arc::clone(&atlas_store), atlas_cfg.clone())
            .spawn();

    tools.register(Box::new(
        sovereign_tools::BlastRadiusTool::new(Arc::clone(&merged_graph))
            .with_project_root(repo_root.clone())
            .with_health_checker(Arc::clone(&health_checker))
            .with_atlas(Arc::clone(&atlas_store)),
    ));
    if let Some(ref ds) = docs_store {
        tools.register(Box::new(
            sovereign_tools::ProjectContextTool::new(Arc::clone(ds))
                .with_features(Arc::clone(&features_store)),
        ));
    }

    // ── ATOS feature management ─────────────────────────────────
    tools.register(Box::new(sovereign_tools::ProvisionFeatureTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::ArchiveFeatureTool::new(
        Arc::clone(&features_store),
    )));
    tools.register(Box::new(sovereign_tools::ReadNoteByIdTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::PromoteNoteTool::new(Arc::clone(
        &notes_store,
    ))));
    // ReadNoteDigestTool runs in fallback (header-only) mode here —
    // `svrn project serve` doesn't load a model, so the Fast-slot
    // summarization path is unavailable. The banner in the fallback
    // digest makes the degraded state visible to agents. The daemon
    // binary wires inference in via `.with_inference(...)`.
    tools.register(Box::new(sovereign_tools::ReadNoteDigestTool::new(
        Arc::clone(&notes_store),
    )));
    tools.register(Box::new(sovereign_tools::RecordAtosEventTool::new(
        Arc::clone(&features_store),
    )));
    // atos_plan_emit intentionally NOT registered — see runtime
    // tools_cmd/registry.rs for rationale (markdown plan path
    // replaced structured-JSON path).
    tools.register(Box::new(sovereign_tools::WriteRedteamFindingTool::new(
        Arc::clone(&notes_store),
    )));

    // ── Session reflection (feedback loop) ─────────────────────────────
    tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
        Arc::clone(&notes_store),
    )));

    // ── Doc path checker ────────────────────────────────────────────────
    tools.register(Box::new(
        sovereign_tools::CheckDocPathsTool::new().with_project_root(repo_root.clone()),
    ));

    // ── DESIGN.md structural signals ────────────────────────────────────
    //
    // Project-scoped: bound to this repo's DESIGN.md by default, so the
    // agent can call `design_signals_extract()` with no args and get the
    // right file. Absolute paths still work — the tool resolves them
    // verbatim, bypassing project_root.
    tools.register(Box::new(
        sovereign_tools::DesignSignalsExtractTool::new().with_project_root(repo_root.clone()),
    ));

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
    // calls from the same sovereign project serve invocation.
    let mcp_session_id = format!("serve-{}", uuid::Uuid::new_v4());

    // Phase 5: standalone `svrn serve` always knows its project
    // root (resolved above as `repo_root`). Pass it as the
    // FeatureRoot so `tools/list` consults
    // `.sovereign/features/*/spec.md` and only advertises the
    // spec-gated tools (`spec`, `drift`) when a spec exists. Cache
    // is process-global so repeated `tools/list` calls amortise the
    // stat against a 1-second TTL.
    let feature_root = sovereign_mesh::mcp_router::FeatureRoot::new(Some(repo_root.clone()));
    // Phase 5b: build a notifier and wire it to a SpecWatcher rooted
    // at repo_root. The watcher's on_change closure publishes to the
    // notifier; the notifier fans the JSON-RPC frame out to every
    // subscribed SSE client. Result: when the user creates
    // `.sovereign/features/foo/spec.md` (or edits ARCHITECTURE.md),
    // every connected MCP agent sees `notifications/tools/list_changed`
    // within ~100ms and refetches `tools/list` — surfacing `spec` and
    // `drift` without a restart.
    let notifier = sovereign_mesh::mcp_router::McpNotifier::new();
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
    let app = sovereign_mesh::mcp_router::mcp_router(
        tools,
        Arc::clone(&notes_store),
        mcp_session_id,
        feature_root,
        notifier,
    );

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind to {bind_addr}: {e}");
            return 1;
        }
    };

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

// ─── SCIP graph loading & hot-reload ──────────────────────────

/// Poll `data_dir` for SCIP graph file changes every 30 seconds. On any
/// change, rebuild the merged graph out-of-band and atomically swap it
/// into `handle`. Tools (FindCalleesTool, FindCallersTool) pick up the
/// new graph on their next `load_full()`.
async fn scip_graph_reloader(handle: sovereign_tools::ScipGraphHandle, data_dir: PathBuf) {
    const POLL_INTERVAL: Duration = Duration::from_secs(30);

    let mut last_seen = snapshot_graph_mtimes(&data_dir);
    tracing::debug!(
        watched = last_seen.len(),
        "scip reloader: polling scip_graph.db files every 30s"
    );

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        let current = snapshot_graph_mtimes(&data_dir);
        if current == last_seen {
            continue;
        }

        tracing::info!(
            prev_graphs = last_seen.len(),
            current_graphs = current.len(),
            "scip reloader: change detected, rebuilding merged graph"
        );

        let (fresh, summary) = load_merged_graph(&data_dir, false).await;
        handle.store(Arc::new(fresh));
        last_seen = current;

        tracing::info!(
            graphs = summary.graphs_found,
            symbols = summary.total_symbols,
            edges = summary.total_refs,
            "scip reloader: merged graph swapped"
        );
    }
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
