// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich resolve` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_resolve`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_resolve::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_resolve::*;

const HELP: Help = Help {
    command: "svrn enrich atlas-resolve",
    summary: "Resolve atlas atoms + edges from Phase 1 sketches.",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-resolve <corpus-id> [--phase 3a|3b|all]"),
        HelpSection::Flags(&[
            (
                "--phase 3a",
                "Entity + event atoms + Involves edges only. Fast; no LLM calls. \
                 Default when --phase is omitted.",
            ),
            (
                "--phase 3b",
                "Adds state / relation / claim / question atoms + Transition + Grounds \
                 edges + populates trajectories.json. Implies 3a (entities + events \
                 are re-resolved so atom ids stay consistent).",
            ),
            (
                "--phase all",
                "Synonym for --phase 3b — runs the full structural pass. Phase 5 \
                 LLM-enriched grounding is a separate subcommand that will land in a \
                 later step.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich atlas-resolve brothers_karamazov",
                "Default (Phase 3a) — resolve entities + events from the cached sketches.",
            ),
            (
                "svrn enrich atlas-resolve bk --phase all",
                "Full structural pass — every atom type + trajectories.json populated.",
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
