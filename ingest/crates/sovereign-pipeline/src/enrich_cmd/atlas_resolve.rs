// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich resolve` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_resolve`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_resolve::…` keeps resolving for this crate's siblings.

use sovereign_cli_base::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_resolve::*;

const HELP: Help = Help {
    command: "svrn enrich atlas-resolve",
    summary: "Resolve atlas atoms + edges from Phase 1 sketches.",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-resolve <corpus-id> [--phase 3a]"),
        HelpSection::Flags(&[
            (
                "--phase 3a",
                "Entity + event atoms + Involves edges only; writes no claims and no \
                 typed records. Opt-in; the default runs the whole layer.",
            ),
            (
                "--phase all",
                "The default, spelled out: entities and events, then state / relation / \
                 claim / question atoms, the recipe's typed records (document stamps, \
                 RESOLVE, derived folds) and trajectories.json.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich atlas-resolve brothers_karamazov",
                "The whole layer — every atom type, the typed records, trajectories.json.",
            ),
            (
                "svrn enrich atlas-resolve bk --phase 3a",
                "Entities and events alone, from the cached sketches.",
            ),
        ]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich extract <corpus> --full` so the Phase 1 \
             cache exists. Produces `~/.svrnmesh/indexes/<corpus>/atlas/atoms.json`, \
             `edges.json`, and `trajectories.json`.",
        ),
    ],
};
pub async fn cmd_atlas_resolve(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    match run(&parsed).await {
        Ok(_) => 0,
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
