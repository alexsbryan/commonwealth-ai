// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich report / review` — the CLI surface for
//! [`sovereign_enrichment_build::schema_review`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::schema_review::…` keeps resolving for this crate's siblings.

use super::config::EnrichConfig;
use corpus_engine::enrichment::atlas::{
    build_schema_validation_report, compare_across_corpora, count_open_questions,
    count_transitions_without_trigger, count_ungrounded_claims, read_atlas_atoms,
    read_atlas_cross_corpus_edges, read_atlas_edges, AtomEnvelope, SchemaValidationInput,
    SchemaValidationReport, ATLAS_DIRNAME,
};
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

    let mut reports = Vec::new();
    for corpus_id in &parsed.corpora {
        let cfg = match EnrichConfig::require(corpus_id) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: loading config for `{corpus_id}`: {e}");
                return 1;
            }
        };
        let report = match compute_report(&cfg.corpus_id) {
            Ok(r) => r,
            Err(msg) => {
                eprintln!("error: {msg}");
                return 1;
            }
        };
        reports.push(report);
    }

    let comparison = compare_across_corpora(&reports);

    println!(
        "=== Schema review across {} corpora ===",
        comparison.corpora.len()
    );
    for c in &comparison.corpora {
        println!("  · {c}");
    }
    println!();

    if comparison.convergent_gaps.is_empty() {
        println!("  No convergent gaps — no schema revision candidates.");
    } else {
        println!("  Convergent gaps (schema revision candidates — present in ≥ 2 corpora):");
        for g in &comparison.convergent_gaps {
            println!();
            println!("  [{}]", g.signature);
            println!("    present_in:     {}", g.present_in.join(", "));
            println!("    recommendation: {}", g.recommendation);
        }
    }
    println!();

    if comparison.idiosyncratic_gaps.is_empty() {
        println!("  No idiosyncratic gaps — nothing single-corpus to tune.");
    } else {
        println!(
            "  Idiosyncratic gaps (prompt-tuning candidates — present in exactly one \
             corpus):"
        );
        for g in &comparison.idiosyncratic_gaps {
            println!("    · [{}] on {}", g.signature, g.present_in);
        }
    }

    0
}
