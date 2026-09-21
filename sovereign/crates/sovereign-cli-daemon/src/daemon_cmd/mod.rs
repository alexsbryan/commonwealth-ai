// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon` — the verb surface around the daemon process.
//!
//! Until the de-embed (docs/FIVE_PROGRAMS.md §11 step 10) this module
//! also RAN the daemon in-process: config load, provider, engine,
//! stores, commissioning, the run loop. That body forked to the
//! `sovereign-daemon` crate's own `[[bin]]`, and what stays here is
//! everything that talks ABOUT the daemon rather than BEING it:
//!
//! 1. argv dispatch: lifecycle verbs (start/stop/restart/reload/
//!    status), `vram-plan`, help — plus the `run` path, which keeps
//!    the CLI-side first-boot wizard gate and then execs the
//!    `sovereign-daemon` sibling with the args that followed the
//!    `daemon` verb (see `crate::daemon_bin`).
//! 2. The wizard gate: a bare `svrn daemon` on a machine with no
//!    config inlines the interactive setup (`run_setup_only`) before
//!    the exec, because the wizard lives in this crate's `setup_cmd`
//!    and the sibling cannot run it. `--setup-only` returns without
//!    exec'ing at all.
//! 3. The pidfile + `sovereign_root` accessors the other verbs share
//!    (`read_daemon_pid` is consumed by `install-service`'s
//!    double-start guard and `setup --fim`'s restart ladder).

use std::io::IsTerminal as _;

use sovereign_contracts::launch::Launch;
use sovereign_core::setup_config::SetupConfig;

mod vram_plan;
// `pub(crate)` so `setup_cmd::fim` can reach `restart_daemon` directly.
// `svrn setup --fim` rewrites the model config and must bounce the
// daemon itself — telling the operator to go run `svrn daemon restart`
// mid-flow would break the one-command promise and leave the verify
// ladder below with nothing to verify.
pub(crate) mod lifecycle;
// Liveness probe for the pidfile-managed (manual) daemon — consumed by
// `install-service`'s double-start guard and doctor's supervision check.
pub(crate) use lifecycle::read_daemon_pid;

use lifecycle::{reload_daemon, restart_daemon, start_daemon, status_daemon, stop_daemon};
// Kept for `lifecycle::start_daemon`, which forwards the `--rpc-worker`
// flag to the daemon it spawns via `bootstrap::rpc_worker_flag` — the
// flag parser lives with the daemon crate that also consumes the flag.
use sovereign_daemon::bootstrap;

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
        Some("run") => run_daemon(launch, args).await,
        Some("start") => start_daemon(&args[1..]).await,
        Some("stop") => stop_daemon().await,
        Some("restart") => restart_daemon(&args[1..]).await,
        Some("reload") => reload_daemon().await,
        Some("status") => status_daemon().await,
        // Sizing, not lifecycle: what VRAM would a loadout need, and which
        // card holds it. Lives under `daemon` because it answers the same
        // question the daemon's own boot preflight asks (`build/preflight`),
        // just ahead of the hardware existing.
        Some("vram-plan") => vram_plan::run(&args[1..]),
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
            run_daemon(launch, args).await
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
            ("--rpc-worker[=<bind>]", "Lend this node's GPU to the mesh: serve an llama.cpp RPC worker so peers can place layers here. Default bind 127.0.0.1:50052 — members reach the worker over the encrypted mesh tunnel, so no LAN bind is needed; a non-loopback bind is refused unless SOVEREIGN_RPC_ALLOW_PLAINTEXT_LAN=1 acknowledges it. Works on `run`, `start` and `restart`. This only OFFERS the GPU — unlike `[shared_model] role = \"anchor\"`, it does not also turn on peer discovery or enter the host election."),
        ]),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("(bare)",  "Run the daemon in the foreground. On first boot inlines the setup wizard; subsequent runs just load config and start. Equivalent to `daemon run`."),
            ("run",     "Same as bare — kept for explicit invocation by launchd / systemd unit files. Execs the `sovereign-daemon` sibling binary with everything after the `daemon` verb."),
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

