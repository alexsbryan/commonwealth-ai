// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-stock` sibling binary: the stock
//! install, svrn with serve hosted in one process (pb-stock-binary;
//! phase-b-29 Q1). `SOVEREIGN_DAEMON_BIN` still names what `svrn daemon run`
//! execs, so a developer can point it at a bare `sovereign-daemon`, which
//! dials a configured serve.
//!
//! `svrn daemon run` ran the daemon LINKED in this crate until the
//! de-embed (docs/internal/FIVE_PROGRAMS.md §11 step 10): the run body now lives
//! in `sovereign-daemon`'s own `[[bin]]`, and this crate keeps the verb
//! surface around it — lifecycle, the first-boot wizard gate, help —
//! exec'ing the sibling for the run itself. Same discovery + fallback
//! shape as `sovereign-cli`'s `agent_bench_bin::exec`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const BIN_NAME: &str = "sovereign-stock";

pub(crate) fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_DAEMON_BIN")
}

/// `args` is everything AFTER the `daemon` verb — the sibling's `main`
/// re-dispatches them exactly as this crate's `daemon_cmd::run` would
/// have (`run` token included; bare-flag and bare invocations arrive
/// without it), running the forked daemon body in this process's place.
pub(crate) fn exec(args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "sovereign: cannot find sibling binary '{BIN_NAME}'. \
             Build it with `cargo build -p sovereign-stock`, \
             or set SOVEREIGN_DAEMON_BIN to its path."
        );
        return 127;
    };

    host_kit::sibling::warn_if_stale(&bin, BIN_NAME);
    exec_bin(&bin, args)
}

/// Worker mode is its own binary in sovereign-pods (pb-pods-worker): `svrn
/// daemon run --worker-mode` execs it with the argv unchanged, so the pod
/// contract (`exec sovereign-cli daemon run --worker-mode`) holds.
pub(crate) fn exec_pod_worker(args: &[String]) -> i32 {
    const POD_WORKER: &str = "sovereign-pod-worker";
    let Some(bin) =
        sovereign_turn_client::reach::locate_sibling(POD_WORKER, "SOVEREIGN_POD_WORKER_BIN")
    else {
        eprintln!(
            "sovereign: cannot find sibling binary '{POD_WORKER}'. \
             Build it with `cargo build -p sovereign-pods`, \
             or set SOVEREIGN_POD_WORKER_BIN to its path."
        );
        return 127;
    };
    exec_bin(&bin, args)
}

/// A verb serve's binary owns (`sovereign_serve::WEIGHT_VERBS`), exec'd with
/// its argv: `svrn daemon vram-plan` sizes a loadout in serve, whose
/// placement it is (pb-distribution-setup).
pub(crate) fn exec_serve(verb: &str, args: &[String]) -> i32 {
    const SERVE: &str = "sovereign-serve";
    let Some(bin) = sovereign_turn_client::reach::locate_sibling(SERVE, "SOVEREIGN_SERVE_BIN")
    else {
        eprintln!(
            "svrn daemon {verb}: owned by serve, whose binary '{SERVE}' was not found. \
             Build it with `cargo build -p sovereign-serve`, \
             or set SOVEREIGN_SERVE_BIN to its path."
        );
        return 127;
    };
    let mut argv = vec![verb.to_string()];
    argv.extend(args.iter().cloned());
    exec_bin(&bin, &argv)
}

fn exec_bin(bin: &Path, args: &[String]) -> i32 {
    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(bin).args(&argv).exec();
        eprintln!("sovereign: exec {} failed: {err}", bin.display());
        126
    }

    #[cfg(not(unix))]
    {
        match std::process::Command::new(bin).args(&argv).status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("sovereign: spawn {} failed: {e}", bin.display());
                126
            }
        }
    }
}
