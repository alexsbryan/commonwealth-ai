// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-daemon` — the assembled-host process (docs/FIVE_PROGRAMS.md
//! §11 step 10): the svrn daemon binary's own main, holding what used to be
//! the `daemon` verb's half of `sovereign-cli-daemon`'s dispatcher.
//!
//! # argv contract
//!
//! `argv[1..]` is whatever followed the `daemon` verb in the old
//! dispatcher — the `svrn` CLI's shim `exec`s this bin with exactly
//! those args. Direct invocations (launchd/systemd units, a dev shell)
//! pass the same shape: `[run] [--config <path>] [--rpc-worker[=<bind>]]
//! …`. The verb is reconstructed below and handed to the ONE parser
//! (`Launch::parse`), so the decision this process makes about itself is
//! the same one the old dispatcher made:
//!
//! - `sovereign-daemon run …` → `Launch::Daemon` → `daemon_cmd::run`
//! - `sovereign-daemon run --worker-mode …` → refused: worker mode is the
//!   `sovereign-pod-worker` binary (pb-pods-worker)
//! - `sovereign-daemon join --config <p> --node-name <n>` (invite on
//!   stdin) → `Launch::AdminJoin` → `daemon_cmd::admin_join::run`
//! - a `current_exe()` re-exec carrying `--compute-child` / `--rpc-worker`
//!   (the daemon's own supervisors spawn those with THIS binary's path)
//!   → the child mains, before any daemon bootstrap runs.
//!
//! Everything below the argv reconstruction is the daemon-verb slice of
//! `sovereign-cli-daemon/src/lib.rs::run_with_args`, mirrored line for
//! line; the twin is named so the two can be collapsed when the CLI
//! tree's copy retires.

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sovereign_daemon::process::run(&raw_args, None));
}
