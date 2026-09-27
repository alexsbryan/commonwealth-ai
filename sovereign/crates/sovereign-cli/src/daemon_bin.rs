// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-cli-daemon` sibling binary
//! (long-running host: daemon, setup, install-service, doctor).
//!
//! Discovery order:
//!   1. `$SOVEREIGN_CLI_DAEMON_BIN` if set
//!   2. Sibling of `current_exe()` named `sovereign-cli-daemon`
//!   3. PATH lookup of `sovereign-cli-daemon`

use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-cli-daemon";

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_CLI_DAEMON_BIN")
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "sovereign: cannot find sibling binary '{BIN_NAME}'. \
             Build it with `cargo build -p sovereign-cli-daemon --release`, \
             or set SOVEREIGN_CLI_DAEMON_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-cli-daemon");

    crate::sibling::exec_into(&bin, verb, args)
}
