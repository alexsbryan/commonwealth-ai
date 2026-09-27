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

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "sovereign: cannot find sibling binary '{BIN_NAME}'. \
             Build it with `cargo build -p sovereign-cli-dev --release`, \
             or set SOVEREIGN_CLI_DEV_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-cli-dev");

    crate::sibling::exec_into(&bin, verb, args)
}
