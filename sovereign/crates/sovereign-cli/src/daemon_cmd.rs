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

use std::io::IsTerminal as _;
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine::{CorpusEngine, EmbedFn, NoteStore};
use sovereign_core::model_family::{
    EmbedModelInfo, ModelFamily, NormalizationStrategy, PoolingStrategy,
};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::ToolRegistry;
use sovereign_inference::embedded::EmbeddedLlamaCpp;
use sovereign_mesh::admin_http::ProviderFactory;

/// Entry point routed from `main.rs` when the user invokes
/// `sovereign daemon` or one of its subcommands.
///
/// Phase 4 dispatch order:
/// - `sovereign daemon`             → bare invocation falls through to `run`,
///                                    which inlines the setup wizard on
///                                    first boot if no config exists.
/// - `sovereign daemon run [flags]` → unchanged; the OS-service entry point.
/// - `sovereign daemon --flag ...`  → bare flags (e.g. `--setup-only`)
///                                    route to `run` so users can type
///                                    `sovereign daemon --setup-only` without
///                                    the explicit `run` token.
/// - `sovereign daemon <known>`     → start/stop/restart/reload/status as
///                                    before.
pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    match args.first().map(String::as_str) {
        Some("run") => run_daemon(&args[1..]).await,
        Some("start") => start_daemon().await,
        Some("stop") => stop_daemon().await,
        Some("restart") => restart_daemon().await,
        Some("reload") => reload_daemon().await,
        Some("status") => status_daemon().await,
        Some(flag) if flag.starts_with("--") => {
            // Bare flags like `sovereign daemon --setup-only` route
            // straight to run_daemon — the user means "start the
            // daemon (or its first-boot wizard) with these flags."
            run_daemon(args).await
        }
        Some(other) => {
            eprintln!("error: unknown daemon subcommand '{other}'");
            crate::util::help::print(&HELP);
            1
        }
        None => {
            // Bare `sovereign daemon` — Phase 4 routes this to
            // run_daemon so first-time users get a working daemon
            // without hunting for the magic `run` keyword. launchd
            // and systemd unit files keep using `daemon run`
            // explicitly; both paths land in the same place.
            run_daemon(&[]).await
        }
    }
}

/// Public entry for `sovereign setup` (Phase 4 shim). Runs only the
/// wizard portion (hardware detect → model pick → config write); does
/// NOT register a service or load models. The setup_cmd module's
/// `run_setup` calls into this so both `sovereign setup` and
/// `sovereign daemon --setup-only` share one code path.
pub async fn run_setup_only(args: &[String]) -> i32 {
    let mut forwarded = vec!["--wizard-only".to_string()];
    forwarded.extend(args.iter().cloned());
    crate::setup_cmd::run_setup(&forwarded).await
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign daemon",
    summary: "Long-running OICP server with managed inference + MCP tools.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign daemon [--setup-only] | sovereign daemon <subcommand>",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--setup-only", "Run the first-boot wizard (hardware detect + model pick + config) and exit without binding the listener."),
        ]),
        crate::util::help::HelpSection::Subcommands(&[
            ("(bare)",  "Run the daemon in the foreground. On first boot inlines the setup wizard; subsequent runs just load config and start. Equivalent to `daemon run`."),
            ("run",     "Same as bare — kept for explicit invocation by launchd / systemd unit files."),
            ("start",   "Start the daemon in the background (detached child + PID file at ~/.sovereign/daemon.pid). Waits for readiness."),
            ("status",  "Report whether the daemon is running and answering on :9741."),
            ("stop",    "Stop the daemon cleanly (SIGTERM). Uses the PID file from `start` when present; otherwise falls back to launchctl / systemctl."),
            ("reload",  "Apply config changes without a restart (POST /v1/admin/reload)."),
            ("restart", "Hard-restart via launchctl / systemctl. Drops in-flight requests."),
        ]),
        crate::util::help::HelpSection::Notes(
            "Logs: ~/.sovereign/logs/daemon.log. To register as a launchd/systemd service, run `sovereign install-service`.",
        ),
    ],
};

