// SPDX-License-Identifier: AGPL-3.0-or-later
//! The assembled-host process's own core — `sovereign-daemon run`
//! (docs/FIVE_PROGRAMS.md §11 step 10), reached three ways:
//!
//! 1. `exec`'d by the `svrn` CLI's `daemon` verb shim, which passes the
//!    argv that followed the verb;
//! 2. directly by launchd/systemd unit files;
//! 3. re-exec'd by its own supervisors (`--compute-child`,
//!    `--rpc-worker`), handled in the bin before this module runs.
//!
//! Adapted from `sovereign-cli-daemon/src/daemon_cmd/` at the
//! `dm-daemon-assembled-bin` cut: the RUN path moved whole; the
//! out-of-process verb infrastructure (lifecycle verbs, pidfile
//! readers, the setup wizard) stayed with the CLI tree.
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

use std::sync::Arc;

use sovereign_contracts::launch::Launch;

mod boot;
mod help;
mod lifecycle;
// Twins of cli-daemon's modules, moved whole — see each file's header.
mod log_rotation;
mod memory_watch;
mod mesh_resume;
mod panic_hook;
mod rlimit;
mod vram_plan;

use boot::run_daemon;
use lifecycle::wait_for_shutdown;

/// Entry point for the bin's `Daemon`/`Worker` launches.
///
/// Dispatch order (mirrors the CLI tree's `daemon` verb):
/// - bare invocation falls through to `run`, which requires an existing
///   config (the first-boot wizard lives in `svrn setup`);
/// - `run [flags]` → the OS-service entry point;
/// - `--flag ...` → bare flags route to `run` so launchd unit files
///   can pass flags without the explicit `run` token;
/// - `vram-plan` → the sizing query.
pub async fn run(launch: &Launch, args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    match args.first().map(String::as_str) {
        Some("run") => run_daemon(launch, &args[1..]).await,
        // Sizing, not lifecycle: what VRAM would a loadout need, and which
        // card holds it. Lives under `daemon` because it answers the same
        // question the daemon's own boot preflight asks (`build/preflight`),
        // just ahead of the hardware existing.
        Some("vram-plan") => vram_plan::run(&args[1..]),
        // The lifecycle verbs are OUT-of-process infrastructure and stay
        // with the `svrn` CLI tree (cli-daemon keeps its own originals).
        // Named apart from the unknown-subcommand error so an operator
        // who reaches this bin directly learns where the verbs went.
        Some(other @ ("start" | "stop" | "restart" | "reload" | "status")) => {
            eprintln!(
                "error: `daemon {other}` is a CLI-side lifecycle verb — \
                 run it through `svrn`"
            );
            1
        }
        Some(flag) if flag.starts_with("--") => {
            // Bare flags like `--config <path>` route straight to
            // run_daemon — the caller means "start the daemon with
            // these flags."
            run_daemon(launch, args).await
        }
        Some(other) => {
            eprintln!("error: unknown daemon subcommand '{other}'");
            help::print(&HELP);
            1
        }
        None => {
            // Bare invocation — same destination as `run`. launchd
            // and systemd unit files keep using `daemon run`
            // explicitly; both paths land in the same place.
            run_daemon(launch, &[]).await
        }
    }
}

const HELP: help::Help = help::Help {
    command: "sovereign-daemon (svrn daemon run)",
    summary: "Long-running OICP server with managed inference + MCP tools.",
    sections: &[
        help::HelpSection::Usage(
            "sovereign-daemon [run] [--config <path>] [--rpc-worker[=<bind>]] | sovereign-daemon vram-plan …",
        ),
        help::HelpSection::Flags(&[
            ("--config <path>", "Override the default `~/.svrnmesh/config.toml` path."),
            ("--rpc-worker[=<bind>]", "Lend this node's GPU to the mesh: serve an llama.cpp RPC worker so peers can place layers here. Default bind 127.0.0.1:50052 — members reach the worker over the encrypted mesh tunnel, so no LAN bind is needed; a non-loopback bind is refused unless SOVEREIGN_RPC_ALLOW_PLAINTEXT_LAN=1 acknowledges it. This only OFFERS the GPU — unlike `[shared_model] role = \"anchor\"`, it does not also turn on peer discovery or enter the host election."),
        ]),
        help::HelpSection::Subcommands(&[
            ("(bare)",     "Run the daemon in the foreground. Requires an existing config (`svrn setup`). Equivalent to `run`."),
            ("run",       "Same as bare — kept for explicit invocation by launchd / systemd unit files."),
            ("vram-plan", "Size a slot loadout and name the smallest card that holds it."),
        ]),
        help::HelpSection::Notes(
            "The lifecycle verbs (start/stop/restart/reload/status) and the first-boot wizard live in the `svrn` CLI. Logs: ~/.svrnmesh/logs/daemon.log.",
        ),
    ],
};

/// Graceful shutdown choreography, extracted from `run_daemon` so its tail
/// reads as one named step. Blocks on SIGINT/SIGTERM, persists mesh state (NOT
/// `leave()` — that would force a fresh solo mesh on next boot), removes our
/// pidfile if it still points at us, and returns the process exit code: `102`
/// on the memory watcher's RSS-hard-limit path (so launchd/systemd relaunch),
/// `0` on every deliberate shutdown. On macOS it `_exit`s to skip the
/// ggml-metal destructor assertion (full rationale inline).
async fn shutdown_daemon(
    daemon: Arc<crate::EmbeddedDaemon>,
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
    // claimed the file, leave it alone — the CLI-side pidfile reader
    // (cli-daemon's `read_daemon_pid`) already handles a stale pidfile
    // via `kill(pid, 0)`.
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
    // across AppState, InferenceRouter, the inference adapter,
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
    let exit_code: i32 = if memory_watch::hard_exit_requested() {
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

/// Install the daemon panic hook. Pub wrapper over the twin module so
/// the bin — which links this crate as an external library and cannot
/// see `pub(crate)` — can install it before the tokio runtime exists,
/// exactly where the CLI tree's dispatcher did.
pub fn install_panic_hook(data_dir: std::path::PathBuf) {
    panic_hook::install(data_dir);
}

/// Branded per-user data root (rebrand-aware path SSOT). Twin of
/// `sovereign_cli_shared::dirs::sovereign_root`, which is itself a
/// pass-through to the SSOT named below — the daemon crate may not
/// take a cli-shared (svrn-package) edge, so it names the SSOT
/// directly.
pub(crate) fn sovereign_root() -> std::path::PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root()
}
