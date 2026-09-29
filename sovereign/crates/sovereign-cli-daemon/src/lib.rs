// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-daemon` — long-running daemon host + setup + service
//! install + doctor, as BOTH a binary and a library.
//!
//! The library face exists for the desktop's supervised child-process
//! mode (DAEMON_RESILIENCE.md P0.1): the desktop binary detects a
//! `--daemon-child` argv and calls [`daemon_child_main`], so ONE daemon
//! serves the CLI binary and the desktop child alike — no ~241 MB
//! sidecar duplicated into the installer. Since the de-embed
//! (docs/FIVE_PROGRAMS.md §11 step 10) the daemon body itself lives in
//! the `sovereign-daemon` binary; the child arm execs it, keeping the
//! same pid (and so the same supervisor handle) while inheriting every
//! daemon defense from the sibling.

mod daemon_bin;
mod daemon_cmd;
mod doctor_cmd;
mod install_service_cmd;
mod memory_watch;
mod model_cmd;
mod panic_hook;
mod setup_cmd;
mod setup_config;

use sovereign_cli_shared::tracing_init::init_tracing;
use sovereign_contracts::launch::Launch;

/// Process-level entry shared by the `sovereign-cli-daemon` binary and
/// the desktop `--daemon-child` arm. Sets the diagnostic env defaults,
/// runs the rebrand migration, installs the daemon panic hook (daemon
/// verb only), builds the 8 MiB-stack tokio runtime, and dispatches.
/// Returns the process exit code — the caller exits.
pub fn run_with_args(raw_args: Vec<String>) -> i32 {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }

    // ONE decision about what this process becomes (ARCH §2.1, §10.6). Before
    // this, `run_with_args` re-derived it three times — `first() ==
    // Some("--compute-child")` here, `first() == Some("daemon")` for the panic
    // hook, and `match cmd` in `dispatch` — and the desktop's `main.rs` kept a
    // FOURTH list that disagreed with this one on ordering.
    let launch = Launch::parse(&raw_args, Launch::Bare);

    // `--compute-child` and `--rpc-worker` are serve's (`child_launch`), not
    // this crate's: both are `current_exe()` re-execs of the loading process,
    // and `daemon run` here execs the stock binary, which routes them first
    // (five-programs fp-10/fp-25, pb-serve-distributes).
    // An `--rpc-worker` argv that reaches this binary anyway (a hand-typed
    // `svrn daemon --rpc-worker`) is handed to the owner unchanged, before the
    // migration and runtime below, so it lands where it always did.
    if let Launch::RpcWorker { .. } = &launch {
        return daemon_bin::exec(&raw_args);
    }
    // `daemon run --worker-mode` is the `sovereign-pod-worker` binary's
    // (pb-pods-worker): exec'd with the argv unchanged, before the rebrand
    // migration (an ephemeral pod has no legacy dir) and the panic hook (the
    // worker installs its own).
    if let Launch::Worker { .. } = &launch {
        return daemon_bin::exec_pod_worker(&raw_args);
    }

    // Rebrand back-compat (see sovereign_core::rebrand): idempotent, non-destructive.
    // The daemon is the migration authority — it runs before binding the API port.
    sovereign_core::rebrand::promote_legacy_env();
    sovereign_core::rebrand::run_startup_migration();

    // Panic hook for the long-running daemon verb only (setup/doctor/
    // install-service are interactive CLI runs where the std default
    // suffices). Installed after the rebrand migration so the crash dir
    // lands in the post-migration data dir; before the runtime so even a
    // runtime-build panic leaves a record. (DAEMON_RESILIENCE.md P0.4 —
    // without this, a tokio worker-task panic was swallowed with no log
    // line and no artifact.)
    // `is_resident()` is the one implementation of "binds a long-lived
    // listener": it covers `daemon run` and `daemon run --worker-mode`, which
    // is exactly what the `first() == Some("daemon")` test covered.
    //
    // It is NOT what the run lock keys on, and an earlier version of this
    // comment said it was. Residency and data-root ownership are different
    // questions: `Launch::Worker` is resident and owns no persistent state at
    // all, so it has nothing to lock. The lock is keyed on the data root by
    // whoever is about to write it (`host_kit::RunLock`).
    if launch.is_resident() {
        let data_dir = sovereign_contracts::rebrand::svrnmesh_root();
        panic_hook::install(data_dir);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-daemon-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(dispatch(launch, &raw_args))
}

/// The desktop `--daemon-child` entry: exactly `daemon run`, nothing
/// else reachable. The desktop binary calls this BEFORE any Tauri
/// initialization, so the child is a plain headless daemon process
/// (DAEMON_RESILIENCE.md P0.1).
pub fn daemon_child_main() -> i32 {
    run_with_args(vec!["daemon".into(), "run".into()])
}

