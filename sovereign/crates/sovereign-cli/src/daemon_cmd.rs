//! `sovereign daemon run` — the hidden subcommand that launchd/systemd
//! calls to actually run the embedded Commonwealth daemon in the
//! foreground. Humans don't invoke this directly; they go through
//! `sovereign setup` (which registers the service) and then let the
//! service manager keep it alive.
//!
//! Responsibilities:
//! 1. Read `~/.sovereign/config.toml` for model paths + ports.
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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use corpus_engine::{
    CorpusEngine, EmbedFn, LintResultStore, NoteStore, TestResultStore,
};
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
            ("stop",    "Stop the daemon cleanly (SIGTERM). Tries the PID file first, then looks up the listener on :9741 via lsof/ss, then falls back to launchctl / systemctl."),
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

    // ── VRAM capacity preflight ───────────────────────────────────
    //
    // Estimate the working set (weights + KV cache + grammar
    // scratch) for every slot the daemon would eagerly load, sum
    // against detected VRAM with a safety margin, and refuse to
    // start if the config would overcommit. Catches the
    // 2026-05-11 L40S thrash class: 2 × Q4_K_L "fit" at 38 GB
    // idle but evicted each other under live KV pressure, taking
    // throughput to 1 slug/hr.
    //
    // Bypass via `SOVEREIGN_SKIP_VRAM_CHECK=1` when the operator
    // knows better than the planner (e.g. a slot mix where one
    // model is lazy-loaded behind a high idle_secs gate). The
    // report still prints in that case so the diagnosis stays
    // visible in logs.
    {
        let hardware = sovereign_inference::hardware::HardwareProfile::detect();
        let slots = sovereign_inference::capacity::build_slots_from_config(&config);
        let report = sovereign_inference::capacity::check_fit(&slots, &hardware);
        if !report.fits {
            if sovereign_inference::capacity::check_skipped_by_env() {
                tracing::warn!(
                    required_mb = report.total_required_mb,
                    available_mb = report.available_mb,
                    "VRAM check would have refused this config — bypassed by SOVEREIGN_SKIP_VRAM_CHECK. \
                     Thrash risk accepted by operator."
                );
            } else {
                eprintln!("{}", report.refuse_message());
                eprintln!(
                    "hint: edit {} and re-run, or set SOVEREIGN_SKIP_VRAM_CHECK=1 \
                     to bypass at your own risk.",
                    SetupConfig::default_path().display(),
                );
                return 1;
            }
        } else {
            tracing::info!(
                required_mb = report.total_required_mb,
                available_mb = report.available_mb,
                slots = report.per_slot.len(),
                "VRAM preflight: config fits"
            );
        }
    }

    // ── Force-tool-calls config → process env ─────────────────────
    //
    // The inference adapter reads `SOVEREIGN_FORCE_TOOL_CALLS` per
    // request to decide whether to upgrade `tool_choice="auto"` to
    // `"required"` (which engages the JSON-Schema tool-envelope
    // grammar). When the operator sets `[daemon] force_tool_calls =
    // true` in setup_config.toml, we propagate that into the process
    // env at boot so the existing per-request lookup picks it up.
    // Caller-supplied env wins — `std::env::set_var` only overrides
    // when nothing was set on the CLI invocation. Operators who want
    // a one-shot test (`SOVEREIGN_FORCE_TOOL_CALLS=0 sovereign daemon
    // run`) can still do so without editing the config file.
    if config.daemon.force_tool_calls
        && std::env::var("SOVEREIGN_FORCE_TOOL_CALLS").is_err()
    {
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        tracing::info!(
            "daemon: force_tool_calls=true — grammar engaged on every \
             tools-using request (set via setup_config.toml)"
        );
    }

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
        // code slot is Qwen3-Coder-30B-A3B-Instruct (the only code
        // GGUF we ship today). Pinning the family to Qwen3 picks up
        // Qwen's recommended sampling defaults — top_k=20 (vs the
        // Unknown fallback of 40), top_p=0.95, presence_penalty=1.5
        // — and the SystemPromptToken thinking control. Empirically
        // (2026-05-08 measurement) the Unknown defaults left the
        // sampler too permissive on long Rust emissions, contributing
        // to the character-drop pattern (`f3 2`, `Lat encyClass`).
        ModelFamily::Qwen3,
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
            // Optional cross-encoder reranker. Bootstraps from
            // `SOVEREIGN_RERANK_MODEL_PATH` — points at a GGUF
            // reranker (e.g. jina-reranker-v3, bge-reranker-v2-m3).
            // When unset, the daemon comes up without a reranker and
            // `Runtime.rerank_fn` stays `None` so retrieval baseline
            // behaviour is preserved.
            if let Ok(rerank_path) = std::env::var("SOVEREIGN_RERANK_MODEL_PATH") {
                let path = PathBuf::from(&rerank_path);
                match arc.install_rerank_slot(path, ModelFamily::Reranker) {
                    Ok(model_id) => {
                        tracing::info!(
                            slot = "rerank",
                            model_id = %model_id,
                            "rerank slot installed from SOVEREIGN_RERANK_MODEL_PATH"
                        );
                    }
                    Err(e) => {
                        // Soft-fail: a missing or broken reranker
                        // file should not block daemon startup;
                        // retrieval simply runs the baseline path.
                        tracing::warn!(
                            path = %rerank_path,
                            error = %e,
                            "rerank slot install failed — running without reranker"
                        );
                    }
                }
            }
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

    // ── Lint / test result stores ─────────────────────────────────
    // Always opened so the agent-facing `lint_status` / `test_status`
    // tools have a backing store to read from. When no watcher is
    // configured (no workspace resolved, or sovereign.toml has no
    // [lint_runner]/[test_runner]), the tools report `never_run` —
    // accurate and unambiguous.
    let lint_store: Arc<LintResultStore> = match LintResultStore::open(
        &data_dir.join("lint_results.db"),
    ) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "error: cannot open lint results db {}: {e}",
                data_dir.join("lint_results.db").display()
            );
            return 1;
        }
    };
    let test_store: Arc<TestResultStore> = match TestResultStore::open(
        &data_dir.join("test_results.db"),
    ) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "error: cannot open test results db {}: {e}",
                data_dir.join("test_results.db").display()
            );
            return 1;
        }
    };

    // Wipe orphan rows left by a previous daemon process that was
    // SIGKILLed mid-run. Without this, `lint_status` / `test_status`
    // can return `running` indefinitely against a row whose owning
    // process is long dead. Best-effort — cleanup failure shouldn't
    // block daemon startup.
    if let Ok(n) = lint_store.clear_orphan_runs().await {
        if n > 0 {
            tracing::info!(
                purged = n,
                "lint_results: cleared orphan rows from prior daemon process"
            );
        }
    }
    if let Ok(n) = test_store.clear_orphan_runs().await {
        if n > 0 {
            tracing::info!(
                purged = n,
                "test_results: cleared orphan rows from prior daemon process"
            );
        }
    }

    // ── Workspace-driven watchers (optional) ──────────────────────
    // The daemon has no inherent project. When the user wants the
    // background lint/test watcher running, they point us at a
    // workspace via either:
    //   1. SOVEREIGN_WORKSPACE_DIR env var (preferred for launchd —
    //      set in the plist's EnvironmentVariables block), or
    //   2. ~/.sovereign/workspace — a single-line text file with
    //      the workspace path (handy for users who can't easily
    //      edit launchd plists).
    //
    // Inside that workspace, `.sovereign/sovereign.toml` declares
    // `[lint_runner]` / `[test_runner]`. The default sovereign.toml
    // committed at the workspace root points at
    // `scripts/sovereign-lint.sh` which fan-runs `cargo check` over
    // sovereign + commonwealth + corpus-engine in parallel. So one
    // env var lights up coverage for all three.
    let workspace_dir = resolve_workspace_dir();
    let watcher_active_flag = Arc::new(AtomicBool::new(false));
    let mut lint_watcher: Option<Arc<corpus_engine::LintWatcher>> = None;
    let mut test_watcher: Option<Arc<corpus_engine::TestWatcher>> = None;
    let mut watched_lint_scope: Option<String> = None;
    let mut watched_test_scope: Option<String> = None;
    // Held for the lifetime of `start_daemon` — its `Drop` aborts the
    // watcher's spawned tasks. Underscored because we never read it
    // back; the value is the side effect of holding the handle alive.
    let mut _coordinator_handle: Option<corpus_engine::CoordinatorHandle> = None;
    if let Some(ref ws) = workspace_dir {
        let sov_cfg = corpus_engine::SovereignConfig::load_or_default(
            &ws.join(".sovereign"),
        );
        // Single-permit semaphore shared by the lint + test watchers so
        // their cargo subprocesses serialize instead of compounding
        // memory pressure. Without this, both fire concurrent cargo
        // check / cargo test invocations on every debounced edit
        // flush, doubling RSS and inviting macOS to SIGTERM the daemon
        // under pressure.
        let run_slot = Arc::new(tokio::sync::Semaphore::new(1));

        if let Some(ref cfg) = sov_cfg.lint_runner {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { ws.join(p) }
            });
            watched_lint_scope = Some(cfg.command.clone());
            lint_watcher = Some(Arc::new(
                corpus_engine::LintWatcher::new(
                    &cfg.command,
                    working_dir,
                    cfg.timeout_secs.unwrap_or(120),
                    Arc::clone(&lint_store),
                )
                .with_run_slot(Arc::clone(&run_slot)),
            ));
            tracing::info!(
                command = %cfg.command,
                workspace = %ws.display(),
                "lint watcher configured (shared run slot)"
            );
        }
        if let Some(ref cfg) = sov_cfg.test_runner {
            let working_dir = cfg.working_dir.as_ref().map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { ws.join(p) }
            });
            watched_test_scope = Some(cfg.command.clone());
            test_watcher = Some(Arc::new(
                corpus_engine::TestWatcher::new(
                    &cfg.command,
                    working_dir,
                    cfg.timeout_secs.unwrap_or(300),
                    Arc::clone(&test_store),
                )
                .with_run_slot(Arc::clone(&run_slot)),
            ));
            tracing::info!(
                command = %cfg.command,
                workspace = %ws.display(),
                "test watcher configured (shared run slot)"
            );
        }

        if lint_watcher.is_some() || test_watcher.is_some() {
            let debounce_ms = sov_cfg
                .lint_runner
                .as_ref()
                .and_then(|c| c.debounce_ms)
                .or_else(|| sov_cfg.test_runner.as_ref().and_then(|c| c.debounce_ms))
                .unwrap_or(800);
            let mut coordinator = corpus_engine::WatcherCoordinator::new(debounce_ms);
            if let Some(ref w) = lint_watcher {
                coordinator.register(
                    Arc::clone(w) as Arc<dyn corpus_engine::BackgroundWatcher>,
                );
            }
            if let Some(ref w) = test_watcher {
                coordinator.register(
                    Arc::clone(w) as Arc<dyn corpus_engine::BackgroundWatcher>,
                );
            }
            match coordinator.start(vec![ws.clone()]).await {
                Ok(handle) => {
                    watcher_active_flag.store(true, Ordering::Release);
                    _coordinator_handle = Some(handle);
                    eprintln!(
                        "sovereign daemon: lint/test watcher live on {}",
                        ws.display()
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        workspace = %ws.display(),
                        "watcher coordinator failed to start"
                    );
                }
            }
        }
    } else {
        tracing::debug!(
            "no workspace resolved (set SOVEREIGN_WORKSPACE_DIR or write \
             ~/.sovereign/workspace) — lint/test watcher disabled"
        );
    }

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
        // recipes_dir doubles as the registry's overrides_dir. Locally-
        // published recipes from `sovereign recipe publish` land at
        // `~/.sovereign/recipes/<id>/recipe.toml` and only resolve when
        // the engine's overrides_dir points there. Earlier this passed
        // `indexes_dir` for the recipes argument, which made every
        // `corpus install` skip the local override and try the public
        // registry URL — the wikipedia-catalog dev variant could never
        // be installed because its data URL is not yet hosted.
        let recipes_dir = data_dir.join("recipes");
        Arc::new(
            CorpusEngine::new(recipes_dir, indexes_dir, embed)
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
        Arc::clone(&lint_store),
        Arc::clone(&test_store),
        test_watcher.clone(),
        watched_lint_scope.clone(),
        watched_test_scope.clone(),
        Arc::clone(&watcher_active_flag),
        workspace_dir.clone(),
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
    // Keep a typed handle to the mesh provider so we can push the
    // slot-alias map into it once `register_local_model_slots` has
    // populated `AppState.slot_aliases`. The trait-object form is
    // what the daemon needs; the typed form is what the alias
    // installer needs.
    let mesh_provider = Arc::new(
        sovereign_mesh::peer_inference::MeshInferenceProvider::new(
            Arc::clone(&provider),
            Arc::clone(&daemon),
        ),
    );
    let routed_provider: Arc<dyn InferenceProvider> = mesh_provider.clone();
    daemon
        .set_inference_provider(Arc::clone(&routed_provider))
        .await;
    // Push slot aliases from AppState into the mesh provider once
    // the daemon's setup phase has registered model slots. Without
    // this, the mesh layer can't resolve `commonwealth/primary` →
    // local GGUF in its Local-serving branch, and the deferred
    // resolution path (routes_inference passes the alias through
    // for mesh routing) never lands on a real slot. Done on a
    // spawned task because `daemon.app_state()` only returns
    // `Some` after `start()` transitions DaemonState to Running.
    {
        let daemon_for_alias_push = Arc::clone(&daemon);
        let mesh_for_alias_push = mesh_provider.clone();
        tokio::spawn(async move {
            // Poll briefly for the AppState to be available. The
            // setup transition usually completes within a few
            // hundred ms; cap at 30s so a stuck setup never hangs
            // this spawn.
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(state) = daemon_for_alias_push.app_state().await {
                    let snapshot = state.inner.slot_aliases.load();
                    let map: std::collections::HashMap<String, String> = snapshot
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    if !map.is_empty() {
                        tracing::info!(
                            count = map.len(),
                            "daemon_cmd: pushing slot aliases into mesh provider"
                        );
                        mesh_for_alias_push.set_slot_aliases(map);
                        break;
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        "daemon_cmd: slot-alias push timed out after 30s — \
                         mesh layer will serve aliases as plain model ids"
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        });
    }
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
    // Reading-surface HTTP routes — `/internal/corpus/{c}/chunks/...`.
    // Backs the desktop's glass-box reading UI when running against a
    // standalone daemon (CLI-mode) instead of the in-process Tauri
    // daemon. Loopback-only.
    daemon
        .install_reading_http_router(
            sovereign_mesh::reading_http::reading_router(Arc::clone(&daemon)),
        )
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
            Ok(manager) => {
                // Folder-ingest v1 §3.3 — install enrichment
                // defaults so the watched-folder driver can
                // synthesise an EnrichConfig for "Enable
                // enrichment" requests. Pull model ids from the
                // daemon's resolved chat / embed slots; on a
                // fresh setup with no models picked, fall back
                // to empty strings so the driver returns a clear
                // "defaults not installed" error to the UI.
                fn id_from_path(p: &std::path::Path) -> String {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string()
                }
                let chat_model = id_from_path(&config.models.primary);
                let embed_model = id_from_path(&config.models.embed);
                if !chat_model.is_empty() && !embed_model.is_empty() {
                    let base_url = format!(
                        "http://127.0.0.1:{}",
                        config.daemon.client_port
                    );
                    manager
                        .set_enrichment_defaults(
                            sovereign_tools::local_corpus::watched::enrich::EnrichmentDefaults {
                                chat_model,
                                embed_model,
                                base_url,
                                cli_path: None,
                            },
                        )
                        .await;
                } else {
                    tracing::info!(
                        "watched_folder:enrichment_defaults_skipped — \
                         chat_model or embed_model not configured; \
                         per-folder enrichment will return an error \
                         until models are picked"
                    );
                }
                Some(
                    sovereign_mesh::watched_folder_setup::WatchedSubsystem::install(
                        Arc::clone(&daemon),
                        Arc::clone(&engine),
                        Arc::new(manager),
                        config.watched_folders.max_concurrent_sweeps,
                    )
                    .await,
                )
            }
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

    // ── Pidfile ───────────────────────────────────────────────────
    //
    // `sovereign daemon stop` keys off `~/.sovereign/daemon.pid` to
    // know which process to SIGTERM. Previously only `daemon start`
    // (the detached-child launcher) wrote that file, so any other
    // launch path — `sovereign daemon run` from a shell, `cargo run
    // -- daemon run`, systemd's `ExecStart` — left no pidfile and
    // `stop` silently fell back to `systemctl/launchctl stop`, which
    // is a no-op for daemons launched outside the service manager.
    //
    // Writing the pidfile here from `run_daemon` itself makes the
    // file an accurate property of "a daemon is running" rather than
    // "the daemon was launched via `start`". The bind has already
    // succeeded above, so any pre-existing pidfile is stale and can
    // be overwritten safely (the live owner of :9741 is us).
    let pid_path = daemon_pid_path();
    if let Some(parent) = pid_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let self_pid = std::process::id();
    if let Err(e) = std::fs::write(&pid_path, format!("{self_pid}\n")) {
        tracing::warn!(
            path = %pid_path.display(),
            error = %e,
            "could not write daemon pidfile — `daemon stop` will need lsof/launchctl fallback"
        );
    }

    // ── Block until SIGINT/SIGTERM ────────────────────────────────
    wait_for_shutdown().await;

    // Graceful shutdown — preserves mesh.json so the next launch
    // resumes into the same mesh. Critically NOT `leave()`, which
    // would clear persistence and force a fresh solo mesh on next
    // boot (the regression that left Machine A and Machine B in
    // different meshes after every Ctrl-C).
    let _ = daemon.shutdown().await;

    // Remove the pidfile only if it still points at us. If something
    // racier (a fresh `daemon start` parent re-wrote it during our
    // shutdown, or a new daemon took our port after we released it)
    // claimed the file, leave it alone — `read_daemon_pid` already
    // handles a stale pidfile via `kill(pid, 0)`.
    if let Ok(raw) = std::fs::read_to_string(&pid_path) {
        if raw.trim().parse::<u32>().ok() == Some(self_pid) {
            let _ = std::fs::remove_file(&pid_path);
        }
    }

    eprintln!("sovereign daemon stopped");
    0
}

/// Build the tool registry that serves `/mcp/*`. Mirrors the subset of
/// tools `sovereign project serve` registers. When no code indexes
/// are installed, tools return helpful "not indexed" messages rather
/// than erroring, so a freshly-setup daemon is still useful for
/// `write_note` / `read_notes`.
#[allow(clippy::too_many_arguments)]
async fn build_tool_registry(
    data_dir: &std::path::Path,
    engine: Arc<CorpusEngine>,
    notes: Arc<NoteStore>,
    lint_store: Arc<LintResultStore>,
    test_store: Arc<TestResultStore>,
    test_watcher: Option<Arc<corpus_engine::TestWatcher>>,
    watched_lint_scope: Option<String>,
    watched_test_scope: Option<String>,
    watcher_active_flag: Arc<AtomicBool>,
    workspace_dir: Option<PathBuf>,
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

    // ── Lint / test watcher tools ───────────────────────────────
    // Always registered so MCP clients see a stable tool list. When
    // no watcher is wired (workspace not resolved or sovereign.toml
    // empty), the tools report `never_run` / `watcher_active: false`
    // — accurate, not silently-missing.
    {
        let mut tool = sovereign_tools::LintStatusTool::new(Arc::clone(&lint_store))
            .with_watcher_active(Arc::clone(&watcher_active_flag));
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
    {
        let mut tool = sovereign_tools::BuildTool::new(Arc::clone(&lint_store))
            .with_watcher_active(Arc::clone(&watcher_active_flag));
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
            .with_watcher_active(Arc::clone(&watcher_active_flag));
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
        tools.register(Box::new(sovereign_tools::RunTestsTool::new(
            Arc::clone(w),
        )));
    }

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

    // ATOS step verification — runs verify commands with
    // hollow/untouched gates to catch silent agent no-ops.
    tools.register(Box::new(sovereign_tools::AtosVerifyTool::new()));

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

    // Wikipedia on-demand fetch — operates against the catalog corpus
    // installed on this daemon. Wired here so `sovereign tools call
    // wikipedia_fetch --title=…` and the MCP /mcp surface can drive
    // catalog-hit → fetch end-to-end without a live chat session.
    tools.register(Box::new(sovereign_tools::WikipediaFetchTool::new(
        Arc::clone(&engine),
    )));

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

    // No usable pidfile. Before forwarding to systemctl/launchctl,
    // try a port-based lookup: if something is listening on :9741, that
    // process IS the daemon and we can SIGTERM it directly. This catches:
    //   - daemons launched by an older binary that didn't write a pidfile
    //   - daemons started via `cargo run -- daemon run` from a dev shell
    //   - daemons whose pidfile was hand-deleted
    // Without this, `daemon stop` falls through to `systemctl stop` on
    // an inactive unit, which is a no-op returning exit 0 — i.e. silently
    // reports success while the actual daemon keeps serving.
    #[cfg(unix)]
    if let Some(pid) = find_daemon_pid_by_port(9741) {
        let rc = unsafe { libc_kill(pid, 15 /* SIGTERM */) };
        if rc == 0 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if unsafe { libc_kill(pid, 0) } != 0 {
                    let _ = std::fs::remove_file(daemon_pid_path());
                    eprintln!("✓ stopped (pid {pid}, found by :9741 listener)");
                    return 0;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            eprintln!(
                "⚠ pid {pid} (owner of :9741) didn't exit after 10s; leaving it alone"
            );
            return 1;
        }
        // kill() failed (most likely EPERM on a daemon owned by another
        // user). Fall through to the service-manager path; it might be
        // the only thing with permission.
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

/// Find the PID listening on `port` on localhost. Used by `daemon stop`
/// as a last-resort fallback when no pidfile is present and the service
/// manager has nothing to stop — see `stop_daemon` for the call site.
///
/// We try two probes in order, preferring `lsof` because its output is
/// stable and trivially parseable, then `ss` (iproute2) for hosts where
/// lsof isn't installed (notably minimal Linux containers). Both are
/// invoked with arguments that print one PID per line and nothing else.
#[cfg(unix)]
fn find_daemon_pid_by_port(port: u16) -> Option<i32> {
    use std::process::Command;

    // `lsof -t` → "terse" output: bare PIDs, one per line.
    // `-sTCP:LISTEN` filters out connected sockets (so we don't pick up
    // a client of :9741 if one happens to be alive).
    // `-i 4TCP:<port>` is more selective than `-i :<port>` — IPv4 only,
    // TCP only, avoiding UDP false positives.
    if let Ok(out) = Command::new("lsof")
        .args([
            "-t",
            "-sTCP:LISTEN",
            "-i",
            &format!("4TCP:{port}"),
        ])
        .output()
    {
        if out.status.success() {
            if let Some(pid) = parse_first_pid_line(&out.stdout) {
                return Some(pid);
            }
        }
    }

    // `ss -H -tlnp 'sport = :<port>'` prints one line per listener
    // with no header. The pid lives inside `users:(("name",pid=N,fd=...))`
    // so we have to extract it. -H suppresses the header line.
    if let Ok(out) = Command::new("ss")
        .args([
            "-H",
            "-tlnp",
            &format!("sport = :{port}"),
        ])
        .output()
    {
        if out.status.success() {
            if let Some(pid) = parse_ss_first_pid(&String::from_utf8_lossy(&out.stdout)) {
                return Some(pid);
            }
        }
    }

    None
}

#[cfg(unix)]
fn parse_first_pid_line(bytes: &[u8]) -> Option<i32> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.lines()
        .next()
        .and_then(|l| l.trim().parse::<i32>().ok())
}

/// Extract the first `pid=N` integer from one or more `ss -tlnp` lines.
/// Returns None when no line contains a `pid=` token or the digits don't
/// parse as i32. The pid token shape is iproute2-stable: the relevant
/// fragment is `users:(("name",pid=12345,fd=7))`.
#[cfg(unix)]
fn parse_ss_first_pid(text: &str) -> Option<i32> {
    for line in text.lines() {
        if let Some(rest) = line.split("pid=").nth(1) {
            let digits: String =
                rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(p) = digits.parse::<i32>() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(all(test, unix))]
mod stop_daemon_tests {
    use super::*;

    #[test]
    fn parses_lsof_terse_single_pid() {
        assert_eq!(parse_first_pid_line(b"664307\n"), Some(664307));
    }

    #[test]
    fn parses_lsof_terse_first_of_multiple_pids() {
        // lsof -t prints one pid per line if multiple processes match.
        // Picking the first is fine — the bind would have prevented a
        // second daemon from coexisting; multiple lines means there is
        // a non-daemon listener masquerading on the port, and we'd
        // rather SIGTERM the obvious one than spray signals at every
        // pid in the list.
        assert_eq!(parse_first_pid_line(b"664307\n123\n"), Some(664307));
    }

    #[test]
    fn returns_none_on_empty_lsof_output() {
        assert_eq!(parse_first_pid_line(b""), None);
        assert_eq!(parse_first_pid_line(b"\n"), None);
    }

    #[test]
    fn parses_ss_listener_line_with_pid() {
        // Real shape of `ss -H -tlnp 'sport = :9741'` output:
        let sample = "LISTEN 0      4096       127.0.0.1:9741       0.0.0.0:* \
                      users:((\"sovereign-cli\",pid=664307,fd=12))";
        assert_eq!(parse_ss_first_pid(sample), Some(664307));
    }

    #[test]
    fn ignores_ss_lines_without_pid_field() {
        let sample = "LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\n\
                      LISTEN 0 4096 127.0.0.1:9741 0.0.0.0:* \
                        users:((\"sovereign-cli\",pid=664307,fd=12))";
        assert_eq!(parse_ss_first_pid(sample), Some(664307));
    }

    #[test]
    fn ss_no_pid_token_returns_none() {
        assert_eq!(parse_ss_first_pid(""), None);
        assert_eq!(parse_ss_first_pid("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*"), None);
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

    // Diagnostics + defensive stack budget for the daemon process.
    //
    // - `RUST_BACKTRACE=full` makes the next stack overflow log a
    //   real frame trace to daemon.err — without it we see only
    //   "thread 'tokio-rt-worker' has overflowed its stack" with no
    //   symbol info, which is what made the 2026-05-11 fragility
    //   investigation slow.
    // - `RUST_MIN_STACK=8388608` (8 MiB) bumps the default thread
    //   stack size for every std::thread spawn the daemon makes.
    //   tokio's multi-thread runtime workers inherit this when they
    //   don't explicitly set `thread_stack_size`. Default is 2 MiB
    //   on macOS, which we've reproducibly overflowed under
    //   drift-detect load (77 overflows / 166 daemon starts this
    //   session). 8 MiB is the same headroom Cargo's build worker
    //   threads use and matches what corpus-engine's tree-sitter
    //   path needs on deeply-nested wikitext templates.
    //
    // Both vars are only set when the parent didn't already set
    // them — so a developer profiling with custom RUST_BACKTRACE
    // (e.g. =0, =1) or shrinking the stack to reproduce isn't
    // overridden.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        cmd.env("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        cmd.env("RUST_MIN_STACK", "8388608");
    }

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

/// Resolve the workspace directory the daemon should watch for
/// lint/test changes. Returns `None` when the user has not opted in,
/// in which case `lint_status` / `test_status` report
/// `watcher_active: false` and `never_run` — the honest signal.
///
/// Lookup order:
/// 1. `SOVEREIGN_WORKSPACE_DIR` environment variable. Preferred for
///    launchd/systemd: set it in the service's environment block so
///    every daemon launch picks it up automatically.
/// 2. `~/.sovereign/workspace` — single-line text file containing
///    the workspace path. Useful for users who can't easily edit
///    their service environment.
///
/// Both forms are validated to point at an existing directory; a
/// missing or non-directory path is treated as "no workspace
/// configured" (with a warning log so the misconfiguration is
/// visible in the daemon log without breaking startup).
fn resolve_workspace_dir() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("SOVEREIGN_WORKSPACE_DIR") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if path.is_dir() {
                return Some(path);
            } else {
                tracing::warn!(
                    path = %path.display(),
                    "SOVEREIGN_WORKSPACE_DIR set but not a directory — ignoring"
                );
            }
        }
    }
    let workspace_file = home_dir_buf().join(".sovereign").join("workspace");
    if let Ok(contents) = std::fs::read_to_string(&workspace_file) {
        let trimmed = contents.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if path.is_dir() {
                return Some(path);
            } else {
                tracing::warn!(
                    path = %path.display(),
                    file = %workspace_file.display(),
                    "~/.sovereign/workspace path is not a directory — ignoring"
                );
            }
        }
    }
    None
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
/// `~/.sovereign/config.toml` (e.g. via the desktop Settings
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
            // code slot is Qwen3-Coder-30B-A3B-Instruct (the only code
        // GGUF we ship today). Pinning the family to Qwen3 picks up
        // Qwen's recommended sampling defaults — top_k=20 (vs the
        // Unknown fallback of 40), top_p=0.95, presence_penalty=1.5
        // — and the SystemPromptToken thinking control. Empirically
        // (2026-05-08 measurement) the Unknown defaults left the
        // sampler too permissive on long Rust emissions, contributing
        // to the character-drop pattern (`f3 2`, `Lat encyClass`).
        ModelFamily::Qwen3,
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
        let mesh_provider = Arc::new(
            sovereign_mesh::peer_inference::MeshInferenceProvider::new(
                raw,
                Arc::clone(&self.daemon),
            ),
        );
        // Push current slot aliases into the freshly-built mesh
        // provider so a reload preserves the deferred-resolution
        // wiring. Mirrors the cold-start spawned task in
        // `run_daemon`; here we run inline because the daemon is
        // already in the Running state at reload time.
        if let Some(state) = self.daemon.app_state().await {
            let snapshot = state.inner.slot_aliases.load();
            let map: std::collections::HashMap<String, String> = snapshot
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !map.is_empty() {
                mesh_provider.set_slot_aliases(map);
            }
        }
        let routed: Arc<dyn InferenceProvider> = mesh_provider;
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