/// The `run` path: the CLI-side pre-flight, then the exec.
///
/// `args` is the FULL slice that followed the `daemon` verb — it may
/// carry a leading `run` token (`daemon run …`), arrive as bare flags
/// (`daemon --setup-only`), or be empty (bare `daemon`). The exec hands
/// it to the sibling verbatim; only the wizard gate below peels the
/// `run` token off, because it answers with this crate's own flags.
async fn run_daemon(launch: &Launch, args: &[String]) -> i32 {
    // ── Worker-mode branch (ephemeral pod) ────────────────────────
    //
    // `svrn daemon run --worker-mode` runs an ephemeral worker daemon
    // (see `sovereign/docs/EPHEMERAL_WORKER_PODS.md`) — a different
    // server on a different socket with none of the persistent-peer
    // surface. The whole run body, this branch included, forked to the
    // `sovereign-daemon` sibling, so the honest move is to pass the
    // args through untouched.
    //
    // THE LAUNCH ANSWERS THIS, not a second argv scan, and the branch
    // sits BEFORE the first-boot gate on purpose (the original body
    // ordered it this way): a pod-spawned worker carries `--config`
    // but no canonical config and no TTY, so the wizard gate below
    // would refuse a worker that the old code ran fine.
    if matches!(launch, Launch::Worker { .. }) {
        return crate::daemon_bin::exec(args);
    }

    // ── Phase 4 flag parsing ──────────────────────────────────────
    //
    // `--setup-only` runs the wizard and exits without binding the
    // listener. Useful for users who want to configure the host now
    // and start the daemon manually later. Other flags pass through
    // to the daemon-start path; unrecognised flags are tolerated for
    // forward-compatibility (the daemon doesn't accept tunables on
    // the command line, only via the config file).
    let flag_args: &[String] = if args.first().map(String::as_str) == Some("run") {
        &args[1..]
    } else {
        args
    };
    let setup_only = flag_args.iter().any(|a| a == "--setup-only");

    // `--config <path>` overrides the default `~/.svrnmesh/config.toml`
    // path. Phase 2 of EPHEMERAL_WORKER_PODS uses this to point the
    // child daemon spawned by `SubprocessRunner` at the auto-generated
    // pod-side config (written by `worker_http::write_child_daemon_config`).
    // Production launchd/systemd units don't pass `--config`; they
    // continue to use the canonical path. The wizard short-circuit
    // below still checks `exists()` at the canonical path even when
    // `--config` is set — that's intentional: if the operator passes
    // `--config` they're telling us they have a config, so we skip
    // the wizard entirely and surface a clean error if the file is
    // missing.
    let config_override: Option<std::path::PathBuf> = {
        let mut path: Option<std::path::PathBuf> = None;
        let mut it = flag_args.iter();
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
    //
    // The wizard runs HERE, before the exec, because it lives in this
    // crate's `setup_cmd` — the sibling cannot run it. After a
    // successful wizard the config exists, so the sibling's own gate
    // (its fork kept one) falls straight through to boot.
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
        let wizard_args: Vec<String> = flag_args
            .iter()
            .filter(|a| a.as_str() != "--setup-only")
            .cloned()
            .collect();
        let code = run_setup_only(&wizard_args).await;
        if code != 0 {
            return code;
        }
        // After a successful wizard the config file exists; exec below.
    }

    if setup_only {
        // Wizard already ran above (or config existed and the wizard
        // was a no-op). Either way, return without exec'ing the daemon.
        return 0;
    }

    // ── The run itself: exec the sibling ──────────────────────────
    //
    // Everything below this line used to be the daemon bootstrap —
    // log rotation, memory watch, provider, engine, stores, Runtime
    // commissioning, mesh resume, the shutdown choreography — and all
    // of it forked to the `sovereign-daemon` binary at the de-embed
    // (docs/FIVE_PROGRAMS.md §11 step 10). `exec(2)` replaces this
    // process image, so the pid, the pidfile semantics, the service
    // manager's child, and the desktop's supervised `--daemon-child`
    // all keep pointing at the process that now runs the daemon.
    crate::daemon_bin::exec(args)
}

/// Branded per-user data root (rebrand-aware path SSOT) — the daemon's
/// pidfile, workspace pointer, and logs all hang off it.
pub(crate) fn sovereign_root() -> std::path::PathBuf {
    sovereign_cli_shared::dirs::sovereign_root()
}
