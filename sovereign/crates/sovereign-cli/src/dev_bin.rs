// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-cli-dev` sibling binary
//! (workbench: ATOS workflow + project lifecycle + code intel +
//! tools).
//!
//! When the user runs `sovereign atos ...` (or any verb that
//! delegates into the workbench — `notes promote`, `audit teardown`,
//! `drift accept`, `status`, `charter`, ...), the parent `sovereign`
//! dispatcher locates its sibling `sovereign-cli-dev` binary and
//! execs into it. Same PID on Unix (replaces this process), so
//! stdout/stderr/stdin flow straight through with no shell
//! interposition.
//!
//! Binary discovery order:
//!   1. `$SOVEREIGN_CLI_DEV_BIN` if set
//!   2. Sibling of `current_exe()` named `sovereign-cli-dev`
//!   3. PATH lookup of `sovereign-cli-dev`
//!
//! Returns the child's exit code on platforms that can't replace
//! the process (non-Unix); Unix never returns from `exec`.

use std::ffi::OsString;
use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-cli-dev";

fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SOVEREIGN_CLI_DEV_BIN") {
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

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "sovereign: cannot find sibling binary '{BIN_NAME}'. \
             Build it with `cargo build -p sovereign-cli-dev --release`, \
             or set SOVEREIGN_CLI_DEV_BIN to its path."
        );
        return 127;
    };

    let mut argv: Vec<OsString> = Vec::with_capacity(args.len() + 1);
    argv.push(OsString::from(verb));
    for a in args {
        argv.push(OsString::from(a));
    }

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