async fn run_daemon(args: &[String]) -> i32 {
    // ── Phase 4 flag parsing ──────────────────────────────────────
    //
    // `--setup-only` runs the wizard and exits without binding the
    // listener. Useful for users who want to configure the host now
    // and start the daemon manually later. Other flags pass through
    // to the daemon-start path; unrecognised flags are tolerated for
    // forward-compatibility (the daemon doesn't accept tunables on
    // the command line, only via the config file).
    let setup_only = args.iter().any(|a| a == "--setup-only");

    // ── Phase 4 first-boot wizard ─────────────────────────────────
    //
    // Pre-Phase-4 the daemon refused to start with a "run sovereign
    // setup first" hint. Now we inline the wizard so a user typing
    // `sovereign daemon` on a fresh box gets a working setup. The
    // wizard prompts for model selection, so it requires a TTY: a
    // launchd-spawned daemon with no config will fall through to
    // the same hint as before, since `is_terminal()` returns false
    // in that environment.
    if !sovereign_core::setup_config::SetupConfig::exists() {
        if !std::io::stdin().is_terminal() {
            eprintln!("error: no config at {}", SetupConfig::default_path().display());
            eprintln!(
                "hint: launchd/systemd can't run the interactive wizard. \
                 Run `sovereign daemon --setup-only` from a terminal first."
            );
            return 1;
        }
        // Forward `--setup-only` and unknown flags to the wizard so
        // users can pass `--yes` / `--data-dir` directly: `sovereign
        // daemon --setup-only --yes`.
        let wizard_args: Vec<String> = args
            .iter()
            .filter(|a| a.as_str() != "--setup-only")
            .cloned()
            .collect();
        let code = run_setup_only(&wizard_args).await;
        if code != 0 {
            return code;
        }
        // After a successful wizard the config file exists; load below.
    }

    if setup_only {
        // Wizard already ran above (or config existed and the wizard
        // was a no-op). Either way, return without booting the daemon.
        return 0;
    }

    // ── Log rotation ──────────────────────────────────────────────
    //
    // launchd holds the FDs on `daemon.log` / `daemon.err` (set via
    // the plist's StandardOutPath / StandardErrorPath) and never
    // re-opens them, so rename-style rotation would leak the inode.
    // Instead we copy-truncate at startup (cheap if under cap, safe
    // for in-flight launchd writes — the FD continues into the
    // now-empty file) and again on a 30-minute timer for long-running
    // daemon processes. See `util::log_rotation` for the contract.
    //
    // Ordered FIRST so a daemon that's been running for days and
    // produced a 5-GB log doesn't make the operator's `tail -f` drop
    // dead before the new daemon prints its first useful line.
    let log_dir = home_dir_buf().join(".sovereign").join("logs");
    crate::util::log_rotation::rotate_daemon_logs(
        &log_dir,
        crate::util::log_rotation::DEFAULT_SIZE_CAP_BYTES,
        crate::util::log_rotation::DEFAULT_KEEP_N_BAKS,
    );
    // 30-minute periodic rotation so a daemon that runs continuously
    // for days stays bounded between launchd restarts. The interval is
    // a knob — shorter cadence catches bursts faster but adds I/O
    // wakeups; 30 min is comfortably long for a stat() + size check.
    let _rotation_handle = crate::util::log_rotation::spawn_rotation_loop(
        log_dir.clone(),
        crate::util::log_rotation::DEFAULT_SIZE_CAP_BYTES,
        crate::util::log_rotation::DEFAULT_KEEP_N_BAKS,
        std::time::Duration::from_secs(30 * 60),
    );

    // ── Load config ───────────────────────────────────────────────
    let config = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!(
                "hint: run `sovereign daemon --setup-only` to (re-)create the config."
            );
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
        // PR-E2: optional Code specialist. When set, `code`-hinted
        // requests hot-swap into the lazy slot (shared with primary).
        // None = pre-E2 two-slot behaviour — all substantive work on
        // the Main responder.
        config.models.code.as_deref(),
        // context_size — sourced from `[models].context_size` so a
        // batch host (Strix Halo, 128 GB unified) can opt into 32k
        // without touching code, while a 64 GB Mac stays at the safe
        // 16384 default. History: 4096 was too tight (opencode's
        // max_tokens=4096 + AGENTS.md prompt blew it out); 32768 was
        // briefly the default but on a 35B Q4 with weights + fast +
        // embed + OS + ingest, the 16 GB KV cache made
        // `llama_new_context_with_model` return NULL on first primary
        // use. 16384 halves KV to ~8 GB and keeps headroom on a Mac.
        // The atlas Phase 1 pipeline benefits from 32k on Strix Halo.
        config.models.effective_context_size(),
        None, // gpu_layers — auto-detect
        ModelFamily::Unknown,
        ModelFamily::Unknown,
        ModelFamily::Unknown,
        ModelFamily::Unknown, // code slot — family detection deferred (see PR-E2)
    ) {
        Ok(p) => {
            let arc = Arc::new(p);
            // Wire the optional LRU memory budget BEFORE installing
            // extras. With a budget set, each `load_extra` call
            // (including the eager startup loads from `[models.extra]`)
            // checks against it and evicts cold slots if needed.
            // Without a budget, eviction is disabled and slots persist
            // until manually unloaded — matches historical behaviour.
            if let Err(e) =
                arc.set_extras_memory_budget(config.models.max_extras_memory_bytes())
            {
                eprintln!("error: failed to set extras memory budget: {e}");
                return 1;
            }
            // Idle-unload monitor for extras slots. Independent of
            // the primary's idle monitor — extras have eager-load /
            // hold-resident semantics by default; this background
            // task lets the operator opt into "drop after N seconds
            // idle" reclamation. Default is 0 = disabled.
            arc.start_extras_idle_monitor(config.daemon.extras_idle_secs);
            // Operator-declared additional chat slots. Each entry in
            // `[models.extra]` is loaded eagerly here; failures on
            // individual slots are warned but don't fail the daemon.
            // Routing kicks in when `/v1/chat/completions` arrives
            // with a `model` field matching the gguf stem of one of
            // these slots — `select_slot_for_request` picks
            // `SlotTarget::Extra(name)` and sidesteps Speed-based
            // routing entirely.
            //
            // `install_extras` takes `&self` (interior-mutable via
            // RwLock) so we install AFTER wrapping in `Arc` — the
            // runtime `/internal/models/load` endpoint uses the same
            // entry point on the same Arc.
            if !config.models.extra.is_empty() {
                if let Err(e) = arc.install_extras(
                    config.models.extra.clone(),
                    config.models.effective_context_size(),
                ) {
                    eprintln!("error: failed to install extras slots: {e}");
                    return 1;
                }
            }
            // Sourced from `[daemon].primary_idle_secs`. Default 60s
            // suits a desktop touching the model occasionally; batch
            // workloads (atlas enrich) want 1800+ to skip the 3–4 s
            // reload tax between back-to-back short LLM calls.
            arc.start_idle_monitor(config.daemon.primary_idle_secs);
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

    // ── CorpusEngine ──────────────────────────────────────────────
    // Single shared instance: powers both the `/mcp` tool registry
    // (find_callers, code_search, etc.) AND — now that we wired
    // the engine into the mesh daemon's AppState — the
    // `corpus_collaborate` handler that runs the daemon's share of
    // a partitioned Wikipedia/etc. ingest via
    // `engine.ingest_with_overrides`.
    //
    // The embed function MUST be real. Earlier this session the
    // daemon shipped a zero-vector stub here (correct for SCIP
    // code graphs, which don't embed), and when collaborative
    // ingestion started calling `ingest_with_overrides` it wrote
    // ~4 million 768-dim all-zeros vectors into the partition's
    // `chunks.lance` at "60,000 chunks/sec" — nonsense embeddings
    // at the speed of the Lance writer, not the embed model. Any
    // merge of that partition into the canonical index would have
    // poisoned retrieval with zero vectors.
    //
    // Route EmbedFn through the already-loaded `provider`. Same
    // llama.cpp embed slot the desktop's ingest uses, same 1024
    // dims, same pooling. Also wire the batch variant: Wikipedia
    // throughput is ~5× higher with batched embed calls on
    // M-series Metal compared to per-chunk.
    // Resolve the persistent node_id up front so the engine's
    // `partition_path(corpus_id)` returns the same
    // `<corpus>-partition-node-<hex>` the daemon itself will expect.
    // Without this the engine defaults to `self_node_id = "local"` and
    // every partition-of-self lookup misses — `in_progress_ingestions`
    // returns 0 for a partition dir that's sitting right there on
    // disk with `ingestion_in_progress=true`.
    //
    // Resolution order mirrors what `EmbeddedDaemon::start_daemon`
    // does on resume vs. create:
    //   1. `<data_dir>/node_id` file, if present.
    //   2. `self_node_id` baked into `mesh.json` (the common case —
    //      existing meshes carry the id inside the mesh snapshot even
    //      when the standalone node_id file was never materialised).
    //   3. Generate a fresh id and persist it (fresh install).
    // `load_or_generate_self_node_id` covers (1) and (3) but would
    // ignore (2), which is exactly the bug we're fixing: the user's
    // daemon resumes with mesh.json's id while the engine had been
    // minting a mismatched fresh one.
    let self_node_id = match sovereign_mesh::persist::load_node_id(&data_dir) {
        Ok(Some(id)) => id,
        _ => match sovereign_mesh::persist::load(&data_dir) {
            Ok(Some(persisted)) => persisted.self_node_id,
            _ => sovereign_mesh::persist::load_or_generate_self_node_id(&data_dir),
        },
    };

    let engine: Arc<CorpusEngine> = {
        let indexes_dir = data_dir.join("indexes");
        let provider_for_embed = Arc::clone(&provider);
        let embed: EmbedFn = Arc::new(move |text: &str| {
            let p = Arc::clone(&provider_for_embed);
            let text = text.to_string();
            Box::pin(async move {
                p.embed(&text)
                    .await
                    .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
            })
        });
        let provider_for_batch = Arc::clone(&provider);
        let batch_embed: corpus_engine::types::BatchEmbedFn = Arc::new(move |texts: &[String]| {
            let p = Arc::clone(&provider_for_batch);
            let texts = texts.to_vec();
            Box::pin(async move {
                p.embed_batch(&texts)
                    .await
                    .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
            })
        });
        // Derive the embed model identifier from the configured GGUF
        // path so `_corpus_meta.json` records the actual model rather
        // than failing the ingest pre-flight ("embedding model name not
        // configured"). Matches the wiring in `state.rs:717-723` and
        // every other call site (`main.rs:506`, `chat_cmd/bootstrap.rs`,
        // `code_cmd.rs`, `project_cmd.rs`); the standalone daemon was
        // the lone holdout, which is why the desktop's
        // `/internal/corpus/install` POST hits this engine and bombs at
        // the pre-flight before the first byte is downloaded.
        let embed_model_name = config
            .models
            .embed
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown-embed-model")
            .to_string();
        Arc::new(
            CorpusEngine::new(indexes_dir.clone(), indexes_dir, embed)
                .with_embedding_model(&embed_model_name)
                .with_batch_embed_fn(batch_embed)
                .with_self_node_id(self_node_id.to_string()),
        )
    };

    // ── Tool registry (code intelligence + notes) ─────────────────
    // The embedded daemon serves /mcp for all locally-indexed corpora
    // under data_dir/indexes/. Tools return helpful errors when no
    // index is installed yet (first boot after setup, pre-project-init).
    let tools = build_tool_registry(
        &data_dir,
        Arc::clone(&engine),
        Arc::clone(&notes_store),
    )
    .await;

    // ── EmbeddedDaemon ────────────────────────────────────────────
    // Wrap in Arc so the mesh HTTP router can clone it for axum
    // handlers (see `install_mesh_http_router` — the router needs an
    // owned `Arc<EmbeddedDaemon>` to drive `create/join/rotate/leave`
    // from HTTP callers).
    let daemon = Arc::new(sovereign_mesh::EmbeddedDaemon::new(data_dir.clone()));

    // Wrap the raw `EmbeddedLlamaCpp` in `MeshInferenceProvider`
    // before installing it as the daemon's serving provider.
    //
    // Without this wrapper the daemon's HTTP `/v1/chat/completions`
    // path silently substitutes a local model whenever the request
    // names a model that's only advertised by a peer (e.g. asking
    // for `gemma-4-E4B-it-Q4_K_M` on a node that only loads
    // `Qwen3.5-9B` and `35B-Q6` would answer with 35B-Q6 and stamp
    // the response accordingly). The wrapper inspects
    // `request.model_id` and either:
    //   * serves locally when self_manifest advertises the id
    //     (the local provider's slot picker handles Fast/Primary/
    //     Code/extras matching by name), or
    //   * forwards the request over HTTP to the peer whose manifest
    //     advertises the id, or
    //   * returns `ModelNotLoaded` if no node serves it — instead
    //     of the previous silent substitution.
    //
    // Mirrors the desktop wiring in
    // `sovereign-desktop/src-tauri/src/state.rs:649` so a request
    // hitting either entrypoint follows the same routing rules.
    let routed_provider: Arc<dyn InferenceProvider> = Arc::new(
        sovereign_mesh::peer_inference::MeshInferenceProvider::new(
            Arc::clone(&provider),
            Arc::clone(&daemon),
        ),
    );
    daemon
        .set_inference_provider(Arc::clone(&routed_provider))
        .await;
    // Hand the engine to the mesh daemon so the auto_ingest loop and
    // /internal/corpus/* HTTP surface can both see in-progress
    // wikipedia/etc. ingests. See engine block above for the
    // diagnostic story.
    daemon.set_corpus_engine(Arc::clone(&engine)).await;

    // Lazy-stamp canonical fingerprints for any installed
    // canonicals that don't yet carry one (legacy ingests pre-
    // dating the canonical-sync surface). One BLAKE3 over the
    // content_hash list per corpus; idempotent. Fired in the
    // background so daemon startup doesn't block on it. See
    // `corpus_engine::CorpusEngine::lazy_stamp_legacy_fingerprints`
    // for the contract.
    {
        let engine_for_stamp = Arc::clone(&engine);
        tokio::spawn(async move {
            engine_for_stamp.lazy_stamp_legacy_fingerprints().await;
        });
    }

    // Tier-2 enrichment resume: find any `<...>-tier2` workspace
    // under `<data_dir>/enrichment/` whose checkpoint is incomplete
    // and re-spawn `enrich extract --resume` for each. Picks up
    // unfinished work after a daemon restart / host reboot. Safe
    // to fire on every boot — already-complete workspaces no-op,
    // and `--resume` skips chapters already in the checkpoint.
    {
        let enrich_dir = data_dir.join("enrichment");
        let idx_dir = data_dir.join("indexes");
        tokio::spawn(async move {
            let cli_binary = std::env::current_exe()
                .unwrap_or_else(|_| std::path::PathBuf::from("sovereign"));
            tracing::info!(
                enrichment_dir = %enrich_dir.display(),
                "tier-2 resume: scanning for unfinished workspaces"
            );
            let outcomes = sovereign_tools::atlas_postinstall::resume_inflight_tier2(
                enrich_dir, idx_dir, cli_binary,
            )
            .await;
            for o in outcomes {
                use sovereign_tools::atlas_postinstall::Tier2LaunchOutcome;
                match o {
                    Tier2LaunchOutcome::Spawned {
                        workspace_id,
                        log_path,
                        pid,
                    } => tracing::info!(
                        workspace = %workspace_id,
                        log = %log_path.display(),
                        pid,
                        "tier-2 resume: re-spawned"
                    ),
                    Tier2LaunchOutcome::AlreadyComplete { .. } => {}
                    // Resume scan never passes peer advice — this
                    // arm is unreachable in practice but the
                    // exhaustiveness check requires us to cover it.
                    Tier2LaunchOutcome::DeferredToPeer { .. } => {}
                    Tier2LaunchOutcome::InitFailed { reason }
                    | Tier2LaunchOutcome::SpawnFailed { reason } => tracing::warn!(
                        reason,
                        "tier-2 resume: re-spawn failed"
                    ),
                }
            }
        });
    }

    // Publish this node's embed model fingerprint so peers can filter
    // us in/out of collaborative ingestion.
    //
    // Without this wiring, `corpus_collaborate` returns 503
    // "embed model not configured on this node — cannot plan
    // collaboration" even though the embed slot is loaded and
    // working. The desktop does the same publication in
    // `sovereign-desktop/src-tauri/src/state.rs:885`; the CLI daemon
    // just didn't mirror it.
    //
    // Probe the provider for the real output dimensions rather than
    // trusting a hardcoded value — gets us the same ground truth the
    // corpus-engine uses for its dimension-mismatch guard.
    match provider.embed("probe").await {
        Ok(probe_vec) => {
            // `model_id` = bare filename stem (e.g.
            // `qwen-embedding-0.6b`). Peers compare EmbedModelInfo
            // for exact equality, so the string has to match what
            // the desktop/other CLI daemons advertise for the same
            // GGUF. File-stem is the stable shared handle.
            let model_id = config
                .models
                .embed
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("embed")
                .to_string();
            // Resolve the embed family from the bundled manifest so
            // pooling + normalisation match whatever the desktop
            // path would advertise for the same GGUF. Without this,
            // CLI daemons serving Qwen3-Embedding would have
            // silently mismatched peers running the desktop build
            // (Qwen3-Embedding is Last + Server, not Mean +
            // Application) — collaborative ingestion would never
            // plan across them.
            //
            // BYOM paths that don't match any manifest row fall
            // through to `ModelFamily::Unknown` → Mean + Application
            // (safe default for generic mean-pool BERT embedders).
            let embed_filename = config
                .models
                .embed
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let embed_family = sovereign_core::models_manifest::DEFAULT_MANIFEST
                .embed_family_for_file(embed_filename)
                .unwrap_or(ModelFamily::Unknown);
            let embed_quirks = embed_family.default_quirks().embed;
            let pooling = embed_quirks
                .as_ref()
                .map(|q| q.pooling)
                .unwrap_or(PoolingStrategy::Mean);
            let normalization = embed_quirks
                .as_ref()
                .map(|q| q.normalize)
                .unwrap_or(NormalizationStrategy::Application);
            let embed_info = EmbedModelInfo {
                model_id: model_id.clone(),
                dimensions: probe_vec.len(),
                pooling,
                normalization,
            };
            tracing::info!(
                model_id = %embed_info.model_id,
                dims = embed_info.dimensions,
                family = ?embed_family,
                pooling = ?pooling,
                normalization = ?normalization,
                "embed model info: advertising to mesh peers"
            );
            daemon.set_embed_model_info(embed_info).await;
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "embed probe failed — peers will NOT route collaborative ingestion to this node"
            );
        }
    }
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
    daemon
        .set_provider_factory(Arc::new(LlamaCppFactory {
            daemon: Arc::clone(&daemon),
        }))
        .await;
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
    let mut reindexer = sovereign_mesh::reindexer::Reindexer::new(
        freshness_indexes_dir.clone(),
        Arc::clone(&merged_handle),
    );
    // Phase 7.1: configure the commit-message harvester so the
    // reindexer's git-HEAD poll harvests non-noisy commits into
    // `source='committed'` notes. Must run BEFORE any clone /
    // share — Arc::get_mut returns None once this is shared.
    sovereign_mesh::reindexer::Reindexer::with_commit_harvester(
        &mut reindexer,
        Arc::clone(&notes_store),
    );
    daemon
        .install_project_http_router(sovereign_mesh::project_http::project_router(Arc::clone(
            &reindexer,
        )))
        .await;

    // Knowledge-view HTTP surface — POST /v1/knowledge/landscape_digest.
    //
    // Built read-only at this stage: the daemon holds a
    // KnowledgeViewManager so an attached desktop can fetch
    // assembled digest blocks via HTTP, but the enrichment loop
    // (observer → debouncer → atlas writes) is NOT wired here.
    // That requires the daemon to own a SQLite state store with an
    // installed observer, which is the next architectural pass.
    // Today's behaviour: the daemon serves whatever digest can be
    // built from existing on-disk skeletons. If no enrichment has
    // been run, the digest is empty — the desktop's
    // `MeshLandscapeDigestClient` treats that identically to
    // KnowledgeView=off (empty splice, no prompt impact).
    //
    // `local_only_skill_ids` is empty here; the desktop's HTTP
    // client resolves `active_is_local_only` against ITS own skill
    // registry and passes the bool in the request. See
    // `MeshLandscapeDigestClient::new` and
    // `LandscapeDigestRequest.active_is_local_only`.
    let knowledge_view_db_path = data_dir.join("sovereign.db");
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&provider));
    let knowledge_view_manager = Arc::new(
        sovereign_tools::knowledge_view::KnowledgeViewManager::new(
            Arc::clone(&engine),
            inference_fn,
            knowledge_view_db_path,
            Vec::new(),
        )
        .await,
    );
    daemon
        .install_knowledge_view_http_router(
            sovereign_mesh::landscape_digest_http::landscape_digest_router(Arc::clone(
                &knowledge_view_manager,
            )),
        )
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

    // ── Watched-folder reconciliation scheduler ─────────────────
    //
    // Constructs the LocalCorpusManager + per-corpus registry,
    // re-populates the registry from the persisted corpora list
    // (auto-resume on daemon restart), then spawns the dispatcher
    // loop. The scheduler walks each registered watched-folder
    // corpus on its configured cadence (default 120 s, floored at
    // 60 s) and applies the diff through CorpusUpdater.
    //
    // The local-corpus subsystem requires a StateStore but only
    // touches it on `remove` (delete_corpus_state). The persistent
    // source of truth for corpus metadata is `{data_dir}/local-corpora/*.json`,
    // which the manager loads at construction. An in-memory store
    // is therefore sufficient for the daemon — `remove`'s
    // delete_corpus_state becomes a benign no-op against the empty
    // in-memory map.
    // Watched-folder reconciliation subsystem. The full wiring (build
    // registry → resume corpora → install runtime singleton → mount
    // HTTP routes → spawn scheduler) is factored into
    // `sovereign_mesh::watched_folder_setup` so the desktop's
    // embedded daemon can call the same path.
    let _watched_subsystem = {
        let lc_store: Arc<dyn sovereign_core::traits::StateStore> =
            Arc::new(sovereign_store::memory::InMemoryStateStore::new());
        match sovereign_tools::local_corpus::LocalCorpusManager::init(
            Arc::clone(&engine),
            lc_store,
            None,
            data_dir.clone(),
            data_dir.join("vault-snapshots"),
        )
        .await
        {
            Ok(manager) => Some(
                sovereign_mesh::watched_folder_setup::WatchedSubsystem::install(
                    Arc::clone(&daemon),
                    Arc::clone(&engine),
                    Arc::new(manager),
                    config.watched_folders.max_concurrent_sweeps,
                )
                .await,
            ),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "watched_folder:manager_init_failed — scheduler not spawned"
                );
                None
            }
        }
    };

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

    // Graceful shutdown — preserves mesh.json so the next launch
    // resumes into the same mesh. Critically NOT `leave()`, which
    // would clear persistence and force a fresh solo mesh on next
    // boot (the regression that left Machine A and Machine B in
    // different meshes after every Ctrl-C).
    let _ = daemon.shutdown().await;
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
    engine: Arc<CorpusEngine>,
    notes: Arc<NoteStore>,
) -> ToolRegistry {
    let indexes_dir = data_dir.join("indexes");

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

    // DESIGN.md structural signals — no state dependency; the tool
    // reads the DESIGN.md path argument at call time. No
    // `with_project_root` in the daemon context because the daemon
    // doesn't know which project the caller means.
    tools.register(Box::new(
        sovereign_tools::DesignSignalsExtractTool::new(),
    ));

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
async fn stop_daemon() -> i32 {
    eprintln!("stopping sovereign daemon …");

    // Prefer the PID file written by `daemon start` — that's the only
    // way we can reliably stop a daemon that wasn't launched via a
    // service manager. Fall back to launchctl / systemctl when the
    // PID file is absent (service-managed install) or stale.
    if let Some(pid) = read_daemon_pid() {
        #[cfg(unix)]
        {
            // SAFETY: POSIX kill is async-signal-safe; we're only sending
            // SIGTERM to a pid we own (we wrote the pidfile ourselves).
            let rc = unsafe { libc_kill(pid, 15 /* SIGTERM */) };
            if rc == 0 {
                // Wait up to 10s for graceful exit, polling liveness.
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(10);
                while std::time::Instant::now() < deadline {
                    if unsafe { libc_kill(pid, 0) } != 0 {
                        let _ = std::fs::remove_file(daemon_pid_path());
                        eprintln!("✓ stopped (pid {pid})");
                        return 0;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                eprintln!("⚠ pid {pid} didn't exit after 10s; leaving it alone");
                return 1;
            }
            // kill() failed — pid likely stale. Clean up and fall
            // through to service_install so a service-managed instance
            // (if any) still gets stopped.
            let _ = std::fs::remove_file(daemon_pid_path());
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
    }

    match crate::service_install::stop_service() {
        Ok(()) => {
            eprintln!("✓ stop signal sent — daemon will exit after draining in-flight requests");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// `sovereign daemon start` — spawn `daemon run` as a detached child
/// so it keeps running after this CLI exits. Writes the child pid to
/// `~/.sovereign/daemon.pid` and tails logs to `~/.sovereign/logs/`.
///
/// Idempotent: if `:9741` already answers, prints the running pid (if
/// we wrote it) and returns 0.
///
/// This is the dev-workflow counterpart to `sovereign setup`'s
/// launchd/systemd registration — when you don't want a service
/// manager owning lifecycle, `start` gives you a one-liner.
async fn start_daemon() -> i32 {
    if wait_for_ready(std::time::Duration::from_millis(200)).await {
        let pid_hint = read_daemon_pid()
            .map(|p| format!(" (pid {p})"))
            .unwrap_or_default();
        eprintln!("✓ daemon already running on :9741{pid_hint}");
        return 0;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot resolve current_exe: {e}");
            return 1;
        }
    };

    let log_dir = home_dir_buf().join(".sovereign").join("logs");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!("error: cannot create {}: {e}", log_dir.display());
        return 1;
    }
    let out_path = log_dir.join("daemon.out");
    let err_path = log_dir.join("daemon.err");

    // Append so repeated start/stop cycles keep history rather than
    // truncating on each launch — matches launchd's default rotation
    // semantics (it also appends until an external log-rotator moves
    // the file aside, which is the `.bak` pattern seen in the logs
    // dir already).
    let out_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: open {}: {e}", out_path.display());
            return 1;
        }
    };
    let err_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&err_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: open {}: {e}", err_path.display());
            return 1;
        }
    };

    // Clean up a stale PID file before writing a new one so `stop`
    // can't target a recycled pid if the prior daemon crashed.
    if let Some(pid) = read_daemon_pid() {
        #[cfg(unix)]
        if unsafe { libc_kill(pid, 0) } != 0 {
            let _ = std::fs::remove_file(daemon_pid_path());
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
    }

    eprintln!("starting sovereign daemon (logs: {})…", log_dir.display());

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .arg("run")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(err_file));

    // Detach from the shell's process group so Ctrl-C in the invoking
    // terminal doesn't take the daemon down with it. Combined with
    // the /dev/null stdin + redirected stdio above, this is enough
    // for the common dev case (launch-from-shell, close shell). For
    // truly hostile environments (ssh disconnect on a flaky link),
    // use the launchd/systemd path via `sovereign setup` instead.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: spawn daemon: {e}");
            return 1;
        }
    };
    let pid = child.id() as i32;

    // Drop the Child handle so we don't wait on it — the process is
    // now fully detached and owned by init (via process_group(0)).
    drop(child);

    let pid_path = daemon_pid_path();
    if let Err(e) = std::fs::write(&pid_path, format!("{pid}\n")) {
        eprintln!(
            "⚠ started pid {pid} but could not write {}: {e}",
            pid_path.display()
        );
        // Don't abort — the daemon is still coming up; caller can
        // stop via `pkill -f 'sovereign daemon run'` if needed.
    }

    if wait_for_ready(std::time::Duration::from_secs(20)).await {
        eprintln!("✓ daemon ready at http://127.0.0.1:9741 (pid {pid})");
        return 0;
    }
    eprintln!(
        "⚠ pid {pid} started but :9741 didn't respond within 20s\n\
         tail {} for details",
        err_path.display()
    );
    1
}

