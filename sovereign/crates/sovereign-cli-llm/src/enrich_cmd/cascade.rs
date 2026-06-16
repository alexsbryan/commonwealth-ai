// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign enrich cascade <corpus> --from <phase>` — run every
//! phase downstream of (and including) `<phase>` in ordinal order.
//!
//! Used for the medium iteration loop: "I edited phase 5 exemplars;
//! rerun 5 and everything that depends on it." Phase 1 in a cascade
//! always uses `--full` (subset doesn't update caches that phases 2+
//! rely on).

use std::str::FromStr;
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    CascadeStep, ChapterSelection, PhaseCache, PhaseRunner, PipelinePhase,
    RunOutputWriter,
};

use super::config::EnrichConfig;
use super::corpus_io::build_corpus;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich cascade",
    summary: "Rerun a phase and every downstream phase that depends on it.",
    sections: &[
        HelpSection::Usage("sovereign enrich cascade <corpus-id> --from <phase>"),
        HelpSection::Flags(&[
            ("--from <phase>", "Starting phase: questions | question-clusters | concerns | chunk-clusters | positions | tensions | gaps"),
        ]),
        HelpSection::Examples(&[
            ("sovereign enrich cascade ak --from positions", "Rerun phases 5, 6, 7."),
            ("sovereign enrich cascade ak --from questions", "Full pipeline from phase 1 (uses --full)."),
        ]),
        HelpSection::Notes(
            "When `--from questions`, phase 1 runs with `--full`. Subset runs must go through `extract` directly.",
        ),
    ],
};

pub async fn cmd_cascade(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let (corpus_id, from) = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it first",
            cfg.base_url
        );
        return 2;
    }

    let pipeline = match super::pipeline_resolve::resolve_pipeline(&cfg) {
        Some(p) => p,
        None => {
            eprintln!("error: unknown pipeline: {}", cfg.pipeline_id);
            return 1;
        }
    };
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, chat) = client.into_closures();
    let cache = PhaseCache::new(paths::cache_dir(&corpus_id));
    let runs = RunOutputWriter::new(paths::runs_dir(&corpus_id));
    let runner = Arc::new(PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(&corpus_id),
    ));

    let (ctx, _manifest) = match build_corpus(&cfg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: building corpus context: {e}");
            return 1;
        }
    };

    println!("  cascading from {} …", from.id());

    let selection = if from == PipelinePhase::Questions {
        Some(ChapterSelection::Full)
    } else {
        None
    };
    let res = match runner.cascade(from, &ctx, selection).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cascade failed: {e}");
            return 1;
        }
    };

    // Print a per-step summary.
    for step in &res.steps {
        match step {
            CascadeStep::Phase1(r) => println!(
                "    ✓ phase 1: {} chapter(s) processed (failures: {})",
                r.output.questions_by_chapter.len(),
                r.failures.len()
            ),
            CascadeStep::Phase2(r) => println!(
                "    ✓ phase 2: {} cluster(s), {} unclustered",
                r.output.clusters.len(),
                r.output.unclustered.len()
            ),
            CascadeStep::Phase3(r) => println!(
                "    ✓ phase 3: {} concern(s) (failures: {})",
                r.output.concerns.len(),
                r.failures.len()
            ),
            CascadeStep::Phase4(r) => println!(
                "    ✓ phase 4: {} chunk cluster(s)",
                r.output.clusters.len()
            ),
            CascadeStep::Phase5(r) => println!(
                "    ✓ phase 5: {} position(s) (failures: {})",
                r.output.positions.len(),
                r.failures.len()
            ),
            CascadeStep::Phase6(r) => println!(
                "    ✓ phase 6: {} tension(s) (failures: {})",
                r.output.tensions.len(),
                r.failures.len()
            ),
            CascadeStep::Phase7(r) => println!("    ✓ phase 7: {} gap(s)", r.output.gaps.len()),
        }
    }

    0
}

fn parse_args(args: &[String]) -> Result<(String, PipelinePhase), String> {
    let mut corpus_id: Option<String> = None;
    let mut from: Option<PipelinePhase> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--from" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--from requires a phase id".to_string())?;
                from = Some(PipelinePhase::from_str(v).map_err(|e| format!("--from: {e}"))?);
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let from = from.ok_or_else(|| "missing --from <phase>".to_string())?;
    if from == PipelinePhase::Ingest {
        return Err("--from ingest is not a cascade starting point".into());
    }
    Ok((corpus_id, from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cascade_from_positions() {
        let args = vec!["ak".into(), "--from".into(), "positions".into()];
        let (id, from) = parse_args(&args).unwrap();
        assert_eq!(id, "ak");
        assert_eq!(from, PipelinePhase::Positions);
    }

    #[test]
    fn parse_cascade_requires_from() {
        let err = parse_args(&["ak".into()]).unwrap_err();
        assert!(err.contains("--from"));
    }

    #[test]
    fn parse_cascade_rejects_ingest() {
        let err = parse_args(&["ak".into(), "--from".into(), "ingest".into()]).unwrap_err();
        assert!(err.contains("not a cascade"));
    }

    #[test]
    fn parse_cascade_rejects_unknown_phase() {
        let err = parse_args(&["ak".into(), "--from".into(), "mystery".into()]).unwrap_err();
        assert!(err.contains("unknown phase"));
    }
}
