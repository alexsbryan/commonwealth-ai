// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-daemon` — long-running daemon host + setup + service
//! install + doctor. Parent `sovereign` shim execs into this binary
//! for `daemon`, `setup`, `install-service`, `doctor` verbs.

mod daemon_cmd;
mod doctor_cmd;
mod install_service_cmd;
mod memory_watch;
pub(crate) mod log_rotation;
mod service_install;
mod setup_cmd;
mod setup_config;
mod watcher_supervisor;

use sovereign_cli_shared::tracing_init::init_tracing;

fn main() {
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
    if std::env::var_os("RUST_MIN_STACK").is_none() {
        std::env::set_var("RUST_MIN_STACK", "8388608");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("sovereign-cli-daemon-rt")
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = raw_args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: &[String] = if raw_args.is_empty() {
        &[]
    } else {
        &raw_args[1..]
    };

    // The daemon needs structured tracing for launchd / systemd
    // operators tailing logs. Match the filter sovereign-cli used
    // pre-split.
    if cmd == "daemon" {
        init_tracing(
            "sovereign_cli_daemon=info,\
             sovereign_core=info,\
             sovereign_mesh=info,\
             sovereign_inference=info,\
             corpus_engine=info,\
             commonwealth_discovery=info,\
             commonwealth_api=info",
        );
    } else if cmd == "setup" {
        init_tracing("sovereign_cli_daemon=info");
    }

    let code: i32 = match cmd {
        "daemon" => daemon_cmd::run(rest).await,
        "setup" => setup_cmd::run_setup(rest).await,
        "install-service" => install_service_cmd::run(rest).await,
        "doctor" => doctor_cmd::run_doctor(rest).await,
        "" => {
            eprintln!("sovereign-cli-daemon: usage: sovereign-cli-daemon <subcommand> [args...]");
            2
        }
        other => {
            eprintln!("sovereign-cli-daemon: unknown subcommand '{other}'");
            2
        }
    };

    std::process::exit(code);
}
