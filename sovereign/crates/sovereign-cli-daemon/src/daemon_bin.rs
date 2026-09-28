// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-stock` sibling binary: the stock
//! install, svrn with serve hosted in one process (pb-stock-binary;
//! phase-b-29 Q1). `SOVEREIGN_DAEMON_BIN` still names what `svrn daemon run`
//! execs, so a developer can point it at a bare `sovereign-daemon`, which
//! dials a configured serve.
//!
//! `svrn daemon run` ran the daemon LINKED in this crate until the
//! de-embed (docs/FIVE_PROGRAMS.md §11 step 10): the run body now lives
//! in `sovereign-daemon`'s own `[[bin]]`, and this crate keeps the verb
//! surface around it — lifecycle, the first-boot wizard gate, help —
//! exec'ing the sibling for the run itself. Same discovery + fallback
//! shape as `sovereign-cli`'s `agent_bench_bin::exec`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const BIN_NAME: &str = "sovereign-stock";

pub(crate) fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_DAEMON_BIN")
}

/// Sibling staleness warning — a local twin of `sovereign-cli`'s
/// `sibling::warn_if_stale` (sovereign-cli/src/sibling.rs), which this
/// crate cannot reach without depending on the dispatcher binary. The
/// footgun is the same one that module was minted for: edit the daemon
/// crate, rebuild only this binary, run the verb — the exec below
/// silently serves the STALE daemon. When the twin moves to a shared
/// leaf (`sovereign-cli-shared` is the natural home), this copy should
/// collapse into it rather than live beside it.
///
/// Warn-only, same posture: never blocks, never changes the exit code.
/// Mtime comparison, co-located artifacts only, muted by
/// `SOVEREIGN_NO_STALE_WARN=1`.
fn warn_if_stale(bin: &Path) {
    if std::env::var_os("SOVEREIGN_NO_STALE_WARN").is_some() {
        return;
    }
    let Ok(dispatcher) = std::env::current_exe().and_then(|p| std::fs::canonicalize(p)) else {
        return;
    };
    let Ok(sibling) = std::fs::canonicalize(bin) else {
        return;
    };
    if dispatcher.parent() != sibling.parent() {
        return;
    }
    let (Some(disp_mtime), Some(sib_mtime)) = (mtime(&dispatcher), mtime(&sibling)) else {
        return;
    };
    // Same slack as the sovereign-cli twin: workspace builds finish
    // crates seconds apart, so only a sibling that predates this
    // binary by more than this is worth a warning.
    if sib_mtime + std::time::Duration::from_secs(2) < disp_mtime {
        let lag = disp_mtime.duration_since(sib_mtime).unwrap_or_default();
        eprintln!(
            "warning: {BIN_NAME} binary is {} older than the binary exec'ing it — \
             if you changed the daemon or serve crate, rebuild: cargo build -p sovereign-stock  \
             (silence: SOVEREIGN_NO_STALE_WARN=1)",
            human_duration(lag)
        );
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn human_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
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

    warn_if_stale(&bin);
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
