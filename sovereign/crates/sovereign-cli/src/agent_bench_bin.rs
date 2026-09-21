// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-agent-bench` sibling binary.
//!
//! `svrn agent-bench` ran LINKED until 2026-09-21, which put a `bench`-package
//! crate inside the `svrn` dispatcher. The crate already ships its own
//! `[[bin]]` whose `main` is `run_agent_bench(&args[1..])` — the same call the
//! dispatcher was making — so exec'ing it costs nothing and drops the edge.
//! Same discovery + fallback shape as `dev_bin::exec`.

use std::ffi::OsString;
use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-agent-bench";

fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SOVEREIGN_AGENT_BENCH_BIN") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(real) = std::fs::canonicalize(&exe) {
            if let Some(dir) = real.parent() {
                let cand = dir.join(BIN_NAME);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    which::which(BIN_NAME).ok()
}

/// `args` is everything AFTER the `agent-bench` verb — the sibling's `main`
/// skips its own argv[0] and passes the rest straight to `run_agent_bench`.
pub fn exec(args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "sovereign: cannot find sibling binary '{BIN_NAME}'. \
             Build it with `cargo build -p sovereign-agent-bench`, \
             or set SOVEREIGN_AGENT_BENCH_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, BIN_NAME);

    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&bin).args(&argv).exec();
        eprintln!("sovereign: exec {} failed: {err}", bin.display());
        126
    }

    #[cfg(not(unix))]
    {
        match std::process::Command::new(&bin).args(&argv).status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("sovereign: spawn {} failed: {e}", bin.display());
                126
            }
        }
    }
}
