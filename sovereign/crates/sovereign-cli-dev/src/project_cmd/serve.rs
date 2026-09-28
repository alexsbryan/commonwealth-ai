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

    eprintln!();
    eprintln!("  Stores:");

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

    // ── Code, composed: result stores, Reindexer, watchers, work atlas,
    // code's bundles and its MCP dispatch (`crate::code_face`, the one the
    // stock binary composes too). The docs indexer is this server's own.
    let docs_watchers = docs_store
        .iter()
        .map(|ds| {
            Arc::new(corpus_engine_watchers::ProjectIndexWatcher::new(
                Arc::clone(ds),
                repo_root.clone(),
            )) as Arc<dyn corpus_engine_watchers::BackgroundWatcher>
        })
        .collect();
    let face = match crate::code_face::compose(crate::code_face::CodeParts {
        indexes_dir: data_dir.clone(),
        stores_dir: data_dir.clone(),
        notes: Arc::clone(&notes_store),
        index: Arc::clone(&engine) as Arc<dyn sovereign_code::CodeIndexSource>,
        workspace: Some(repo_root.clone()),
        sovereign_dir: Some(sovereign_dir.clone()),
        session_prefix: "serve",
        extra_watchers: docs_watchers,
    })
    .await
    {
        Ok(face) => face,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    for line in &face.banner {
        eprintln!("  {line}");
    }

    let tools = Arc::clone(&face.tools);
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

    // The notifier behind `GET /mcp`, wired to a SpecWatcher rooted at
    // repo_root: when the user creates `.sovereign/features/foo/spec.md`
    // (or edits ARCHITECTURE.md), every connected MCP agent sees
    // `notifications/tools/list_changed` within ~100ms and refetches
    // `tools/list` — surfacing `spec` and `drift` without a restart.
    let notifier = host_kit::mcp::http::McpNotifier::new();
    let watcher_notifier = notifier.clone();
    let _spec_watcher =
        match sovereign_code::spec_watcher::SpecWatcher::start(&repo_root, move || {
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
    // Code's dispatcher, behind the kit's HTTP+SSE framing. `listChanged`
    // is advertised because the spec watcher above pushes it.
    let dispatcher = face.mcp.list_changed(true);
    // Held for the server's life: dropping it stops the Reindexer, the
    // watchers and the atlas GC.
    let _code_runtime = face.runtime;
    let app = host_kit::mcp::http::routes(Arc::new(dispatcher), notifier)
        .route("/mcp/stats", axum::routing::get(super::mcp_host::mcp_stats))
        .merge(face.routes)
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
