//! `sovereign daemon run` — the hidden subcommand that launchd/systemd
//! calls to actually run the embedded Commonwealth daemon in the
//! foreground. Humans don't invoke this directly; they go through
//! `sovereign setup` (which registers the service) and then let the
//! service manager keep it alive.
//!
//! Responsibilities:
//! 1. Read `~/.config/sovereign/config.toml` for model paths + ports.
//! 2. Build an `EmbeddedLlamaCpp` inference provider from the three
//!    GGUF slots (primary / fast / embed).
//! 3. Build a `ToolRegistry` + `NoteStore` so `/mcp/*` has tools.
//! 4. Build `EmbeddedDaemon` with `.set_inference_provider()` and
//!    `.set_mcp()` so `:9741` serves both `/v1/*` and `/mcp/*`.
//! 5. `try_resume()` the persisted mesh; on first run where no
//!    `mesh.json` exists, create a silent "solo" mesh so the listener
//!    comes up. `sovereign mesh rotate` (future) can later print a
//!    shareable join key.
//! 6. Block on `tokio::signal::ctrl_c()` so the service manager
//!    controls lifecycle.

use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine::{CorpusEngine, EmbedFn, NoteStore};
use sovereign_core::model_family::ModelFamily;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::ToolRegistry;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_mesh::admin_http::ProviderFactory;

/// Entry point routed from `main.rs` when the user invokes
/// `sovereign daemon run`. Any other `daemon` subcommand prints usage.
pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    match args.first().map(String::as_str) {
        Some("run") => run_daemon(&args[1..]).await,
        Some("restart") => restart_daemon().await,
        Some("reload") => reload_daemon().await,
        Some("status") => status_daemon().await,
        Some(other) => {
            eprintln!("error: unknown daemon subcommand '{other}'");
            crate::util::help::print(&HELP);
            1
        }
        None => {
            // Bare `sovereign daemon` — user probably wanted help.
            crate::util::help::print(&HELP);
            1
        }
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign daemon",
    summary: "Long-running service managed by launchd (macOS) or systemd (Linux).",
    sections: &[
        crate::util::help::HelpSection::Usage("sovereign daemon <subcommand>"),
        crate::util::help::HelpSection::Subcommands(&[
            ("run",     "Run in the foreground; exits on SIGINT/SIGTERM. Normally the OS service manager invokes this, not you."),
            ("status",  "Report whether the daemon is running and answering on :9741."),
            ("reload",  "Apply config changes without a restart (POST /v1/admin/reload). Use this after editing model paths in ~/.config/sovereign/config.toml."),
            ("restart", "Hard-restart via launchctl / systemctl. Drops in-flight requests. Use when a model/port/data_dir change requires a full rebind or the daemon is wedged."),
        ]),
        crate::util::help::HelpSection::Notes(
            "Logs: ~/.sovereign/logs/daemon.log. The daemon was registered by `sovereign setup`.",
        ),
    ],
};

