// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon run` — the hidden subcommand that launchd/systemd
//! calls to actually run the embedded Commonwealth daemon in the
//! foreground. Humans don't invoke this directly; they go through
//! `svrn setup` (which registers the service) and then let the
//! service manager keep it alive.
//!
//! Responsibilities:
//! 1. Read `~/.svrnmesh/config.toml` for model paths + ports.
//! 2. Build an `EmbeddedLlamaCpp` inference provider from the three
//!    GGUF slots (primary / fast / embed).
//! 3. Build a `ToolRegistry` + `NoteStore` so `/mcp/*` has tools.
//! 4. Build every service the daemon needs — engine, mesh-routed provider,
//!    tool mount, project / knowledge-view / solve routers, shared stores —
//!    then commission `EmbeddedDaemon` with all of them in ONE
//!    `DaemonServices::Headless` value, so `:9741` serves `/v1/*`, `/mcp/*`
//!    and the rest with no post-construction wiring step to forget.
//! 5. `try_resume()` the persisted mesh; on first run where no
//!    `mesh.json` exists, create a silent "solo" mesh so the listener
//!    comes up. `svrn mesh rotate` (future) can later print a
//!    shareable join key.
//! 6. Block on `tokio::signal::ctrl_c()` so the service manager
//!    controls lifecycle.

use std::io::IsTerminal as _;
use std::sync::Arc;

use corpus_engine::CorpusEngine;
use corpus_engine_notes::NoteStore;
use corpus_engine_watchers::{LintResultStore, TestResultStore};
use sovereign_contracts::launch::Launch;
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbeddedLlamaCpp;

// §3.2 split: the run_daemon bootstrap stays here (it's the orchestrator);
// the separable lifecycle / workspace / provider / worker / tool-registry
// concerns moved to submodules. `home_dir_buf` + `warn_orphaned_indexes`
// stay here (the former is shared with submodules as an ancestor-private).
pub(crate) mod bootstrap;
pub(crate) mod build;
mod discovery_policy;
// `pub(crate)` so `setup_cmd::fim` can reach `restart_daemon` directly.
// `svrn setup --fim` rewrites the model config and must bounce the
// daemon itself — telling the operator to go run `svrn daemon restart`
// mid-flow would break the one-command promise and leave the verify
// ladder below with nothing to verify.
pub(crate) mod lifecycle;
// Headless OCR install. Compiled unconditionally — the module carries both
// cfg arms of `install_ocr_ctx` so the single call site in `bootstrap` never
// grows a `#[cfg]`, and a build without `--features ocr` still logs WHY OCR
// is unavailable instead of doing nothing.
mod ocr_install;
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
pub async fn run(launch: &Launch, args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    match args.first().map(String::as_str) {
        Some("run") => run_daemon(launch, &args[1..]).await,
        Some("start") => start_daemon(&args[1..]).await,
        Some("stop") => stop_daemon().await,
        Some("restart") => restart_daemon(&args[1..]).await,
        Some("reload") => reload_daemon().await,
        Some("status") => status_daemon().await,
        Some(flag) if flag.starts_with("--") => {
            // Bare flags like `svrn daemon --setup-only` route
            // straight to run_daemon — the user means "start the
            // daemon (or its first-boot wizard) with these flags."
            run_daemon(launch, args).await
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
            run_daemon(launch, &[]).await
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
            "svrn daemon [--setup-only] [--rpc-worker[=<bind>]] | svrn daemon <subcommand>",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--setup-only", "Run the first-boot wizard (hardware detect + model pick + config) and exit without binding the listener."),
            ("--rpc-worker[=<bind>]", "Lend this node's GPU to the mesh: serve an llama.cpp RPC worker so peers can place layers here. Default bind 0.0.0.0:50052. Works on `run`, `start` and `restart`. This only OFFERS the GPU — unlike `[shared_model] role = \"anchor\"`, it does not also turn on peer discovery or enter the host election."),
        ]),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("(bare)",  "Run the daemon in the foreground. On first boot inlines the setup wizard; subsequent runs just load config and start. Equivalent to `daemon run`."),
            ("run",     "Same as bare — kept for explicit invocation by launchd / systemd unit files."),
            ("start",   "Start the daemon in the background (detached child + PID file at ~/.svrnmesh/daemon.pid). Waits for readiness."),
            ("status",  "Report whether the daemon is running and answering on :9741."),
            ("stop",    "Stop the daemon cleanly (SIGTERM). Tries the PID file first, then looks up the listener on :9741 via lsof/ss, then falls back to launchctl / systemctl."),
            ("reload",  "Apply config changes without a restart (POST /v1/admin/reload)."),
            ("restart", "Hard-restart via launchctl / systemctl. Drops in-flight requests."),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Logs: ~/.svrnmesh/logs/daemon.log. To register as a launchd/systemd service, run `svrn install-service`.",
        ),
    ],
};

