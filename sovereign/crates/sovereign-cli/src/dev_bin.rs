// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-cli-dev` sibling binary
//! (workbench: project lifecycle + code intel + tools).
//!
//! When the user runs a verb that delegates into the workbench
//! (`notes`-family forwards, `audit`, `drift detect`, `status`,
//! `charter`, ...), the parent `sovereign` dispatcher locates its
//! sibling `sovereign-cli-dev` binary and execs into it. Same PID
//! on Unix (replaces this process), so stdout/stderr/stdin flow
//! straight through with no shell interposition.
//!
//! Binary discovery order:
//!   1. `$SOVEREIGN_CLI_DEV_BIN` if set
//!   2. Sibling of `current_exe()` named `sovereign-cli-dev`
//!   3. PATH lookup of `sovereign-cli-dev`
//!
//! Returns the child's exit code on platforms that can't replace
//! the process (non-Unix); Unix never returns from `exec`.

use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-cli-dev";

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_CLI_DEV_BIN")
}

fn not_found() -> i32 {
    eprintln!(
        "sovereign: cannot find sibling binary '{BIN_NAME}'. \
         Build it with `cargo build -p sovereign-cli-dev --release`, \
         or set SOVEREIGN_CLI_DEV_BIN to its path."
    );
    127
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        return not_found();
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-cli-dev");

    crate::sibling::exec_into(&bin, verb, args)
}

/// Run `sovereign-cli-dev <verb> <args>` as a child and wait for it, for a
/// step inside a verb that goes on afterwards (`project init`'s index step,
/// pb-code-index), where [`exec`] would replace this process.
#[cfg(feature = "code-intel")]
pub fn run(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        return not_found();
    };
    crate::sibling::warn_if_stale(&bin, "sovereign-cli-dev");
    tracing::debug!(bin = %bin.display(), verb, ?args, "dev_bin: run and wait");
    match std::process::Command::new(&bin)
        .arg(verb)
        .args(args)
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("sovereign: cannot run {}: {e}", bin.display());
            127
        }
    }
}
