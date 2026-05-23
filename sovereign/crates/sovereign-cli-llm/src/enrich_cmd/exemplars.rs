//! `sovereign enrich exemplars <corpus>` — exemplar-bank status + lint.
//!
//! Walks every per-phase bank file under the corpus's exemplars
//! directory and prints `(positive, corrected, negative)` counts
//! along with lint findings (empty rationale, duplicate ids, missing
//! `model_output` on corrected/negative entries). Gives the developer
//! a quick sanity check before running a phase.

use corpus_engine::enrichment::pipeline::{ExemplarBank, PipelinePhase};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich exemplars",
    summary: "Report exemplar-bank counts and lint findings per phase.",
    sections: &[
        HelpSection::Usage("sovereign enrich exemplars <corpus-id>"),
        HelpSection::Notes(
            "Missing bank files are reported as '(no bank file)'. Each phase's file lives at \
             ~/.sovereign/enrichment/<corpus>/exemplars/<phase-id>.json. Hand-edit the JSON \
             or use `sovereign enrich promote` (Landing 4) to append curated exemplars.",
        ),
    ],
};

pub async fn cmd_exemplars(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let corpus_id = match args.first() {
        Some(s) if !s.starts_with("--") => s.clone(),
        _ => {
            eprintln!("error: missing <corpus-id>");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    if let Err(e) = EnrichConfig::require(&corpus_id) {
        eprintln!("error: {e}");
        return 1;
    }

    println!("  Exemplar banks for corpus '{corpus_id}':");
    println!();

    let mut any_issues = false;
    for phase in PipelinePhase::ALL {
        // Ingest has no exemplar bank (no LLM call).
        if *phase == PipelinePhase::Ingest {
            continue;
        }
        // Phase 2 and 4 are pure HDBSCAN — no LLM, no exemplars.
        if matches!(
            phase,
            PipelinePhase::QuestionClusters | PipelinePhase::ChunkClusters
        ) {
            continue;
        }

        let path = paths::exemplars_dir(&corpus_id).join(format!("{}.json", phase.id()));
        print!("    {:<12}", phase.id());
        if !path.exists() {
            println!(" (no bank file)");
            continue;
        }
        match ExemplarBank::open(&path, *phase) {
            Ok(bank) => {
                let (p, c, n) = bank.counts_by_kind();
                print!(" {} total — {} positive, {} corrected, {} negative", bank.len(), p, c, n);
                let lints = bank.lint();
                if lints.is_empty() {
                    println!();
                } else {
                    any_issues = true;
                    println!("  ⚠ {} lint issue(s)", lints.len());
                    for l in lints {
                        println!("      · [{}] {} — {}", l.index, l.id, l.reason);
                    }
                }
            }
            Err(e) => {
                any_issues = true;
                println!();
                println!("      ! failed to open: {e}");
            }
        }
    }

    if any_issues {
        println!();
        println!("  One or more banks had lint findings. Fix them before the next run.");
        return 1;
    }
    0
}