/// Dispatch on what this process became. Exhaustive over [`Launch`] on
/// purpose: adding a launch mode must not compile until this binary has
/// decided what it means here, including deciding it is unreachable.
///
/// `raw_args` is carried only for the `Bare` diagnostic — `Launch` answers
/// "what is this process", and "no launch matched" is one answer whether the
/// argv was empty or held a word this binary does not know. The two still
/// deserve different messages.
async fn dispatch(launch: Launch, raw_args: &[String]) -> i32 {
    // The daemon needs structured tracing for launchd / systemd
    // operators tailing logs. Match the filter sovereign-cli used
    // pre-split.
    match &launch {
        Launch::Daemon { .. } => {
            // Track W: `SOVEREIGN_IROH_LOG` cranks iroh/relay/transport internals to
            // debug for diagnosing a reachability wedge; off, they stay at warn
            // (errors still visible) so the log isn't flooded.
            let iroh_debug = std::env::var_os("SOVEREIGN_IROH_LOG").is_some();
            init_tracing(&sovereign_daemon::process::daemon_tracing_filter(
                iroh_debug,
                sovereign_daemon::process::llama_debug_requested(),
            ));
        }
        Launch::Verb { name, .. } if name == "setup" => {
            init_tracing("sovereign_cli_daemon=info");
        }
        _ => {}
    }

    match launch {
        Launch::Daemon { ref args } => daemon_cmd::run(&launch, &args.clone()).await,
        Launch::Verb { name, args } => {
            match name.as_str() {
                "model" => model_cmd::run(&args).await,
                "setup" => setup_cmd::run_setup(&args).await,
                "install-service" => install_service_cmd::run(&args).await,
                "doctor" => doctor_cmd::run_doctor(&args).await,
                // `ONE_SHOT_VERBS` is the closed set `Launch::parse` matches on;
                // a name reaching here means that list and this one disagree.
                other => {
                    eprintln!("sovereign-cli-daemon: verb '{other}' is declared in Launch but not wired here");
                    2
                }
            }
        }
        Launch::Bare => match raw_args.first() {
            None => {
                eprintln!(
                    "sovereign-cli-daemon: usage: sovereign-cli-daemon <subcommand> [args...]"
                );
                2
            }
            Some(other) => {
                eprintln!("sovereign-cli-daemon: unknown subcommand '{other}'");
                2
            }
        },
        Launch::RpcWorker { .. } => unreachable!("rpc-worker is exec'd in run_with_args"),
        Launch::Worker { .. } => unreachable!("worker mode is exec'd in run_with_args"),
        // Other binaries' launches, incl. the compute-child serve's `child_launch` owns.
        // Named explicitly so that adding a variant forces a decision here instead of a `_` arm.
        Launch::ComputeChild { .. }
        | Launch::AdminJoin { .. }
        | Launch::Desktop
        | Launch::Smoketest { .. } => {
            eprintln!(
                "sovereign-cli-daemon: {} is not a launch this binary serves",
                launch.as_str()
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use sovereign_contracts::launch::Launch;

    /// The smoketest token has ONE owner — `sovereign_inference::smoketest::
    /// SMOKETEST_FLAG`, next to the implementation. `sovereign-contracts` sits
    /// below `sovereign-inference` and cannot name it, so `Launch::parse`
    /// matches the literal. This test is the seam: it feeds the OWNER's
    /// constant into the parser and fails if either side drifts. Lives here
    /// because this is a crate that can see both.
    #[test]
    fn launch_smoketest_flag_matches_owner() {
        let argv = vec![sovereign_inference::smoketest::SMOKETEST_FLAG.to_string()];
        assert_eq!(
            Launch::parse(&argv, Launch::Bare).as_str(),
            "smoketest",
            "sovereign-inference renamed SMOKETEST_FLAG without updating Launch::parse"
        );
    }

    /// The three tokens `launch.rs` owns must be what spawn sites send. Pins
    /// the round trip a bare literal at a spawn site would silently break.
    #[test]
    fn every_owned_launch_flag_round_trips_through_parse() {
        use sovereign_contracts::launch::{
            COMPUTE_CHILD_FLAG, DAEMON_CHILD_FLAG, WORKER_MODE_FLAG,
        };
        let one = |a: &str| Launch::parse(&[a.to_string()], Launch::Bare);
        assert_eq!(one(DAEMON_CHILD_FLAG).as_str(), "daemon");
        assert_eq!(one(COMPUTE_CHILD_FLAG).as_str(), "compute-child");
        // Worker is reached only via the `daemon` verb, never bare.
        let worker = Launch::parse(
            &["daemon".into(), "run".into(), WORKER_MODE_FLAG.to_string()],
            Launch::Bare,
        );
        assert_eq!(worker.as_str(), "worker");
        assert!(worker.is_resident());
    }
}
