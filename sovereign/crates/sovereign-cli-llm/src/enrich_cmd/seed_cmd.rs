// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich seed` — the CLI surface for
//! [`sovereign_enrichment_build::seed_cmd`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::seed_cmd::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::seed_cmd::*;

const HELP: Help = Help {
    command: "svrn enrich seed",
    summary: "Stage 1a: extract the seed entity list from the first section.",
    sections: &[
        HelpSection::Usage("svrn enrich seed <corpus-id> [--force]"),
        HelpSection::Flags(&[(
            "--force",
            "Recompute even when a seed list is already cached. Useful when the opening \
             section has been edited or the pipeline's seed prompt has changed.",
        )]),
        HelpSection::Examples(&[
            (
                "svrn enrich seed brothers_karamazov",
                "Read chapter 1, emit canonical entity list, cache to cache/seed.json.",
            ),
            (
                "svrn enrich seed bk --force",
                "Re-run even if the seed cache is warm.",
            ),
        ]),
        HelpSection::Notes(
            "Every subsequent `svrn enrich extract` call reads the cached seed and \
             threads the canonical-names block into every per-chapter Phase 1 prompt. \
             This is what keeps `Fyodor Pavlovich Karamazov` from fragmenting into \
             `Fyodor Karam`, `Fyo Karamzov`, and similar variants across chapters.",
        ),
    ],
};
pub async fn cmd_seed(args: &[String]) -> i32 {
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
        Err(e) => {
            eprintln!("error: {}", e.message());
            e.exit_code()
        }
    }
}
