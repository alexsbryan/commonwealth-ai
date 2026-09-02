// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich configure` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_configuration`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_configuration::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_configuration::*;

const HELP: Help = Help {
    command: "svrn enrich atlas-configuration",
    summary: "Detect 0–3 interpretive Configuration atoms from the resolved atlas (LLM).",
    sections: &[
        HelpSection::Usage("svrn enrich atlas-configuration <corpus-id>"),
        HelpSection::Examples(&[(
            "svrn enrich atlas-configuration brothers_karamazov",
            "Summarise atlas → prompt the configured pipeline's Phase 8 → write configurations.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `svrn enrich atlas-resolve <corpus> --phase all`. \
             Opt-in: only pipelines whose `runs_configuration_phase()` returns true \
             (`literary_atlas`, future `philosophy_atlas`) actually dispatch an LLM call. \
             Produces `~/.svrnmesh/indexes/<corpus>/atlas/configurations.json` and \
             merges configurations into `atoms.json` so the brief assembler sees them \
             without a separate read.",
        ),
    ],
};
pub async fn cmd_atlas_configuration(args: &[String]) -> i32 {
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
        Err(e) => {
            eprintln!("error: {}", e.message());
            e.exit_code()
        }
    }
}
