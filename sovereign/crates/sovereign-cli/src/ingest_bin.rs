// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exec dispatch into ingest's own binary, `svrn-ingest` (phase-b
//! pb-ingest-cli), for `svrn ingest <recipe.toml>`. Same shape as
//! `serve_bin::exec`; the verb is passed as the sibling's first argument, as
//! `llm_bin::exec` does, so `svrn-ingest` routes it with its own subcommands.

use std::path::PathBuf;

const BIN_NAME: &str = "svrn-ingest";

fn locate() -> Option<PathBuf> {
    sovereign_turn_client::reach::locate_sibling(BIN_NAME, "SOVEREIGN_INGEST_BIN")
}

pub fn exec(verb: &str, args: &[String]) -> i32 {
    let Some(bin) = locate() else {
        eprintln!(
            "svrn {verb}: owned by ingest, whose binary '{BIN_NAME}' was not \
             found. Build it with `cargo build -p sovereign-pipeline`, or set \
             SOVEREIGN_INGEST_BIN to its path."
        );
        return 127;
    };

    crate::sibling::warn_if_stale(&bin, "sovereign-pipeline");

    crate::sibling::exec_into(&bin, verb, args)
}