async fn run_daemon(_args: &[String]) -> i32 {
    // ── Load config ───────────────────────────────────────────────
    let config = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("hint: run `sovereign setup` first.");
            return 1;
        }
    };

    // ── Inference provider ────────────────────────────────────────
    // Build the embedded llama.cpp provider from the three GGUF slots.
    // Synchronous — load happens inline; model files are mmapped so
    // cold-start latency is dominated by disk I/O on first reference.
    let provider: Arc<dyn InferenceProvider> = match EmbeddedLlamaCpp::load_full_with_families(
        &config.models.fast,
        Some(&config.models.primary),
        Some(&config.models.embed),
        4096, // context_size — reasonable default; setup can make this configurable later
        None, // gpu_layers — auto-detect
        ModelFamily::Unknown,
        ModelFamily::Unknown,
        ModelFamily::Unknown,
    ) {
        Ok(p) => {
            let arc = Arc::new(p);
            // Unload the primary slot after 60s idle to reclaim VRAM.
            // Matches the pattern in sovereign-desktop.
            arc.start_idle_monitor(60);
            arc
        }
        Err(e) => {
            eprintln!("error: failed to load models: {e}");
            eprintln!("hint: verify paths in {}", SetupConfig::default_path().display());
            return 1;
        }
    };

    // ── Note store (for MCP notes tools + ring-buffer logging) ────
    let data_dir = config.data.dir.clone();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("error: cannot create data dir {}: {e}", data_dir.display());
        return 1;
    }
    let notes_path = data_dir.join("notes.db");
    let notes_store = match NoteStore::open(&notes_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: cannot open notes db {}: {e}", notes_path.display());
            return 1;
        }
    };

    // ── Tool registry (code intelligence + notes) ─────────────────
    // The embedded daemon serves /mcp for all locally-indexed corpora
    // under data_dir/indexes/. Tools return helpful errors when no
    // index is installed yet (first boot after setup, pre-project-init).
    let tools = build_tool_registry(&data_dir, Arc::clone(&notes_store)).await;

    // ── EmbeddedDaemon ────────────────────────────────────────────
    // Wrap in Arc so the mesh HTTP router can clone it for axum
    // handlers (see `install_mesh_http_router` — the router needs an
    // owned `Arc<EmbeddedDaemon>` to drive `create/join/rotate/leave`
    // from HTTP callers).
    let daemon = Arc::new(sovereign_mesh::EmbeddedDaemon::new(data_dir.clone()));
    daemon.set_inference_provider(Arc::clone(&provider)).await;
    let session_id = format!("daemon-{}", uuid::Uuid::new_v4());
    daemon
        .set_mcp(Arc::new(tools), Arc::clone(&notes_store), session_id)
        .await;

    // Mount the mesh HTTP API so the desktop (when running in Attach
    // mode) can drive `create/join/rotate/leave` against this daemon
    // without starting its own colliding EmbeddedDaemon.
    daemon
        .install_mesh_http_router(sovereign_mesh::mesh_http::mesh_router(Arc::clone(&daemon)))
        .await;

    // Admin HTTP surface — POST /v1/admin/reload. The factory below
    // tells the reload handler how to rebuild an InferenceProvider
    // when models.* changes on disk; without it, reload would error
    // out on any model-path change.
    daemon
        .install_admin_http_router(sovereign_mesh::admin_http::admin_router(Arc::clone(
            &daemon,
        )))
        .await;
    daemon.set_provider_factory(Arc::new(LlamaCppFactory)).await;
    daemon.set_setup_config(config.clone()).await;

    // ── Project freshness pipeline ────────────────────────────────
    //
    // The Reindexer owns per-project FS watchers, git-HEAD pollers,
    // and the coalescing rebuild queue. Each registered project
    // gets one `ProjectHandle`; the daemon shells out to this
    // subsystem from HTTP (`/v1/projects/*`) rather than invoking
    // exporters synchronously. Persisted projects (loaded from
    // `~/.sovereign/projects.json`) are re-registered at startup
    // so a daemon restart resumes watching everything without a
    // user action.
    let freshness_indexes_dir = data_dir.join("indexes");
    let merged_handle: sovereign_mesh::reindexer::ScipGraphHandle = {
        // Reuse the merged ScipGraph we already build for the MCP
        // tool registry so tool calls and the reindexer see the
        // same object. `build_tool_registry` below creates its
        // own copy; we wrap ours in an ArcSwap so the reindexer
        // can hot-swap after every rebuild.
        let initial = build_merged_scip_graph(&freshness_indexes_dir).await;
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(initial))
    };
    let reindexer = sovereign_mesh::reindexer::Reindexer::new(
        freshness_indexes_dir.clone(),
        Arc::clone(&merged_handle),
    );
    daemon
        .install_project_http_router(sovereign_mesh::project_http::project_router(Arc::clone(
            &reindexer,
        )))
        .await;

    // Resume any previously-registered projects so FS watchers
    // come back up without the user running `project register`
    // again. Missing / unreadable registry is non-fatal — the
    // daemon runs happily with zero registered projects.
    let registry = sovereign_mesh::projects::Registry::load().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not load project registry; starting empty");
        sovereign_mesh::projects::Registry::default()
    });
    for entry in registry.entries() {
        reindexer.register(entry.clone()).await;
        tracing::info!(corpus = %entry.corpus_id, "resumed registered project");
    }
    warn_orphaned_indexes(&freshness_indexes_dir, &registry);
    // Keep the reindexer alive for the lifetime of the daemon.
    // The variable binding is load-bearing — dropping the Arc
    // stops every supervised watcher.
    let _reindexer_handle = reindexer;

    // ── Resume or bootstrap a solo mesh ───────────────────────────
    match daemon.try_resume().await {
        Ok(true) => {
            tracing::info!("mesh resumed from persisted state");
        }
        Ok(false) => {
            // First boot after setup — create a silent solo mesh.
            let hostname = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "sovereign".to_string());
            let mesh_name = format!("{hostname}'s Mesh");
            match daemon.create_mesh(&mesh_name, &hostname).await {
                Ok(_result) => {
                    tracing::info!(%mesh_name, "solo mesh created");
                }
                Err(e) => {
                    eprintln!("error: could not create initial mesh: {e}");
                    return 1;
                }
            }
        }
        Err(e) => {
            eprintln!("error: mesh resume failed: {e}");
            return 1;
        }
    }

    tracing::info!(
        client_port = config.daemon.client_port,
        internal_port = config.daemon.internal_port,
        "sovereign daemon is running"
    );
    eprintln!(
        "sovereign daemon running — http://localhost:{}/v1 + /mcp",
        config.daemon.client_port
    );

    // ── Block until SIGINT/SIGTERM ────────────────────────────────
    wait_for_shutdown().await;

    // Graceful stop. Ignore errors — we're exiting anyway.
    let _ = daemon.stop().await;
    eprintln!("sovereign daemon stopped");
    0
}