async fn run_daemon(launch: &Launch, args: &[String]) -> i32 {
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
    //
    // THE LAUNCH ANSWERS THIS, not a second argv scan. Until 2026-08-25 this
    // line read `args.iter().any(|a| a == "--worker-mode")` — the last
    // surviving launch-mode READER outside `Launch::parse`, and a §10.6
    // duplicate created by the refactor that introduced `Launch`: `dispatch`
    // collapsed `Daemon` and `Worker` into one `daemon_cmd::run` call, so
    // `Launch` answered and this function asked again. Threading the `Launch`
    // itself (rather than re-deriving from `args`) is what makes the two
    // agree by construction — and it sidesteps the arg-shape mismatch that
    // deferred this fix, since `Launch::Worker` carries argv INCLUDING the
    // `run` subcommand while `run_worker_daemon` wants it stripped.
    // Falsifier 1, readers: 1 -> 0.
    if matches!(launch, Launch::Worker { .. }) {
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

    // `--config <path>` overrides the default `~/.svrnmesh/config.toml`
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
    let log_dir = sovereign_root().join("logs");
    crate::log_rotation::rotate_daemon_logs(
        &log_dir,
        crate::log_rotation::DEFAULT_SIZE_CAP_BYTES,
        crate::log_rotation::DEFAULT_KEEP_N_BAKS,
    );
    // 30-minute periodic rotation so a daemon that runs continuously
    // for days stays bounded between launchd restarts. The interval is
    // a knob — shorter cadence catches bursts faster but adds I/O
    // wakeups; 30 min is comfortably long for a stat() + size check.
    // Supervised: a panic must not silently stop rotation for the rest
    // of the process's life (DAEMON_RESILIENCE.md P0.4).
    let _rotation_handle = crate::supervise::spawn_supervised("log_rotation", {
        let log_dir = log_dir.clone();
        move || {
            crate::log_rotation::rotation_loop(
                log_dir.clone(),
                crate::log_rotation::DEFAULT_SIZE_CAP_BYTES,
                crate::log_rotation::DEFAULT_KEEP_N_BAKS,
                std::time::Duration::from_secs(30 * 60),
            )
        }
    });

    // ── Memory watch ──────────────────────────────────────────────
    // 60s RSS sampler: publishes the latest sample and warns above the
    // soft limit. The hard limit (self-SIGTERM with a non-zero exit so a
    // service manager relaunches a clean process before jetsam SIGKILLs
    // mid-write) is OFF by default — it only helps under a supervisor, so
    // it must be opted into via `SOVEREIGN_RSS_HARD_LIMIT_MB=<mb>|auto`
    // (`scripts/daemon-supervised.sh` sets it). See `crate::memory_watch`.
    // Supervised: a panicked sampler used to silently disarm the OOM
    // defense (DAEMON_RESILIENCE.md P0.4).
    let _memory_watch_handle = crate::supervise::spawn_supervised("memory_watch", || {
        crate::memory_watch::watch_loop(std::time::Duration::from_secs(60))
    });

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

    // ── Is our data root the one that holds this machine's data? ──
    //
    // Four directories have been a data root across releases, and
    // `data_dir()` always returns a plausible one — which is what made a
    // wrong answer invisible (note `b2aa9fb8`). Classify before claiming:
    // starting fresh on top of live data somewhere else is a silent
    // substitution, and the daemon is the surface where it is worst (a
    // service that boots into an empty universe and reports healthy).
    match sovereign_contracts::data_roots::classify(&config.data.dir) {
        v if v.is_refusal() => {
            eprintln!("error: data root {}: {v}", config.data.dir.display());
            return 1;
        }
        sovereign_contracts::data_roots::RootConflict::Clear => {}
        v => tracing::warn!(
            target: "daemon",
            root = %config.data.dir.display(),
            "data roots: {v}"
        ),
    }

    // ── Single-instance guard (DAEMON_RESILIENCE.md P0.5) ─────────
    //
    // Taken as early as the thing it protects is KNOWN — which is here,
    // right after the config parse, not before it. The lock is keyed on the
    // DATA ROOT (`RunLock`), and until 2026-08-24 it was keyed on `$HOME`
    // and therefore had to be taken before the config was read; that key
    // refused three soak nodes with three data dirs under one HOME and
    // admitted two processes onto one data dir from two HOMEs. A TOML parse
    // is the only thing that now happens first, and nothing heavy — no
    // model, no listener, no store — has been touched.
    //
    // Held for the process lifetime: the kernel releases it on any exit,
    // including SIGKILL, so there is no stale-lock cleanup path.
    let _run_lock = match sovereign_contracts::run_lock::RunLock::acquire(&config.data.dir) {
        Ok(lock) => {
            tracing::debug!(
                target: "daemon",
                lock = %lock.path().display(),
                enforced = lock.is_enforced(),
                "run lock: claimed the data root"
            );
            lock
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    // Shared-model cluster role → RPC env contract. The desktop fleet
    // sets `[shared_model] role` instead of SOVEREIGN_RPC_* by hand;
    // translate it here, once, before any RPC consumer reads the env
    // (the inference serve call_once, the discovery loop below, and
    // commonwealth-api's /status advertise). An explicit env var wins.
    // `--rpc-worker` first: it is the operator saying it out loud on this
    // invocation, and the role translation below only fills in what is unset.
    bootstrap::apply_rpc_worker_flag(args);
    bootstrap::apply_shared_model_role_to_env(&config.shared_model);

    // Route llama.cpp's internal log into our tracing layer. Without
    // this, gguf load failures and ggml backend diagnostics print to a
    // dropped stderr (the daemon's child-style stdio capture swallows
    // them) — the operator gets a bare "null result from llama cpp"
    // with no actionable detail. Installed exactly once per process.
    sovereign_inference::llama::install_log_tracing();

    // VRAM capacity preflight — ADVISORY by default: warns and starts
    // anyway on overcommit (so CPU-only / low-VRAM machines aren't
    // hard-blocked). Only refuses under SOVEREIGN_STRICT_VRAM_CHECK=1 or
    // when a model file is unreadable. Full rationale on
    // `build::preflight::check_vram`.
    // Name the config the operator actually passed, not the default one —
    // a `--config` start used to be told to edit a file it never read.
    let config_path_in_use = config_override
        .clone()
        .unwrap_or_else(sovereign_core::setup_config::SetupConfig::default_path);
    if !build::preflight::check_vram_reporting(&config, &config_path_in_use) {
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
    // a one-shot test (`SOVEREIGN_FORCE_TOOL_CALLS=0 svrn daemon
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
    // svrn daemon run` ignores the config).
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
    //
    // Minted HERE, before the provider, because a terminal's provider binds to
    // its entry node THROUGH this handle: the bind is a mesh identity, resolved
    // per turn, and the mesh view does not exist yet. `DeferredDaemon` answers
    // exactly as a commissioned-but-stopped daemon until `bind` — no peers — so
    // a terminal booting ahead of gossip reports its entry node unreachable
    // rather than inventing an address for it.
    let deferred_daemon = Arc::new(sovereign_mesh::DeferredDaemon::new());
    let (provider, raw_engine, resolved_embed_family, distributed_primary_slot) =
        match build::inference::load_provider(&config, Arc::clone(&deferred_daemon)) {
            Ok(t) => t,
            Err(()) => return 1,
        };
    // `None` whenever nothing in this process owns llama slots — TWO ways in
    // now, and the engine-only paths (RPC-worker auto-reload, slot hot-swap)
    // must see the absence rather than a stub either way:
    //   - a `terminal`, which holds no weights at all and forwards instead;
    //   - an engine configured with no local llama slots, where the
    //     RPC-worker reload below is llama's own and simply does not arm.
    // Already an `Option` before either existed; both make the `None` reachable.
    let engine_handle: Option<Arc<EmbeddedLlamaCpp>> = raw_engine;

    // ── Note store (for MCP notes tools + ring-buffer logging) ────
    let data_dir = config.data.dir.clone();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("error: cannot create data dir {}: {e}", data_dir.display());
        return 1;
    }
    // ── The state store — `sovereign.db` (daemon-convergence Phase 3) ────
    //
    // Until now `sovereign daemon run` opened this file NOWHERE, while still
    // mounting `reading_http`: every `conversation-history` chunk it served
    // came back with `title: null`, because the handler resolved the title
    // through a `state_store()` that answered `None` on this variant. That was
    // the single crossing in an otherwise nesting variant lattice — Desktop
    // carried a store, Headless did not, and neither was a superset of the
    // other (`quality/TOPOLOGY.md` §3.5, class D).
    //
    // It is opened HERE, beside `notes.db`, and a failure is fatal rather than
    // degraded: the store is `ServingCore` now, and CORE means the process
    // cannot serve at all without it. Falling back to `InMemoryStateStore`
    // would reproduce exactly the defect being closed — a daemon that answers
    // every conversation lookup with a well-formed nothing (ARCH §18.3).
    //
    // Safe to open unconditionally because of Phase 1: `RunLock` above is
    // keyed on THIS data root, so at most one process is writing this file.
    let state_db_path = data_dir.join("sovereign.db");
    // The CONCRETE handle is kept as well as the trait object: the same
    // `SqliteStateStore` is also the `ConvTieredReader` the turn's enrichment
    // lane reads briefings through (spec CONV_TIERED_PORT.md), and that view
    // is not reachable from `dyn StateStore`. One open, two views — never two
    // opens (TOPOLOGY phase 1: one writer per data root).
    let state_store_concrete = match sovereign_store::sqlite::SqliteStateStore::open(&state_db_path)
    {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "error: cannot open state db {}: {e}",
                state_db_path.display()
            );
            return 1;
        }
    };
    let state_store: Arc<dyn sovereign_core::traits::StateStore> = state_store_concrete.clone();

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
    //   2. ~/.svrnmesh/workspace — a single-line text file with
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
    // ~/.svrnmesh bind-mount) doesn't create duplicates — this
    // field is informational, surfaced in the audit display.
    if let Err(e) = notes_store.set_origin_node_id(self_node_id.to_string()) {
        tracing::warn!(
            target = "notes",
            error = e,
            "notes: origin_node_id already set — wiring race?"
        );
    }
    // The reading half of the same identity. `set_origin_node_id` above
    // decides whose name goes ON outbound notes; this decides whose name
    // a reader sees on the notes coming back — including gossiped ones
    // from peers. Wired together deliberately: a store with only the
    // first renders its own notes as an unrecognised node.
    //
    // The roster is INJECTED rather than read by the notes crate, which
    // is the knowledge layer and holds no mesh types. `persist::load`
    // stays the single reader of mesh.json.
    match bootstrap::build_node_roster(&data_dir, self_node_id) {
        Some(roster) => {
            let self_name = roster.self_name().unwrap_or("<unnamed>").to_string();
            if let Err(e) = notes_store.set_node_roster(roster) {
                tracing::warn!(
                    target = "notes",
                    error = e,
                    "notes: node_roster already set"
                );
            } else {
                tracing::debug!(
                    target = "notes",
                    self_node = %self_node_id,
                    self_name = %self_name,
                    "notes: node roster wired — authors resolve to mesh names"
                );
            }
        }
        None => {
            // Solo node, or mesh.json absent/unparseable. Attribution
            // degrades to the raw id rather than to a guess, so say so
            // once at boot instead of leaving the operator to wonder why
            // every note reads "unrecognised node".
            tracing::debug!(
                target = "notes",
                self_node = %self_node_id,
                "notes: no mesh roster — note authors will render as raw node ids"
            );
        }
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

    let (engine, embed_model_id): (Arc<CorpusEngine>, String) = bootstrap::build_corpus_engine(
        &data_dir,
        Arc::clone(&provider),
        Arc::clone(&notes_store),
        &gliner_raw,
        &config,
        self_node_id,
        &chunk_entity_extractor,
    );

    // Self-healing corpus maintenance. Continuous appenders (the
    // `wikipedia-newsworthy` freshness daemon, watched folders, mesh pulls)
    // leave rows outside the indexes; lancedb then flat-scans them on every
    // search, which is silent, correct, and progressively slower. A desktop
    // user has no way to notice or fix that, so the daemon owns it. See
    // `crate::corpus_maintenance`.
    crate::corpus_maintenance::spawn(Arc::clone(&engine));

    // ── Folder tiered deps ───────────────────────────────────────
    // Watched-folder corpora reuse the conv-tiered table shape
    // (`conv_*` tables, conv_uuid = corpus_id) via the
    // `FolderTieredProvider`. The driver opens its own
    // SqliteStateStore handle so this block is independent of the
    // engine-side conv provider; both share the underlying db file
    // (`~/.svrnmesh/sovereign.db`).
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

    // ── Shared merged SCIP graph ──────────────────────────────────
    // Built ONCE here and handed to BOTH the tool registry (below) and the
    // project reindexer (`start_freshness_pipeline`), so the reindexer's live
    // updates — the tree-sitter overlay on every save and the periodic full
    // rebuild — are visible to `symbols`/`callers`/`blast` immediately, with no
    // daemon restart. Previously each side built its own snapshot and the
    // reindexer's graph had no readers, so the tool surface was frozen at
    // startup — the deepest cause of "the watcher is always stale."
    let merged_scip_handle: sovereign_mesh::reindexer::ScipGraphHandle =
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            tool_registry::build_merged_scip_graph(&data_dir.join("indexes")).await,
        ));

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
        Arc::clone(&merged_scip_handle),
    )
    .await;

    // ── Assemble the daemon's services, THEN commission the daemon ────
    //
    // Order is load-bearing and is the point of daemon-convergence Phase 2:
    // every dependency is built first and handed over in one total value, so
    // there is no window in which a request can reach a half-wired daemon and
    // no slot this bootstrap can forget. `DeferredDaemon` breaks the one
    // genuine cycle — the daemon serves peers through a provider that routes
    // to peers — and carries no capability of its own.
    let (deferred_daemon, mesh_provider) =
        bootstrap::build_mesh_provider(Arc::clone(&provider), deferred_daemon).await;
    let routed_provider: Arc<dyn InferenceProvider> = mesh_provider.clone();

    // Notes-rail convergence recorder (order commons-fluency fix 9):
    // ONE shared instance — named on the daemon's `HeadlessRails` so `/status`
    // reads it, and handed to BOTH the outbound publish sink and the inbound
    // ingest poller so the writers' stamps are what `/status` reports. A second
    // copy would let the status section disagree with the sink — never.
    let convergence_recorder = Arc::new(commonwealth_api::state::ConvergenceRecord::new());

    bootstrap::wire_note_propagation_sink(
        Arc::clone(&notes_store),
        Arc::clone(&work_atlas_mesh_store),
        self_node_id,
        Arc::clone(&convergence_recorder),
    );

    bootstrap::spawn_notes_tier_backfill(Arc::clone(&notes_store));

    bootstrap::spawn_notes_ingest_poller(
        Arc::clone(&work_atlas_mesh_store),
        Arc::clone(&notes_store),
        self_node_id,
        Arc::clone(&convergence_recorder),
    );

    bootstrap::spawn_lazy_stamp_fingerprints(Arc::clone(&engine));

    bootstrap::spawn_tier2_enrichment_resume(&data_dir);

    let advertise_embed =
        bootstrap::advertise_embed_model(Arc::clone(&provider), &config, resolved_embed_family)
            .await;

    // Keep the reindexer alive for the lifetime of the daemon.
    // The variable binding is load-bearing — dropping the Arc
    // stops every supervised watcher.
    let (_reindexer_handle, project_http, knowledge_view_http) =
        bootstrap::start_freshness_pipeline(
            &data_dir,
            Arc::clone(&notes_store),
            Arc::clone(&engine),
            Arc::clone(&provider),
            Arc::clone(&merged_scip_handle),
        )
        .await;

    // The watched-folder singleton must be installed before the daemon starts
    // serving, but the ROUTE is now part of the daemon's declared capability
    // rather than something this call installs — so a failed subsystem yields
    // handlers that answer 503 with a named reason, not routes that 404.
    let _watched_subsystem = bootstrap::setup_watched_folders(
        Arc::clone(&engine),
        Arc::clone(&state_store),
        &data_dir,
        &config,
        folder_tiered_deps,
    )
    .await;

    // ── The daemon commissions the ONE Runtime ────────────────────────────
    //
    // `quality/TOPOLOGY.md` §3.5: "DAEMON — the only process that assembles a
    // Runtime". Until now `sovereign daemon run` held every ingredient — the
    // corpus engine, the state store, the routed inference provider — and no
    // thing that ANSWERS, so a turn could only be served by a host that had
    // built its own `Runtime` around its own copy of the recipe. That is what
    // made "the daemon serves the turn" impossible to state in the type.
    //
    // The recipe is `sovereign-runtime-recipe`, the same one `svrn chat` uses,
    // so the daemon cannot acquire a private dialect of the retrieval stack.
    // Two host inputs differ from the CLI's and both are decisions, not
    // defaults:
    //
    //   * shell WITHHELD — §10 "Decisions taken" 1. Shell execution does not
    //     move into a long-lived daemon running as a different user with a
    //     different cwd. Named in `tool_bundles` as a `Withheld` family, so it
    //     reads as a decision rather than an omission.
    //   * `mesh_knowledge` left at the recipe's `None`. §3.5 lists it among
    //     the five capabilities that leave the Runtime entirely: the client
    //     posts to `127.0.0.1:9741/v1/knowledge/search`, which INSIDE this
    //     process is a loopback call to itself.
    let common = sovereign_runtime_recipe::common_parts(
        sovereign_runtime_recipe::RecipeInputs {
            inference: Arc::clone(&routed_provider),
            store: Arc::clone(&state_store),
            conv_tiered: Some(Arc::clone(&state_store_concrete)
                as Arc<dyn sovereign_core::conv_tiered::ConvTieredReader>),
            corpus_engine: Arc::clone(&engine),
            note_store: Some(Arc::clone(&notes_store)),
            // No workspace skills on the daemon: skill activation is a surface
            // concern (a conversation is tagged by the surface that created
            // it) and the daemon serves every surface. Empty is the same
            // registry `svrn chat` passes.
            skills: Arc::new(sovereign_core::SkillRegistry::new()),
            // Approvals are out of scope for v1 of the turn protocol — the
            // same posture `sovereign-server` ships. `TurnRequest::Approve`
            // exists on the wire; routing it to a daemon-side session owner is
            // Phase 5's remaining work (hazard 12).
            approval: Arc::new(sovereign_core::executor::AutoApprovalChannel),
            inference_config: sovereign_core::types::InferenceConfig::default(),
            indexes_dir: data_dir.join("indexes"),
            // Derived ONCE, by the corpus-engine builder, and handed here —
            // see `build_corpus_engine`. The atlas embedding cache keys on it.
            embed_model: embed_model_id.clone(),
            // The families this daemon's turn registry carries. Shell is
            // named as WITHHELD rather than simply absent, so the decision is
            // a value a reader finds here (TOPOLOGY §10 "Decisions taken" 1;
            // ARCH §18.3).
            tool_bundles: {
                let mut b = sovereign_runtime_recipe::baseline_bundles(
                    sovereign_runtime_recipe::BaselineDeps {
                        store: &state_store,
                        inference: &routed_provider,
                        corpus_engine: &engine,
                        // The daemon opened this above; wiring it here is what
                        // gives `knowledge_lookup` its notes channel. It ran
                        // with that channel dark until 2026-08-26 while the
                        // desktop, which wired it by hand, did not.
                        note_store: Some(&notes_store),
                        web: sovereign_tools::bundles::WebReach::Granted(
                            sovereign_core::egress::search_client()
                                .expect("egress boundary search client build"),
                        ),
                        // No operator switch on a daemon, and escalating to the
                        // open web without one is a decision nobody made.
                        escalation: sovereign_tools::bundles::WebEscalation::Disabled,
                    },
                );
                b.push(Box::new(sovereign_tools::bundles::WikipediaTools::new(
                    Arc::clone(&engine),
                )));
                b.push(Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                    "shell",
                    "no shell in a long-lived daemon running as a different user \
                     with a different cwd (TOPOLOGY §10 decision 1)",
                )));
                b
            },
            // No settings panel on this host, so nothing to consult: every
            // family composed above registers.
            switches: sovereign_runtime_recipe::ToolSwitches::Ungoverned,
            // No config file of its own: the canonical `[[mcp_servers]]` array
            // is the whole declaration on this host.
            mcp_extra: Vec::new(),
            // A service must reach `listening` promptly. The meta-atlas is a
            // ~1 GB JSON parse (981 MB on the authoring host) and blocking
            // boot on it is a daemon that looks hung to `svrn daemon start`;
            // the desktop reached the same conclusion in 2026-06 and has
            // warmed it in the background ever since.
            warmth: sovereign_runtime_recipe::LaneWarmth::Deferred,
            // `build::inference::load_provider` above already installed a
            // rerank slot INSIDE the embedded engine from the same
            // `SOVEREIGN_RERANK_MODEL_PATH`. A standalone one here would put
            // the same GGUF in this process twice, and the VRAM pre-flight
            // would not catch it — it plans one rerank slot.
            rerank: sovereign_runtime_recipe::RerankWiring::AlreadyInProvider,
        },
        &sovereign_runtime_recipe::TracingProgress,
    )
    .await;
    let runtime = sovereign_runtime_recipe::commission(common.parts);
    tracing::info!(
        tools = runtime.tools.count(),
        "daemon: Runtime commissioned — this process can serve a turn"
    );

    // ── Commission, through THE assembler ─────────────────────────────
    //
    // This bootstrap no longer names its own variant. It hands its parts to
    // `sovereign_mesh::assemble`, the one exhaustive match over `Launch` that
    // constructs anything (`quality/TOPOLOGY.md` §10, Falsifier 3), and that
    // match decides what `sovereign daemon run` composes into. A refusal is
    // fatal and names both sides — a daemon that came up as the wrong shape is
    // the hazard, so there is nothing to degrade to (§18.3).
    let services = match sovereign_mesh::assemble(
        launch,
        sovereign_mesh::LaunchParts::Serving {
            serving: sovereign_mesh::ServingProfile {
                core: sovereign_mesh::ServingCore {
                    // The engine the auto_ingest loop and the
                    // /internal/corpus/* surface both read.
                    corpus_engine: Arc::clone(&engine),
                    inference_provider: Arc::clone(&routed_provider),
                    // Phase 3: the headless daemon's own `sovereign.db`,
                    // opened at the top of this function. `reading_http` now
                    // resolves conversation titles on this variant too.
                    state_store: Arc::clone(&state_store),
                    // Phase 5c: the thing that answers. Commissioned just
                    // above, from the one shared recipe.
                    runtime: Arc::clone(&runtime),
                },
                capability: sovereign_mesh::ServingCapability {
                    mcp: bootstrap::build_mcp_surface(tools, Arc::clone(&notes_store)),
                    project_http,
                    corpus_watch_http: sovereign_mesh::corpus_watch_http::corpus_watch_router(),
                },
                advertise_embed,
            },
            headless: Some(sovereign_mesh::HeadlessExtras {
                rails: sovereign_mesh::HeadlessRails {
                    // Rebuilds the provider when `models.*` changes on disk. Holds
                    // the same deferred handle, bound below.
                    provider_factory: Arc::new(provider::LlamaCppFactory {
                        daemon: Arc::clone(&deferred_daemon),
                    }),
                    // The work atlas writes into THIS store, so its entries reach
                    // gossip's `all_entries_for_gossip` enumeration. Without it the
                    // daemon builds a private in-memory store and atlas data is
                    // invisible across the mesh.
                    mesh_store: Arc::clone(&work_atlas_mesh_store),
                    convergence_recorder: Arc::clone(&convergence_recorder),
                },
                knowledge_view_http,
                solve_http: solve_http::solve_router(Arc::clone(&solve_jobs)),
            }),
        },
    ) {
        Ok(s) => s,
        Err(refusal) => {
            eprintln!("error: {refusal}");
            return 1;
        }
    };
    let daemon = sovereign_mesh::EmbeddedDaemon::new(data_dir.clone(), config.clone(), services);
    deferred_daemon.bind(Arc::clone(&daemon));

    // Host side of distributed-inference auto-warm. When this node distributes a
    // large primary across mesh workers, the embedded engine calls this seam to
    // seed each worker's shard BEFORE loading — so the load is all cache hits and
    // never streams a large weight share (the upload deadlock). This retires the
    // manual `SOVEREIGN_RPC_ASSUME_WARMED` for the common case. Installed
    // unconditionally (harmless on a node that never distributes) so both
    // auto-discovered and manual (`SOVEREIGN_RPC_WORKERS`) hosts auto-warm.
    sovereign_mesh::rpc_warm_http::install_rpc_warm_orchestrator(Arc::clone(&daemon));

    // Must be installed BEFORE discovery starts spawning the child: the
    // manifest is a boot-time snapshot taken while the slot is still unspawned,
    // so without this the node never advertises the model its child ends up
    // serving, and every request that names it 503s.
    bootstrap::spawn_self_manifest_refresh(
        Arc::clone(&mesh_provider),
        distributed_primary_slot.clone(),
    );

    bootstrap::spawn_rpc_worker_discovery(
        Arc::clone(&daemon),
        engine_handle,
        distributed_primary_slot,
    );

    bootstrap::spawn_slot_alias_push(Arc::clone(&daemon), mesh_provider);

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

    // The log is append-only across restarts by design, so this line is the
    // KEY every later line in this generation joins against: which binary,
    // built when, under which run id (sovereign_core::run_identity).
    let build = sovereign_core::run_identity::build();
    tracing::info!(
        client_port = config.daemon.client_port,
        internal_port = config.daemon.internal_port,
        run = sovereign_core::run_identity::run_id(),
        pid = build.pid,
        exe = %build.exe,
        exe_mtime = build.exe_mtime.as_deref().unwrap_or("unreadable"),
        version = env!("CARGO_PKG_VERSION"),
        "svrn daemon is running"
    );

    // ── Listener watchdog (DAEMON_RESILIENCE.md P0.5) ─────────────
    // Closes the phantom-Running hole (process alive, no client
    // listener) from OUTSIDE the deliberately best-effort bind path.
    // Exit-code contract: 104 (see `shutdown_daemon`).
    let _listener_watch_handle = {
        let bind = config.daemon.client_bind.clone();
        let port = config.daemon.client_port;
        crate::supervise::spawn_supervised("listener_watch", move || {
            crate::listener_watch::watch_loop(bind.clone(), port)
        })
    };

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
    // A user mesh-leave no longer ends the process — `POST /v1/mesh/leave`
    // re-creates a solo mesh in-process (`leave_to_solo`, rebinding :9741),
    // so the only things that reach here are deliberate signals and the
    // RSS-hard-limit self-SIGTERM.
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
    // Exit code contract: a NON-ZERO exit tells launchd
    // (`KeepAlive.SuccessfulExit=false`) / systemd (`Restart=on-failure`)
    // to relaunch the daemon. Only one path wants that relaunch now:
    //   102 — the memory watcher's RSS hard-limit self-exit.
    // (A user mesh-leave used to exit 103 for relaunch; it now re-solos
    // in-process, so nothing exits.) Every other shutdown is deliberate
    // (SIGINT/SIGTERM) and must stay 0 (= service manager leaves us down).
    let exit_code: i32 = if crate::memory_watch::hard_exit_requested() {
        eprintln!("svrn daemon exiting non-zero: RSS hard limit (service manager will relaunch)");
        102
    } else if crate::listener_watch::exit_requested() {
        eprintln!(
            "svrn daemon exiting non-zero: client listener lost (service manager will relaunch)"
        );
        104
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

/// Branded per-user data root (rebrand-aware path SSOT) — the daemon's
/// pidfile, workspace pointer, and logs all hang off it.
pub(crate) fn sovereign_root() -> std::path::PathBuf {
    sovereign_cli_shared::dirs::sovereign_root()
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
