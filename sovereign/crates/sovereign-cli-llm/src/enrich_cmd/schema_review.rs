// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich report / review` — the CLI surface for
//! [`sovereign_enrichment_build::schema_review`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::schema_review::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::schema_review::*;

const REPORT_HELP: Help = Help {
    command: "svrn enrich schema-report",
    summary: "Compute + print the §12 schema validation report for one corpus.",
    sections: &[
        HelpSection::Usage("svrn enrich schema-report <corpus-id> [--json]"),
        HelpSection::Flags(&[(
            "--json",
            "Emit the SchemaValidationReport as JSON instead of the human-readable table.",
        )]),
        HelpSection::Examples(&[(
            "svrn enrich schema-report brothers_karamazov",
            "Print the §12.4 diagnostic table: coverage / depth / confidence / orphans / gaps.",
        )]),
        HelpSection::Notes(
            "Requires a resolved atlas (run `svrn enrich atlas-resolve <corpus> \
             --phase all` first). The report is computed on demand — retrofitting \
             incremental writes into each phase is a follow-up. Also writes \
             `atlas/schema_validation.json` alongside the other atlas files.",
        ),
    ],
};
pub async fn cmd_schema_report(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&REPORT_HELP);
        return 0;
    }
    let parsed = match parse_report_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&REPORT_HELP);
            return 2;
        }
    };
    let outcome = match run(&parsed) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };
    match render(&parsed, &outcome) {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
const REVIEW_HELP: Help = Help {
    command: "svrn enrich schema-review",
    summary: "Compare schema validation reports across N corpora; flag systematic gaps.",
    sections: &[
        HelpSection::Usage("svrn enrich schema-review <corpus-a> <corpus-b> [<corpus-c> ...]"),
        HelpSection::Examples(&[(
            "svrn enrich schema-review brothers_karamazov compatibilism",
            "Compute both reports; flag gaps present in both as schema-revision candidates.",
        )]),
        HelpSection::Notes(
            "Per spec §12.5: a gap present in ≥ 2 corpora warrants schema revision; \
             a gap present in exactly one warrants prompt tuning. Each corpus must have \
             a resolved atlas.",
        ),
    ],
};
pub async fn cmd_schema_review(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&REVIEW_HELP);
        return 0;
    }
    let parsed = match parse_review_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&REVIEW_HELP);
            return 2;
        }
    };
    match run_review(&parsed) {
        Ok(comparison) => {
            render_review(&comparison);
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