/// Build the tool registry that serves `/mcp/*`. Mirrors the subset of
/// tools `sovereign project serve` registers. When no code indexes
/// are installed, tools return helpful "not indexed" messages rather
/// than erroring, so a freshly-setup daemon is still useful for
/// `write_note` / `read_notes`.
async fn build_tool_registry(
    data_dir: &std::path::Path,
    notes: Arc<NoteStore>,
) -> ToolRegistry {
    // Zero-vector embed for code indexes; real inference flows through
    // `/v1/*` via the InferenceProvider, not the tool layer.
    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let indexes_dir = data_dir.join("indexes");
    // CorpusEngine expects a recipes dir + data dir; we point recipes at
    // the same indexes dir (no recipe registry for the daemon).
    let engine = Arc::new(CorpusEngine::new(
        indexes_dir.clone(),
        indexes_dir.clone(),
        embed,
    ));

    let mut tools = ToolRegistry::new();

    // Code intelligence — scoped to discovered corpora under indexes_dir.
    tools.register(Box::new(sovereign_tools::SymbolLookupTool::new(Arc::clone(
        &engine,
    ))));
    tools.register(Box::new(sovereign_tools::CodeSearchTool::new(Arc::clone(
        &engine,
    ))));
    tools.register(Box::new(sovereign_tools::RecentChangesTool::new(Arc::clone(
        &engine,
    ))));

    // Call-graph tools. Merge every `scip_graph.db` under the indexes
    // directory into a single in-memory graph, then register
    // find_callers / find_callees / blast_radius. Without this step
    // agents can't trace references through the daemon — project_serve
    // had these, the daemon didn't.
    let merged_graph = build_merged_scip_graph(&indexes_dir).await;
    let graph_handle: sovereign_tools::ScipGraphHandle =
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(merged_graph));
    let health_checker = Arc::new(
        sovereign_tools::IndexHealthChecker::new(Arc::clone(&graph_handle)),
    );
    tools.register(Box::new(
        sovereign_tools::FindCallersTool::new(
            Arc::clone(&engine),
            Arc::clone(&graph_handle),
        )
        .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(
        sovereign_tools::FindCalleesTool::new(
            Arc::clone(&engine),
            Arc::clone(&graph_handle),
        )
        .with_health_checker(Arc::clone(&health_checker)),
    ));
    tools.register(Box::new(
        sovereign_tools::BlastRadiusTool::new(Arc::clone(&graph_handle))
            .with_health_checker(Arc::clone(&health_checker)),
    ));

    // Notes tools work regardless of indexing state.
    tools.register(Box::new(sovereign_tools::WriteNoteTool::new(Arc::clone(
        &notes,
    ))));
    tools.register(Box::new(sovereign_tools::ReadNotesTool::new(Arc::clone(
        &notes,
    ))));
    tools.register(Box::new(sovereign_tools::DeleteNoteTool::new(Arc::clone(
        &notes,
    ))));
    tools.register(Box::new(sovereign_tools::SessionReflectionTool::new(
        Arc::clone(&notes),
    )));

    // Project context — served from `indexes/project_docs.db` if a
    // project has been init'd. Absent on a bare-setup daemon; that's
    // fine, just one fewer tool.
    if let Ok(ds) = corpus_engine::ProjectDocsStore::open(
        &indexes_dir.join("project_docs.db"),
    ) {
        tools.register(Box::new(sovereign_tools::ProjectContextTool::new(
            Arc::new(ds),
        )));
    }

    // Doc-path checker — no state dependency.
    tools.register(Box::new(sovereign_tools::CheckDocPathsTool::new()));

    tools
}

