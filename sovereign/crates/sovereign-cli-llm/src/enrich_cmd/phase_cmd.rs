// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared implementation for the six structural phase subcommands
//! (`cluster-questions`, `name-concerns`, `cluster-chunks`,
//! `extract-positions`, `detect-tensions`, `detect-gaps`).
//!
//! Every subcommand parses the same `<corpus-id>` argument, loads the
//! pinned config, probes the daemon, builds the `PhaseRunner`, calls
//! the matching runner method, and prints a one-line summary. Moving
//! the boilerplate here keeps the per-phase subcommand files trivial.

use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    PhaseCache, PhaseRunner, RunOutputWriter,
};

use super::config::EnrichConfig;
use super::corpus_io::build_corpus;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::paths;

/// Which structural phase this command runs.
#[derive(Debug, Clone, Copy)]
pub enum PhaseOp {
    ClusterQuestions,
    NameConcerns,
    ClusterChunks,
    ExtractPositions,
    DetectTensions,
    DetectGaps,
}

impl PhaseOp {
    /// Human label used in the `  running <label>…` banner.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClusterQuestions => "phase 2 (cluster questions)",
            Self::NameConcerns => "phase 3 (name concerns)",
            Self::ClusterChunks => "phase 4 (cluster chunks)",
            Self::ExtractPositions => "phase 5 (extract positions)",
            Self::DetectTensions => "phase 6 (detect tensions)",
            Self::DetectGaps => "phase 7 (detect gaps)",
        }
    }
}

/// Shared body for every structural-phase subcommand.
pub async fn run_phase(op: PhaseOp, args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        print_help(op);
        return 0;
    }
    let corpus_id = match args.first() {
        Some(s) if !s.starts_with("--") => s.clone(),
        _ => {
            eprintln!("error: missing <corpus-id>");
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

    println!("  running {}…", op.label());

    let result: Result<(usize, std::path::PathBuf, Vec<String>), String> = match op {
        PhaseOp::ClusterQuestions => match runner.phase_2_cluster_questions().await {
            Ok(r) => Ok((r.output.clusters.len(), r.run_path, Vec::new())),
            Err(e) => Err(e.to_string()),
        },
        PhaseOp::NameConcerns => {
            let (ctx, _) = match build_corpus(&cfg) {
                Ok(x) => x,
                Err(e) => return fail(&format!("building corpus context: {e}")),
            };
            match runner.phase_3_name_concerns(&ctx).await {
                Ok(r) => Ok((
                    r.output.concerns.len(),
                    r.run_path,
                    r.failures
                        .into_iter()
                        .map(|f| format!("{}: {}", f.subject, f.reason))
                        .collect(),
                )),
                Err(e) => Err(e.to_string()),
            }
        }
        PhaseOp::ClusterChunks => {
            let (ctx, _) = match build_corpus(&cfg) {
                Ok(x) => x,
                Err(e) => return fail(&format!("building corpus context: {e}")),
            };
            match runner.phase_4_cluster_chunks(&ctx).await {
                Ok(r) => Ok((r.output.clusters.len(), r.run_path, Vec::new())),
                Err(e) => Err(e.to_string()),
            }
        }
        PhaseOp::ExtractPositions => {
            let (ctx, _) = match build_corpus(&cfg) {
                Ok(x) => x,
                Err(e) => return fail(&format!("building corpus context: {e}")),
            };
            match runner.phase_5_extract_positions(&ctx).await {
                Ok(r) => Ok((
                    r.output.positions.len(),
                    r.run_path,
                    r.failures
                        .into_iter()
                        .map(|f| format!("{}: {}", f.subject, f.reason))
                        .collect(),
                )),
                Err(e) => Err(e.to_string()),
            }
        }
        PhaseOp::DetectTensions => match runner.phase_6_detect_tensions().await {
            Ok(r) => Ok((
                r.output.tensions.len(),
                r.run_path,
                r.failures
                    .into_iter()
                    .map(|f| format!("{}: {}", f.subject, f.reason))
                    .collect(),
            )),
            Err(e) => Err(e.to_string()),
        },
        PhaseOp::DetectGaps => {
            let (ctx, _) = match build_corpus(&cfg) {
                Ok(x) => x,
                Err(e) => return fail(&format!("building corpus context: {e}")),
            };
            match runner.phase_7_detect_gaps(&ctx).await {
                Ok(r) => Ok((r.output.gaps.len(), r.run_path, Vec::new())),
                Err(e) => Err(e.to_string()),
            }
        }
    };

    match result {
        Ok((count, path, failures)) => {
            println!("  ✓ produced {count} item(s) — {}", path.display());
            if !failures.is_empty() {
                eprintln!("  ! {} per-item failures:", failures.len());
                for f in &failures {
                    eprintln!("      · {f}");
                }
                return 1;
            }
            0
        }
        Err(msg) => fail(&msg),
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("error: {msg}");
    1
}

const PHASE_HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn enrich <phase>",
    summary: "Run one structural phase against a corpus (2-7).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn enrich <cluster-questions|name-concerns|cluster-chunks|\n  extract-positions|detect-tensions|detect-gaps> <corpus-id>",
        ),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Requires upstream caches. Run `svrn enrich status <corpus>` to see \
             which phases are fresh, stale, or never-run.",
        ),
    ],
};

fn print_help(_op: PhaseOp) {
    sovereign_cli_shared::help::print(&PHASE_HELP);
}
