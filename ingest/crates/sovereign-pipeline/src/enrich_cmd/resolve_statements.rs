// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich resolve-statements`: the CLI surface for
//! [`sovereign_enrichment_build::resolve_statements`]. Help and the entry point
//! stay here; the work is in the capability crate.

use sovereign_cli_base::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::resolve_statements::*;

const HELP: Help = Help {
    command: "svrn enrich resolve-statements",
    summary: "RESOLVE alone: statements to records under one declared type, one cited model call per document.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich resolve-statements --recipe <recipe.toml> --type <name> --documents <docs.jsonl> \
             --statements <statements.jsonl> --out <dir> [--model <id>] [--neighbours N] \
             [--max-candidates N] [--min-similarity S] [--answer model|select|reason|proposed] [--same-wording-similarity S] \
             [--similar S] [--limit N]",
        ),
        HelpSection::Flags(&[
            ("--recipe", "The recipe whose [enrichment.ontology] declares the type: its `identity` keys and `identity_criterion`."),
            ("--type", "The declared type every statement is resolved under."),
            ("--documents", "JSON lines {id, title?, body, ...}; the field the recipe declares as `change.document.thread` proposes the records of the document's thread."),
            ("--statements", "JSON lines {document, id, start, end, keys?}; start/end are byte offsets into the body. Documents resolve in the order this file first names them."),
            ("--out", "Directory for decisions.jsonl, clustering.json (statement -> record; a refused statement alone), records.json, summary.json."),
            ("--model", "Chat model at the daemon. Default commonwealth/primary."),
            ("--neighbours", "How many most-similar earlier documents offer their records as candidates. Default 3."),
            ("--max-candidates", "At most this many candidate records from similar documents per document. Default 12; a declared thread's records are never cut."),
            ("--min-similarity", "A document less alike (TF-IDF cosine) offers no records. Default 0."),
            ("--answer", "`model` (default) asks the model for a cited partition of each document; `select` asks one forced choice per statement (which candidate, or none: a distribution in one forward pass, kept on each outcome); `reason` asks the same choice after the model reasons about it (`reasoned_choice`); `proposed` makes no call and takes the proposed answer as given, the zero-model floor a model answer is held to."),
            ("--same-wording-similarity", "The proposed answer joins a wording to a record said so from a document at least this alike. Default 0.4."),
            ("--similar", "The proposed answer joins any wording to the most similar record at or above this. Default off."),
            ("--limit", "Resolve only the first N documents."),
            (
                "--asker <daemon|replay|gold>",
                "Where RESOLVE's answers come from: the model, every answer recorded in \
                 <out>/answers.jsonl (the default); that store, never calling the daemon; or \
                 <out>/answers.gold.jsonl. A question with no stored answer is refused.",
            ),
        ]),
        HelpSection::Examples(&[(
            "svrn enrich resolve-statements --recipe research/ontology-apps/cdcr/recipe-gvc.toml --type happening \
             --documents ~/.svrnmesh/bench-corpora/gvc/raw/documents.jsonl --statements dev-statements.jsonl --out runs/gvc-dev",
            "Resolve GVC's dev mentions; then `svrn bench er-score runs/gvc-dev/clustering.json gold.json`.",
        )]),
        HelpSection::Notes(
            "Identity is decided by an equal declared key or by a model answer whose cited passage is found in the \
             document; anything else is refused and counted in summary.json, never defaulted. Needs the daemon for \
             any statement no key settles.",
        ),
    ],
};

pub async fn cmd_resolve_statements(args: &[String]) -> i32 {
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
        Ok(s) => {
            println!(
                "{} document(s), {} statement(s) -> {} record(s); {} call(s) ({:.2}/document); {:?}; wrote {}",
                s.documents,
                s.statements,
                s.records,
                s.calls,
                s.calls_per_document,
                s.tally,
                parsed.out.display()
            );
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