/// Path to the pidfile written by `daemon start`.
fn daemon_pid_path() -> std::path::PathBuf {
    home_dir_buf().join(".sovereign").join("daemon.pid")
}

/// Read the pidfile and return its pid if the process is still alive.
/// Returns None for a missing, empty, unparseable, or stale pidfile.
fn read_daemon_pid() -> Option<i32> {
    let raw = std::fs::read_to_string(daemon_pid_path()).ok()?;
    let pid: i32 = raw.trim().parse().ok()?;
    #[cfg(unix)]
    {
        // `kill(pid, 0)` returns 0 iff the process exists and we have
        // permission to signal it; any error (ESRCH, EPERM) means we
        // shouldn't trust the pidfile.
        if unsafe { libc_kill(pid, 0) } != 0 {
            return None;
        }
    }
    Some(pid)
}

// Minimal FFI shim for `kill(2)` so we can probe / signal the daemon
// pid without pulling in the `libc` crate as a dep. Signature matches
// POSIX: `int kill(pid_t pid, int sig)` where pid_t is i32 on every
// platform Sovereign targets.
#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn home_dir_buf() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// `sovereign daemon restart` — stop the running daemon (whichever
/// lifecycle owns it: pidfile or launchd) and start a fresh one.
///
/// Earlier versions of this command went straight to `launchctl
/// kickstart -k gui/<uid>/com.sovereign.daemon`. That broke for every
/// user who started the daemon via `sovereign daemon start` (the
/// pidfile-managed path), because the launchd service isn't loaded
/// in the gui domain — kickstart errors out with "Could not find
/// service in domain for user". The asymmetry was: `start` and
/// `stop` both fall back through pidfile → launchctl, but `restart`
/// only ever spoke to launchctl.
///
/// Fix: compose `restart` from `stop_daemon().await + start_daemon().await`,
/// so the same lifecycle inference (pidfile preferred, launchctl as
/// fallback) applies to all three commands. Cost: launchd-managed
/// installs that were previously kickstarted now end up under
/// `daemon start`'s detached-child path — consistent with what
/// `daemon stop && daemon start` already does, and which any user
/// who actually wants strict launchd accounting can run via
/// `launchctl kickstart -k gui/$(id -u)/com.sovereign.daemon`
/// directly.
async fn restart_daemon() -> i32 {
    eprintln!("restarting sovereign daemon …");
    let stop_rc = stop_daemon().await;
    if stop_rc != 0 {
        // stop_daemon already printed the failure reason. Don't try
        // to start on top of a daemon we couldn't confirm is gone —
        // we'd just race the old one for :9741.
        return stop_rc;
    }
    start_daemon().await
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

/// Rebuilds the embedded llama.cpp provider from a fresh `SetupConfig`,
/// wrapped in the same `MeshInferenceProvider` used at cold start so
/// hot-reloads preserve mesh-aware model routing.
///
/// Hot-swapped into `EmbeddedDaemon::inference_provider` by the admin
/// reload handler when the user changes a `models.*` path in
/// `~/.config/sovereign/config.toml` (e.g. via the desktop Settings
/// panel's model picker). Keeps the model-loading side of the daemon
/// out of `sovereign-mesh`, which has no business knowing about GGUF.
struct LlamaCppFactory {
    /// Same `EmbeddedDaemon` the cold-start path wraps the raw
    /// llama.cpp provider against. Held here so a hot-reload
    /// (operator changing the primary GGUF path while the daemon is
    /// running) produces a `MeshInferenceProvider` view of the new
    /// raw provider — without this, reload would drop the wrapper
    /// and `/v1/chat/completions` would silently start substituting
    /// for peer-only model names again.
    daemon: Arc<sovereign_mesh::EmbeddedDaemon>,
}

#[async_trait]
impl ProviderFactory for LlamaCppFactory {
    async fn build_provider(
        &self,
        cfg: &SetupConfig,
    ) -> Result<Arc<dyn InferenceProvider>, String> {
        // Mirror the load parameters used by `run_daemon` on cold
        // start — the reload must not silently downgrade context
        // size or auto-gpu-layer behaviour. Pulls context size and
        // idle timeout from `cfg` for the same reason: a hot-reload
        // shouldn't drop the operator's tuned values.
        let provider = EmbeddedLlamaCpp::load_full_with_families(
            &cfg.models.fast,
            Some(&cfg.models.primary),
            Some(&cfg.models.embed),
            cfg.models.code.as_deref(),
            cfg.models.effective_context_size(),
            None,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown,
            ModelFamily::Unknown, // code slot — family detection deferred (see PR-E2)
        )
        .map_err(|e| format!("reload: failed to load models: {e}"))?;
        // Keep a typed `Arc<EmbeddedLlamaCpp>` to fire
        // `start_idle_monitor` (inherent method), then upcast to
        // `Arc<dyn InferenceProvider>` so the wrapper can hold it.
        let raw_concrete = Arc::new(provider);
        raw_concrete.start_idle_monitor(cfg.daemon.primary_idle_secs);
        let raw: Arc<dyn InferenceProvider> = raw_concrete;

        // Wrap so a hot-reloaded daemon keeps its mesh-aware model
        // routing — same wrapper the cold-start path installs in
        // `run_daemon`. See the comment on the cold-start wiring
        // for why a bare `EmbeddedLlamaCpp` here would re-introduce
        // the silent-substitution bug.
        let routed: Arc<dyn InferenceProvider> = Arc::new(
            sovereign_mesh::peer_inference::MeshInferenceProvider::new(
                raw,
                Arc::clone(&self.daemon),
            ),
        );
        Ok(routed)
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

