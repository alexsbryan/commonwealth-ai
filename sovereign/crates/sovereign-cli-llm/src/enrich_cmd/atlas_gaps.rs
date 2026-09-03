// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich gaps` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_gaps`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_gaps::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_gaps::*;

const HELP: Help = Help {
    command: "svrn enrich atlas-gaps",
    summary: "Detect structural gaps in the resolved atlas (deterministic).",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-gaps <corpus-id>"),
        HelpSection::Examples(&[(
            "svrn enrich atlas-gaps brothers_karamazov",
            "Scan atoms + edges, detect transitions without triggers / ungrounded claims \
             / open questions, write gaps.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich atlas-resolve <corpus> --phase all` so the \
             atlas directory exists. Produces \
             `~/.svrnmesh/indexes/<corpus>/atlas/gaps.json` as a flat list of Gap records \
             with `kind`, `description`, `referenced_atoms`, `evidence`, and `significance`.",
        ),
    ],
};
pub async fn cmd_atlas_gaps(args: &[String]) -> i32 {
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

    match run(&parsed) {
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
