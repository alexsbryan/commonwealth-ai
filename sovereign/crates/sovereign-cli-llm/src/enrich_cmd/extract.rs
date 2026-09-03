// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich extract` — the CLI surface for
//! [`sovereign_enrichment_build::extract`].
//!
//! Unlike its siblings this module never carried a verb triple: `cmd_extract`
//! WAS the work, and the build orchestrator called it directly with synthesized
//! argv. So the body moved down as `run_extract` and only the help text and the
//! `--help` gate stayed. Parsing runs here too, and only to print `HELP` on a
//! bad flag — that is the one behaviour a capability crate cannot provide,
//! since `HELP` is this host's user interface.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::extract::*;

const HELP: Help = Help {
    command: "svrn enrich extract",
    summary: "Run phase 1 (per-chapter question extraction) on a subset or the full corpus.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich extract <corpus-id> [--chapters <id1,id2,...> | --full | --retry-failed] [--terse] [--dry-run]",
        ),
        HelpSection::Flags(&[
            ("--chapters <ids>", "Comma-separated chapter ids (e.g. sec_0001,sec_0003). Subset runs do NOT update the cache."),
            ("--full", "Run on every chapter in the manifest. Updates cache/questions.json."),
            (
                "--retry-failed",
                "Re-run only the chapters that failed in the most recent run. Successful \
                 recoveries are merged into cache/questions.json (matching chapters \
                 overwritten; those failures dropped from the cached failures list). \
                 Errors if no prior run file exists.",
            ),
            (
                "--terse",
                "Use the terse Phase 1 prompt variant and double the configured \
                 max_output_tokens. Combinable with --chapters or --retry-failed. When paired \
                 with --retry-failed, auto-filters to failures the terse variant can recover \
                 (think-truncation and parse-drift — both benefit from the bumped output \
                 budget). Successful retries merge into cache/questions.json.",
            ),
            (
                "--resume",
                "Crash-resilient resume. Reads the per-chapter JSONL checkpoint at \
                 runs/_phase1_checkpoint.jsonl and skips chapter ids already recorded \
                 there (success OR failure). The runner appends to the checkpoint after \
                 every chapter completes, so a kill / crash / power loss mid-run loses \
                 at most one chapter. Combine with --full for long Wikipedia-scale Tier-2 \
                 runs.",
            ),
            (
                "--finalize",
                "Read runs/_phase1_checkpoint.jsonl, write a canonical run-file from it, \
                 and (when applicable) update cache/questions.json. Use after a long \
                 --resume sequence has covered every chapter — no LLM calls fired by this \
                 mode. Mutually exclusive with --chapters / --full / --retry-failed.",
            ),
            (
                "--dry-run",
                "Compose each selected chapter's Phase 1 prompt — system, user and response \
                 schema — print it, and call no model. Exemplar selection and the Stage-1a \
                 seed lookup still run, because both are IN the prompt, so what is printed \
                 is what the real pass would send. This is the cheap loop for a prompt or \
                 ontology change: seconds, where a rebuild is minutes. Refused with \
                 --finalize.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich extract ak --chapters sec_0001,sec_0011,sec_0023",
                "Fast-loop subset run (2-3 min). Output written to runs/.",
            ),
            (
                "svrn enrich extract ak --full",
                "Full-corpus run. Updates cache/questions.json — consumed by phases 2+.",
            ),
            (
                "svrn enrich extract ak --retry-failed",
                "Reprocess the chapters that failed in the last run (parse errors, transient chat failures).",
            ),
            (
                "svrn enrich extract bk --retry-failed --terse",
                "Recover chapters whose default pass failed with <think> truncation, using the terse prompt variant.",
            ),
            (
                "svrn enrich extract wessex-hoard --chapters sec_00014 --dry-run",
                "Print the phase-1 prompt this chapter WOULD be sent — system, user, response schema — and call no model. Seconds, where a rebuild is minutes.",
            ),
        ]),
        HelpSection::Notes(
            "Requires `svrn enrich init` first. Daemon must be running at localhost:9741.",
        ),
    ],
};
pub async fn cmd_extract(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    // Validate before delegating, purely so a bad flag still prints HELP the
    // way it always has. `run_extract` re-parses; argv parsing is pure.
    if let Err(msg) = parse_args(args) {
        eprintln!("error: {msg}");
        eprintln!();
        help::print(&HELP);
        return 2;
    }
    run_extract(args).await
}