/// Merge every per-corpus `scip_graph.db` under `indexes_dir` into a
/// single in-memory graph. Same idea as `project_cmd::load_merged_graph`
/// but without the operator-facing stdout printing, since the daemon
/// runs under launchd/systemd.
async fn build_merged_scip_graph(
    indexes_dir: &std::path::Path,
) -> corpus_engine::ScipGraph {
    let merged = corpus_engine::ScipGraph::open_in_memory("merged")
        .expect("in-memory ScipGraph");
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

/// `sovereign daemon restart` — hard-restart the registered service.
/// Most users reach for this when the daemon feels stuck or after a
/// change that isn't hot-reloadable (port, data_dir). For model-only
/// changes, prefer `sovereign daemon reload` — no gap in availability.
async fn restart_daemon() -> i32 {
    eprintln!("restarting sovereign daemon …");
    match crate::service_install::restart_service() {
        Ok(()) => {
            // Poll `/v1/models` so we don't hand control back to the
            // user while the daemon is still respawning. Ready when
            // we get any 2xx response — even an empty model list
            // means the router is up.
            if wait_for_ready(std::time::Duration::from_secs(10)).await {
                eprintln!("✓ daemon restarted and answering on :9741");
                0
            } else {
                eprintln!(
                    "⚠ restart command accepted but daemon didn't respond within 10s.\n\
                     check logs: ~/.sovereign/logs/daemon.log"
                );
                1
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// `sovereign daemon reload` — POST /v1/admin/reload. Hot-reloads
/// changed model paths in place without dropping connections.
/// Reports which fields hot-reloaded and which require a full
/// restart; a subsequent `sovereign daemon restart` picks those up.
async fn reload_daemon() -> i32 {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: build http client: {e}");
            return 1;
        }
    };
    let resp = client
        .post("http://127.0.0.1:9741/v1/admin/reload")
        .json(&serde_json::json!({}))
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "error: could not reach daemon at :9741 ({e}).\n\
                 hint: is it running? try `sovereign daemon status`."
            );
            return 1;
        }
    };
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"error": "non-JSON response"}));
    if !status.is_success() {
        eprintln!("error: admin/reload returned {status}: {body}");
        return 1;
    }
    let reloaded = body
        .get("reloaded_fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let restart_required = body
        .get("restart_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if reloaded.is_empty() && !restart_required {
        eprintln!("✓ no config changes detected — nothing to reload");
        return 0;
    }
    if !reloaded.is_empty() {
        let names: Vec<String> = reloaded
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        eprintln!("✓ hot-reloaded: {}", names.join(", "));
    }
    if restart_required {
        let pending = body
            .get("restart_required_fields")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        eprintln!(
            "⚠ these changes need a full restart: {pending}\n\
             run `sovereign daemon restart` to apply them."
        );
    }
    0
}

