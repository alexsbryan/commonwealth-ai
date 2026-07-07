// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon run` — the hidden subcommand that launchd/systemd
//! calls to actually run the embedded Commonwealth daemon in the
//! foreground. Humans don't invoke this directly; they go through
//! `svrn setup` (which registers the service) and then let the
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
//!    comes up. `svrn mesh rotate` (future) can later print a
//!    shareable join key.
//! 6. Block on `tokio::signal::ctrl_c()` so the service manager
//!    controls lifecycle.

use std::io::IsTerminal as _;
use std::sync::Arc;

use corpus_engine::{CorpusEngine, LintResultStore, TestResultStore};
use corpus_engine_notes::NoteStore;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

// §3.2 split: the run_daemon bootstrap stays here (it's the orchestrator);
// the separable lifecycle / workspace / provider / worker / tool-registry
// concerns moved to submodules. `home_dir_buf` + `warn_orphaned_indexes`
// stay here (the former is shared with submodules as an ancestor-private).
mod bootstrap;
mod build;
mod lifecycle;
mod solve_http;
mod solve_tools;
// Liveness probe for the pidfile-managed (manual) daemon — consumed by
// `install-service`'s double-start guard and doctor's supervision check.
pub(crate) use lifecycle::read_daemon_pid;
mod provider;
mod tool_registry;
mod worker;
mod workflow_trigger;
mod workspace;

use lifecycle::{
    reload_daemon, restart_daemon, start_daemon, status_daemon, stop_daemon, wait_for_shutdown,
};
use tool_registry::build_tool_registry;
use worker::run_worker_daemon;
use workspace::resolve_workspace_dir;

/// Entry point routed from `main.rs` when the user invokes
/// `svrn daemon` or one of its subcommands.
///
/// Phase 4 dispatch order:
/// - `svrn daemon`             → bare invocation falls through to `run`,
///                                    which inlines the setup wizard on
///                                    first boot if no config exists.
/// - `svrn daemon run [flags]` → unchanged; the OS-service entry point.
/// - `svrn daemon --flag ...`  → bare flags (e.g. `--setup-only`)
///                                    route to `run` so users can type
///                                    `svrn daemon --setup-only` without
///                                    the explicit `run` token.
/// - `svrn daemon <known>`     → start/stop/restart/reload/status as
///                                    before.
pub async fn run(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
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
            // Bare flags like `svrn daemon --setup-only` route
            // straight to run_daemon — the user means "start the
            // daemon (or its first-boot wizard) with these flags."
            run_daemon(args).await
        }
        Some(other) => {
            eprintln!("error: unknown daemon subcommand '{other}'");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
        None => {
            // Bare `svrn daemon` — Phase 4 routes this to
            // run_daemon so first-time users get a working daemon
            // without hunting for the magic `run` keyword. launchd
            // and systemd unit files keep using `daemon run`
            // explicitly; both paths land in the same place.
            run_daemon(&[]).await
        }
    }
}

/// Public entry for `svrn setup` (Phase 4 shim). Runs only the
/// wizard portion (hardware detect → model pick → config write); does
/// NOT register a service or load models. The setup_cmd module's
/// `run_setup` calls into this so both `svrn setup` and
/// `svrn daemon --setup-only` share one code path.
pub async fn run_setup_only(args: &[String]) -> i32 {
    let mut forwarded = vec!["--wizard-only".to_string()];
    forwarded.extend(args.iter().cloned());
    crate::setup_cmd::run_setup(&forwarded).await
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn daemon",
    summary: "Long-running OICP server with managed inference + MCP tools.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn daemon [--setup-only] | sovereign daemon <subcommand>",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--setup-only", "Run the first-boot wizard (hardware detect + model pick + config) and exit without binding the listener."),
        ]),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("(bare)",  "Run the daemon in the foreground. On first boot inlines the setup wizard; subsequent runs just load config and start. Equivalent to `daemon run`."),
            ("run",     "Same as bare — kept for explicit invocation by launchd / systemd unit files."),
            ("start",   "Start the daemon in the background (detached child + PID file at ~/.sovereign/daemon.pid). Waits for readiness."),
            ("status",  "Report whether the daemon is running and answering on :9741."),
            ("stop",    "Stop the daemon cleanly (SIGTERM). Tries the PID file first, then looks up the listener on :9741 via lsof/ss, then falls back to launchctl / systemctl."),
            ("reload",  "Apply config changes without a restart (POST /v1/admin/reload)."),
            ("restart", "Hard-restart via launchctl / systemctl. Drops in-flight requests."),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Logs: ~/.sovereign/logs/daemon.log. To register as a launchd/systemd service, run `svrn install-service`.",
        ),
    ],
};

