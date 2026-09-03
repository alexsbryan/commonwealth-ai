// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich tensions` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_tensions`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_tensions::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_tensions::*;

const HELP: Help = Help {
    command: "svrn enrich atlas-tensions",
    summary: "Select tension candidates from the resolved atlas (deterministic).",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-tensions <corpus-id>"),
        HelpSection::Examples(&[(
            "svrn enrich atlas-tensions brothers_karamazov",
            "Scan atoms.json, enumerate entity-overlap candidate pairs, write tension_candidates.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich atlas-resolve <corpus> --phase all` so the \
             atlas directory exists. Produces \
             `~/.svrnmesh/indexes/<corpus>/atlas/tension_candidates.json`. Does NOT call \
             the LLM — the classifier that promotes candidates to real Tension edges lands \
             in a later step.",
        ),
    ],
};
pub async fn cmd_atlas_tensions(args: &[String]) -> i32 {
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
        Ok(report) => {
            render(&report);
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