/// `sovereign daemon status` — is the daemon alive and answering?
async fn status_daemon() -> i32 {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: build http client: {e}");
            return 1;
        }
    };
    match client
        .get("http://127.0.0.1:9741/v1/models")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_else(|_| {
                serde_json::json!({"data": []})
            });
            let count = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            eprintln!("✓ daemon running at http://localhost:9741 ({count} models registered)");
            0
        }
        Ok(r) => {
            eprintln!(
                "⚠ something is answering on :9741 but returned {}. \
                 not a sovereign daemon, or in a bad state.",
                r.status()
            );
            1
        }
        Err(_) => {
            eprintln!(
                "✗ daemon not reachable on :9741.\n\
                 start it with `sovereign daemon restart` (if installed)\n\
                 or run `sovereign setup` (if not yet configured)."
            );
            1
        }
    }
}

/// Poll `/v1/models` until it returns 2xx or `timeout` elapses.
/// Returns true when the daemon is answering, false on timeout.
async fn wait_for_ready(timeout: std::time::Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(r) = client.get("http://127.0.0.1:9741/v1/models").send().await {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    false
}

/// Rebuilds the embedded llama.cpp provider from a fresh `SetupConfig`.
/// Hot-swapped into `EmbeddedDaemon::inference_provider` by the admin
/// reload handler when the user changes a `models.*` path in
/// `~/.config/sovereign/config.toml` (e.g. via the desktop Settings
/// panel's model picker). Keeps the model-loading side of the daemon
/// out of `sovereign-mesh`, which has no business knowing about GGUF.
struct LlamaCppFactory;

#[async_trait]
impl ProviderFactory for LlamaCppFactory {
    async fn build_provider(
        &self,
        cfg: &SetupConfig,
    ) -> Result<Arc<dyn InferenceProvider>, String> {
        // Mirror the load parameters used by `run_daemon` on cold
        // start — the reload must not silently downgrade context
        // size or auto-gpu-layer behaviour.
        let provider = EmbeddedLlamaCpp::load_full_with_families(
            &cfg.models.fast,
            Some(&cfg.models.primary),
            Some(&cfg.models.embed),
            4096,
            None,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
        )
        .map_err(|e| format!("reload: failed to load models: {e}"))?;
        let arc = Arc::new(provider);
        arc.start_idle_monitor(60);
        Ok(arc)
    }
}

/// Wait for SIGINT (Ctrl-C) or SIGTERM (systemd/launchd shutdown).
/// Returns when either arrives so the caller can run teardown.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        let mut sigterm = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
            Ok(s) => s,
            Err(_) => {
                // If we can't listen for SIGTERM, fall back to just SIGINT.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Surface orphaned per-corpus SCIP indexes at startup.
///
/// On an upgrade from a pre-registry sovereign, `~/.sovereign/
/// indexes/<corpus>/scip_graph.db` will often exist even though
/// `projects.json` is empty. The daemon can't safely auto-register
/// those — we don't know which filesystem path each one came
/// from, and guessing could point the FS watcher at the wrong
/// directory. Instead, log a one-shot hint so the operator knows
/// to re-register each repo manually.
fn warn_orphaned_indexes(
    indexes_dir: &std::path::Path,
    registry: &sovereign_mesh::projects::Registry,
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
        let Some(name) = entry
            .file_name()
            .to_str()
            .map(|s| s.to_string())
        else {
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
        "  Run `sovereign project register` in each repo to resume watching.\n\
         (The daemon won't guess the filesystem path for you — bad guesses\n\
         point the FS watcher at the wrong directory.)"
    );
    eprintln!();
}