async fn run_daemon(args: &[String]) -> i32 {
    // ── Worker-mode branch (ephemeral pod) ────────────────────────
    //
    // `svrn daemon run --worker-mode` runs an ephemeral worker
    // daemon (see `sovereign/docs/EPHEMERAL_WORKER_PODS.md`) instead
    // of a full persistent peer. The worker boots with a bootstrap
    // blob (env `SOVEREIGN_BOOTSTRAP` or `--bootstrap-blob <file>`),
    // serves the four owner-only routes on `:9742` over a
    // seed-derived self-signed TLS cert, and exits when the owner
    // sends `DELETE /internal/worker/job` (or process is signalled).
    //
    // Worker mode skips every persistent-peer surface: no SetupConfig
    // (no inference models), no mesh state machine, no
    // /v1/chat/completions exposure. The binary is the same, but the
    // wiring branches here and stays in worker_daemon.rs from this
    // point forward.
    if args.iter().any(|a| a == "--worker-mode") {
        return run_worker_daemon(args).await;
    }

    // ── Phase 4 flag parsing ──────────────────────────────────────
    //
    // `--setup-only` runs the wizard and exits without binding the
    // listener. Useful for users who want to configure the host now
    // and start the daemon manually later. Other flags pass through
    // to the daemon-start path; unrecognised flags are tolerated for
    // forward-compatibility (the daemon doesn't accept tunables on
    // the command line, only via the config file).
    let setup_only = args.iter().any(|a| a == "--setup-only");

    // `--config <path>` overrides the default `~/.sovereign/config.toml`
    // path. Phase 2 of EPHEMERAL_WORKER_PODS uses this to point the
    // child daemon spawned by `SubprocessRunner` at the auto-generated
    // pod-side config (written by `worker_http::write_child_daemon_config`).
    // Production launchd/systemd units don't pass `--config`; they
    // continue to use the canonical path. The wizard short-circuit
    // above still checks `exists()` at the canonical path even when
    // `--config` is set — that's intentional: if the operator passes
    // `--config` they're telling us they have a config, so we skip
    // the wizard entirely and surface a clean error if the file is
    // missing.
    let config_override: Option<std::path::PathBuf> = {
        let mut path: Option<std::path::PathBuf> = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--config" {
                if let Some(p) = it.next() {
                    path = Some(std::path::PathBuf::from(p));
                }
            }
        }
        path
    };

    // ── Phase 4 first-boot wizard ─────────────────────────────────
    //
    // Pre-Phase-4 the daemon refused to start with a "run sovereign
    // setup first" hint. Now we inline the wizard so a user typing
    // `svrn daemon` on a fresh box gets a working setup. The
    // wizard prompts for model selection, so it requires a TTY: a
    // launchd-spawned daemon with no config will fall through to
    // the same hint as before, since `is_terminal()` returns false
    // in that environment.
    // When `--config <path>` is passed, the operator owns the config
    // file's existence — skip both the `exists()` short-circuit and
    // the interactive wizard. Otherwise fall through to the
    // canonical-path checks.
    if config_override.is_none() && !sovereign_core::setup_config::SetupConfig::exists() {
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "error: no config at {}",
                SetupConfig::default_path().display()
            );
            eprintln!(
                "hint: launchd/systemd can't run the interactive wizard. \
                 Run `svrn daemon --setup-only` from a terminal first."
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
    // daemon processes. See `crate::log_rotation` for the contract.
    //
    // Ordered FIRST so a daemon that's been running for days and
    // produced a 5-GB log doesn't make the operator's `tail -f` drop
    // dead before the new daemon prints its first useful line.
    let log_dir = home_dir_buf().join(".sovereign").join("logs");
    crate::log_rotation::rotate_daemon_logs(
        &log_dir,
        crate::log_rotation::DEFAULT_SIZE_CAP_BYTES,
        crate::log_rotation::DEFAULT_KEEP_N_BAKS,
    );
    // 30-minute periodic rotation so a daemon that runs continuously
    // for days stays bounded between launchd restarts. The interval is
    // a knob — shorter cadence catches bursts faster but adds I/O
    // wakeups; 30 min is comfortably long for a stat() + size check.
    let _rotation_handle = crate::log_rotation::spawn_rotation_loop(
        log_dir.clone(),
        crate::log_rotation::DEFAULT_SIZE_CAP_BYTES,
        crate::log_rotation::DEFAULT_KEEP_N_BAKS,
        std::time::Duration::from_secs(30 * 60),
    );

    // ── Memory watch ──────────────────────────────────────────────
    // 60s RSS sampler: publishes the latest sample, warns above the
    // soft limit, and (opt-in hard limit) self-SIGTERMs with a
    // non-zero exit so the service manager relaunches a clean process
    // before jetsam can SIGKILL mid-write. See `crate::memory_watch`.
    let _memory_watch_handle =
        crate::memory_watch::spawn_memory_watch(std::time::Duration::from_secs(60));

    // ── Load config ───────────────────────────────────────────────
    let config = match config_override.as_ref() {
        Some(path) => match SetupConfig::load_from(path) {
            Ok(c) => {
                eprintln!("[daemon] loaded config from {}", path.display());
                c
            }
            Err(e) => {
                eprintln!("error: --config {}: {e}", path.display());
                return 1;
            }
        },
        None => match SetupConfig::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                eprintln!("hint: run `svrn daemon --setup-only` to (re-)create the config.");
                return 1;
            }
        },
    };

    // Shared-model cluster role → RPC env contract. The desktop fleet
    // sets `[shared_model] role` instead of SOVEREIGN_RPC_* by hand;
    // translate it here, once, before any RPC consumer reads the env
    // (the inference serve call_once, the discovery loop below, and
    // commonwealth-api's /status advertise). An explicit env var wins.
    bootstrap::apply_shared_model_role_to_env(&config.shared_model);

    // Route llama.cpp's internal log into our tracing layer. Without
    // this, gguf load failures and ggml backend diagnostics print to a
    // dropped stderr (the daemon's child-style stdio capture swallows
    // them) — the operator gets a bare "null result from llama cpp"
    // with no actionable detail. Installed exactly once per process.
    sovereign_inference::llama::install_log_tracing();

    // VRAM capacity preflight — refuse a config that would overcommit
    // VRAM (full rationale on `build::preflight::check_vram`). Bypass with
    // SOVEREIGN_SKIP_VRAM_CHECK=1.
    if !build::preflight::check_vram(&config) {
        return 1;
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
    if config.daemon.force_tool_calls && std::env::var("SOVEREIGN_FORCE_TOOL_CALLS").is_err() {
        std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
        tracing::info!(
            "daemon: force_tool_calls=true — grammar engaged on every \
             tools-using request (set via setup_config.toml)"
        );
    }

    // ── Alternation-grammar config → process env ──────────────────
    //
    // Same propagation pattern as force_tool_calls. The inference
    // adapter reads `SOVEREIGN_ALTERNATION_GRAMMAR` per request to
    // route tool-envelope requests through llguidance's canonical
    // `TopLevelGrammar::from_json_schema` path instead of the
    // in-house `JsonConstraint` mask. Caller-supplied env wins so
    // operators can A/B test (`SOVEREIGN_ALTERNATION_GRAMMAR=0
    // sovereign daemon run` ignores the config).
    //
    // launchd-spawned daemons don't inherit caller env, so flipping
    // this in setup_config.toml is the load-bearing path on macOS
    // hosts running the daemon via `svrn daemon start`.
    if config.daemon.alternation_grammar && std::env::var("SOVEREIGN_ALTERNATION_GRAMMAR").is_err()
    {
        std::env::set_var("SOVEREIGN_ALTERNATION_GRAMMAR", "1");
        tracing::info!(
            "daemon: alternation_grammar=true — llguidance schema path \
             engaged on tools-using requests (set via setup_config.toml)"
        );
    }

    // Inference provider — load the embedded llama.cpp provider (3 GGUF
    // slots + extras/idle/rerank wiring); full rationale on
    // `build::inference::load_provider`. `engine_handle` (concrete) feeds
    // the RPC-worker auto-reload path; `resolved_embed_family` feeds the
    // mesh embed-model advertisement.
    let (provider, raw_engine, resolved_embed_family) =
        match build::inference::load_provider(&config) {
            Ok(t) => t,
            Err(()) => return 1,
        };
    let engine_handle: Option<Arc<EmbeddedLlamaCpp>> = Some(raw_engine);

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
    // NoteStore is built early (other subsystems take it as
    // `Arc<NoteStore>`), but `embed_fn`, `origin_node_id`, and
    // `propagation_sink` aren't known yet. They wire post-Arc
    // via the OnceLock setters at three later seams in this
    // function — search this file for `set_origin_node_id`,
    // `set_embed_fn`, and `set_propagation_sink` to find the
    // wiring sites.

    // ── Lint / test result stores ─────────────────────────────────
    // Always opened so the agent-facing `lint_status` / `test_status`
    // tools have a backing store to read from. When no watcher is
    // configured (no workspace resolved, or sovereign.toml has no
    // [lint_runner]/[test_runner]), the tools report `never_run` —
    // accurate and unambiguous.
    let lint_store: Arc<LintResultStore> =
        match LintResultStore::open(&data_dir.join("lint_results.db")) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!(
                    "error: cannot open lint results db {}: {e}",
                    data_dir.join("lint_results.db").display()
                );
                return 1;
            }
        };
    let test_store: Arc<TestResultStore> =
        match TestResultStore::open(&data_dir.join("test_results.db")) {
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
    let bootstrap::WatcherAtlasSetup {
        watcher_heartbeat,
        lint_watcher,
        test_watcher,
        watched_lint_scope,
        watched_test_scope,
        watcher_monitor: _watcher_monitor,
        work_atlas_mesh_store,
        work_atlas_store,
        work_atlas_broadcaster,
        work_atlas_cfg,
        work_atlas_repo_root,
        work_atlas_repo_id,
        work_atlas_branch,
    } = bootstrap::setup_watchers_and_work_atlas(
        &workspace_dir,
        &data_dir,
        Arc::clone(&lint_store),
        Arc::clone(&test_store),
    );

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
    let self_node_id = bootstrap::resolve_self_node_id(&data_dir);
    // Stamp outbound NoteStore propagation events with this node
    // id. `content_hash` is the dedup primary key on the gossip
    // wire so `origin_node_id` rotation (toolbx rebuilds without
    // ~/.sovereign bind-mount) doesn't create duplicates — this
    // field is informational, surfaced in the audit display.
    if let Err(e) = notes_store.set_origin_node_id(self_node_id.to_string()) {
        tracing::warn!(
            target = "notes",
            error = e,
            "notes: origin_node_id already set — wiring race?"
        );
    }

    // GliNER per-chunk entity extractor — hoisted out of the engine
    // block so both the engine's tiered runner (conv corpora) AND the
    // folder_tiered_deps below can share the same Arc<dyn> handle
    // (the underlying GlinerExtractor is ~150MB ONNX; one load only).
    //
    // The raw `Arc<GlinerExtractor>` is hoisted alongside the
    // trait-object wrapper so the NoteStore T2 path can install
    // it as a `GlinerFn` adapter without re-loading the model.
    let (gliner_raw, chunk_entity_extractor) = bootstrap::load_gliner_extractor(&data_dir);

    let engine: Arc<CorpusEngine> = bootstrap::build_corpus_engine(
        &data_dir,
        Arc::clone(&provider),
        Arc::clone(&notes_store),
        &gliner_raw,
        &config,
        self_node_id,
        &chunk_entity_extractor,
    );

    // ── Folder tiered deps ───────────────────────────────────────
    // Watched-folder corpora reuse the conv-tiered table shape
    // (`conv_*` tables, conv_uuid = corpus_id) via the
    // `FolderTieredProvider`. The driver opens its own
    // SqliteStateStore handle so this block is independent of the
    // engine-side conv provider; both share the underlying db file
    // (`~/.sovereign/sovereign.db`).
    //
    // Installed on the manager via `set_tiered_deps` after the
    // manager is constructed (~line 1593 below). Without these,
    // `enable_enrichment` falls back to the legacy subprocess.
    let folder_tiered_deps = bootstrap::build_folder_tiered_deps(
        &data_dir,
        Arc::clone(&provider),
        chunk_entity_extractor,
    );

    // ── Solve job table ───────────────────────────────────────────
    // Shared between the /v1/solve/jobs HTTP router (installed in
    // install_http_and_mcp below) and the solve/solve_status/
    // solve_cancel MCP tools (registered in build_tool_registry) —
    // an MCP agent and a curl session see the same jobs. The solver
    // calls back into this daemon's own /v1/chat/completions over
    // loopback.
    let solve_jobs = Arc::new(solve_http::SolveJobs::new(config.daemon.client_port));

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
        Arc::clone(&watcher_heartbeat),
        workspace_dir.clone(),
        Arc::clone(&work_atlas_store),
        work_atlas_cfg.clone(),
        Arc::clone(&work_atlas_broadcaster),
        work_atlas_repo_root.clone(),
        work_atlas_repo_id.clone(),
        work_atlas_branch.clone(),
        Arc::clone(&solve_jobs),
    )
    .await;

    let (daemon, mesh_provider) =
        bootstrap::build_mesh_providers(&data_dir, Arc::clone(&provider)).await;
    let routed_provider: Arc<dyn InferenceProvider> = mesh_provider.clone();
    daemon
        .set_inference_provider(Arc::clone(&routed_provider))
        .await;

    // Host side of distributed-inference auto-warm. When this node distributes a
    // large primary across mesh workers, the embedded engine calls this seam to
    // seed each worker's shard BEFORE loading — so the load is all cache hits and
    // never streams a large weight share (the upload deadlock). This retires the
    // manual `SOVEREIGN_RPC_ASSUME_WARMED` for the common case. Installed
    // unconditionally (harmless on a node that never distributes) so both
    // auto-discovered and manual (`SOVEREIGN_RPC_WORKERS`) hosts auto-warm.
    sovereign_mesh::rpc_warm_http::install_rpc_warm_orchestrator(Arc::clone(&daemon));

    bootstrap::spawn_rpc_worker_discovery(Arc::clone(&daemon), engine_handle);

    bootstrap::spawn_slot_alias_push(Arc::clone(&daemon), mesh_provider);

    // Hand the engine to the mesh daemon so the auto_ingest loop and
    // /internal/corpus/* HTTP surface can both see in-progress
    // wikipedia/etc. ingests. See engine block above for the
    // diagnostic story.
    daemon.set_corpus_engine(Arc::clone(&engine)).await;

    // Wire the work-atlas's shared `MeshStore` into the daemon BEFORE
    // `try_resume`. Once the daemon transitions to Running its
    // `AppState.mesh_store` is this exact `Arc<MeshStore>`, so:
    //   - the work-atlas tools (`declare_scope`, etc.) write into
    //     the store gossip's `all_entries_for_gossip` enumerates;
    //   - peer broadcasts arriving at `/internal/app/state` merge
    //     into the same instance the atlas reads from.
    // Without this, the daemon would have constructed its own
    // independent in-memory store and atlas data would be invisible
    // across the mesh.
    daemon
        .set_mesh_store(Arc::clone(&work_atlas_mesh_store))
        .await;

    bootstrap::wire_note_propagation_sink(
        Arc::clone(&notes_store),
        Arc::clone(&work_atlas_mesh_store),
        self_node_id,
    );

    bootstrap::spawn_notes_tier_backfill(Arc::clone(&notes_store));

    bootstrap::spawn_notes_ingest_poller(
        Arc::clone(&work_atlas_mesh_store),
        Arc::clone(&notes_store),
        self_node_id,
    );

    bootstrap::spawn_lazy_stamp_fingerprints(Arc::clone(&engine));

    bootstrap::spawn_tier2_enrichment_resume(&data_dir);

    bootstrap::advertise_embed_model(
        Arc::clone(&provider),
        &config,
        resolved_embed_family,
        Arc::clone(&daemon),
    )
    .await;

    bootstrap::install_http_and_mcp(
        Arc::clone(&daemon),
        tools,
        Arc::clone(&notes_store),
        &config,
        Arc::clone(&solve_jobs),
    )
    .await;

    // Keep the reindexer alive for the lifetime of the daemon.
    // The variable binding is load-bearing — dropping the Arc
    // stops every supervised watcher.
    let _reindexer_handle = bootstrap::start_freshness_pipeline(
        &data_dir,
        Arc::clone(&notes_store),
        Arc::clone(&daemon),
        Arc::clone(&engine),
        Arc::clone(&provider),
    )
    .await;

    let _watched_subsystem = bootstrap::setup_watched_folders(
        Arc::clone(&engine),
        &data_dir,
        &config,
        folder_tiered_deps,
        Arc::clone(&daemon),
    )
    .await;

    // ── Resume or bootstrap a solo mesh ───────────────────────────
    match daemon.try_resume().await {
        Ok(true) => {
            tracing::info!("mesh resumed from persisted state");
        }
        Ok(false) => {
            // First boot, no persisted mesh. A fleet JOINER (config carries a
            // `[discovery] join_key`) joins an existing mesh through its static
            // seed addresses; a founder / standalone node (no join_key) creates
            // a silent solo mesh so the listener comes up.
            let hostname = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "sovereign".to_string());
            let disc = &config.discovery;
            match disc.join_key.as_deref() {
                // Configured fleet joiner: try each static seed as a direct
                // `/internal/join` target (no mDNS needed) until one accepts.
                // Hard-fail rather than fall back to a solo mesh — that would
                // split-brain the fleet.
                Some(join_key) if !disc.seed_addrs.is_empty() => {
                    let mut joined = false;
                    for seed in &disc.seed_addrs {
                        let link = sovereign_mesh::DeepLink::Join {
                            join_key: join_key.to_string(),
                            relay_hint: Some(seed.clone()),
                            mesh_name: None,
                            iroh_dial: None,
                            encrypted: false,
                            expires_at: None,
                        };
                        match daemon.join_mesh(&link, &hostname).await {
                            Ok(_) => {
                                tracing::info!(seed = %seed, "joined fleet via configured seed");
                                joined = true;
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    seed = %seed,
                                    error = %e,
                                    "seed join failed; trying next seed"
                                );
                            }
                        }
                    }
                    if !joined {
                        eprintln!(
                            "error: could not join the mesh via any of the {} configured \
                             [discovery] seed_addrs — check the addresses are reachable and \
                             the join_key matches the founder's mesh",
                            disc.seed_addrs.len()
                        );
                        return 1;
                    }
                }
                // join_key set but nowhere to send it — a joiner with no way in.
                Some(_) => {
                    eprintln!(
                        "error: [discovery] join_key is set but seed_addrs is empty — \
                         a fleet joiner needs at least one reachable seed address"
                    );
                    return 1;
                }
                // No join credential: founder / standalone node.
                None => {
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
        "svrn daemon is running"
    );

    let _work_atlas_gc_handle = bootstrap::finalize_work_atlas(
        Arc::clone(&daemon),
        Arc::clone(&work_atlas_broadcaster),
        Arc::clone(&work_atlas_store),
        work_atlas_cfg.clone(),
    );

    bootstrap::install_foreground_yield_hook(
        Arc::clone(&daemon),
        lint_watcher.clone(),
        test_watcher.clone(),
    );

    eprintln!(
        "svrn daemon running — http://localhost:{}/v1 + /mcp",
        config.daemon.client_port
    );

    let (pid_path, self_pid) = bootstrap::write_pidfile();

    // ── Block until SIGINT/SIGTERM, then drain, persist, and exit ──
    shutdown_daemon(daemon, &pid_path, self_pid).await
}

