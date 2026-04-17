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
        crate::util::help::HelpSection::Usage("sovereign daemon run"),
        crate::util::help::HelpSection::Subcommands(&[
            ("run", "Run in the foreground; exits on SIGINT/SIGTERM"),
        ]),
        crate::util::help::HelpSection::Notes(
            "You don't normally invoke this directly. `sovereign setup` registers the\n\
             service; the OS starts it via `daemon run`. Logs: ~/.sovereign/logs/daemon.log.",
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

    tools
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

