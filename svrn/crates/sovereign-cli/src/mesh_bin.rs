// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into the `sovereign-cli-mesh` sibling binary.
//!
//! When the user runs an LLM-touching verb (bench / chat / eval /
//! atlas / enrich / mesh / corpus / ...), the parent `sovereign`
//! dispatcher locates its sibling `sovereign-cli-mesh` binary and
//! execs into it. Same shape as `dev_bin::exec` — see that module
//! for the discovery + fallback rationale.

use std::path::PathBuf;

const BIN_NAME: &str = "sovereign-cli-mesh";

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_CLI_MESH_BIN")
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "sovereign: cannot find sibling binary '{BIN_NAME}'. \
             Build it with `cargo build -p sovereign-cli-mesh --release`, \
             or set SOVEREIGN_CLI_MESH_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-cli-mesh");

    crate::sibling::exec_into(&bin, verb, args)
}