/// Graceful shutdown choreography, extracted from `run_daemon` so its tail
/// reads as one named step. Blocks on SIGINT/SIGTERM, persists mesh state (NOT
/// `leave()` — that would force a fresh solo mesh on next boot), removes our
/// pidfile if it still points at us, and returns the process exit code: `102`
/// on the memory watcher's RSS-hard-limit path (so launchd/systemd relaunch),
/// `0` on every deliberate shutdown. On macOS it `_exit`s to skip the
/// ggml-metal destructor assertion (full rationale inline).
async fn shutdown_daemon(
    daemon: Arc<sovereign_mesh::EmbeddedDaemon>,
    pid_path: &std::path::Path,
    self_pid: u32,
) -> i32 {
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
    if let Ok(raw) = std::fs::read_to_string(pid_path) {
        if raw.trim().parse::<u32>().ok() == Some(self_pid) {
            let _ = std::fs::remove_file(pid_path);
        }
    }

    eprintln!("svrn daemon stopped");

    // macOS-specific: bypass C++ static destructors at process exit
    // to dodge a known `ggml-metal-device.m:618 GGML_ASSERT` firing
    // inside `__cxa_finalize_ranges → ggml_metal_device_free`. The
    // assertion checks Metal resource-set drain; our llama contexts
    // are owned by `Arc<EmbeddedLlamaCpp>` references scattered
    // across AppState, MeshInferenceProvider, the inference adapter,
    // and several background tasks. Drop ordering is non-trivial,
    // and even one straggling reference (e.g., a slot guard held
    // briefly by a closing in-flight request) leaves a non-empty
    // resource set when `exit()` walks the destructor table —
    // SIGABRT, misleading "daemon crashed" log.
    //
    // We've already run the graceful shutdown path:
    //   - `daemon.shutdown().await` persisted mesh.json
    //   - axum::serve drained in-flight requests
    //   - the pidfile is removed
    //   - tracing-subscriber writes line-buffered to stderr
    //
    // Everything else (Metal devices, KV caches, mmap'd ggufs) is
    // reclaimed by the kernel on `_exit`. Confirmed 2026-05-20: this
    // is the same shutdown shape `llama-server` uses (`_Exit` from
    // its signal handler).
    //
    // Linux + other targets keep the standard return path — Metal is
    // macOS-only, so the assertion only fires on darwin.
    // Exit code contract: the memory watcher's hard-limit path needs a
    // NON-ZERO exit so launchd (`KeepAlive.SuccessfulExit=false`) /
    // systemd (`Restart=on-failure`) relaunch the daemon; every other
    // shutdown is deliberate and must stay 0 (= stays down).
    let exit_code: i32 = if crate::memory_watch::hard_exit_requested() {
        eprintln!("svrn daemon exiting non-zero: RSS hard limit (service manager will relaunch)");
        102
    } else {
        0
    };
    #[cfg(target_os = "macos")]
    {
        // Reuse the shared fast-exit (lifted to sovereign-inference 2026-06-16
        // so the desktop app shares it). Skips `__cxa_finalize_ranges` so the
        // ggml-metal device sweeper never asserts on still-resident resources.
        sovereign_inference::fast_exit_skip_destructors(exit_code)
    }
    #[cfg(not(target_os = "macos"))]
    {
        exit_code
    }
}

/// Build the tool registry that serves `/mcp/*`. Mirrors the subset of
/// tools `svrn project serve` registers. When no code indexes
/// are installed, tools return helpful "not indexed" messages rather
/// than erroring, so a freshly-setup daemon is still useful for
/// `write_note` / `read_notes`.
/// macOS shutdown helper — see `run_daemon` for rationale. Calls
/// `_exit(2)` to skip libc's `__cxa_finalize_ranges` chain so the
/// ggml-metal device sweeper never gets a chance to assert on
/// still-resident llama-context resources.
#[cfg(target_os = "macos")]
unsafe fn fast_exit_skip_destructors(code: i32) -> ! {
    extern "C" {
        #[link_name = "_exit"]
        fn libc_exit_no_finalize(status: i32) -> !;
    }
    libc_exit_no_finalize(code)
}

fn home_dir_buf() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
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
