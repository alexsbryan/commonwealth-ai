// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binary shim over the `sovereign_cli_daemon` library. The whole
//! entry — env defaults, rebrand migration, panic hook, runtime,
//! dispatch — lives in `lib.rs::run_with_args` so the desktop's
//! `--daemon-child` mode runs the IDENTICAL daemon (DAEMON_RESILIENCE.md
//! P0.1). Keep this file logic-free.

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sovereign_cli_daemon::run_with_args(raw_args));
}
