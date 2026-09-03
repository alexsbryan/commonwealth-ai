// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich tensions-classify` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_tensions_classify`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_tensions_classify::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_tensions_classify::*;

const HELP: Help = Help {
    command: "svrn enrich atlas-tensions-classify",
    summary: "LLM-classify tension candidates and merge accepted ones into edges.json.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich atlas-tensions-classify <corpus-id> [--max-candidates <n>] [--dry-run]",
        ),
        HelpSection::Flags(&[
            (
                "--max-candidates <n>",
                "Cap the number of candidates classified this run. Useful for \
                 prompt-tuning iterations on a fixed slice. Default: classify every \
                 candidate in tension_candidates.json.",
            ),
            (
                "--dry-run",
                "Compose every prompt + print to stdout, but do not call the model. \
                 Useful for inspecting prompt content before a full run.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich atlas-tensions-classify brothers_karamazov",
                "Classify every candidate in bk's tension_candidates.json and merge \
                 the accepted Tension edges into edges.json.",
            ),
            (
                "svrn enrich atlas-tensions-classify dubliners-test --max-candidates 5",
                "Quick prompt-tuning iteration: only classify the first 5 candidates.",
            ),
        ]),
        HelpSection::Notes(
            "Requires `svrn enrich atlas-tensions <corpus>` to have run first \
             (so tension_candidates.json exists) and a daemon at localhost:9741. \
             Replaces prior LlmPairwise Tension edges in edges.json; preserves every \
             other edge type and every other-provenance edge untouched.",
        ),
    ],
};
pub async fn cmd_atlas_tensions_classify(args: &[String]) -> i32 {
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
