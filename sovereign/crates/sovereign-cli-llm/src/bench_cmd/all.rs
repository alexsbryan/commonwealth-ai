// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench all` — cross-bench rollup driver.
//!
//! Discovers every bench under `sovereign/bench/`, scores each
//! against its baseline, renders a two-pane scoreboard
//! (Enrichment-eval + Retrieval+LLM-judge), exits 0/1 for CI.
//!
//! Two scoring paths:
//! - **Enrichment lane** — in-process call to
//!   `enrich_cmd::eval::score_corpus`. Reads atoms.json directly;
//!   no daemon dependency.
//! - **Retrieval lane** — subprocess `svrn eval run`. The
//!   retrieval path needs a live `ChatSession` (intent classifier,
//!   embed slot, atlas-context manager) and replicating that boot
//!   path here would couple `bench all` to every retrieval-stack
//!   refactor. Subprocess keeps the dependency boundary tight.
//!
//! Default mode is `score_only` against existing atoms / live
//! daemon. `--rebuild` (Stage 6) re-extracts the atlas before
//! scoring. The retrieval lane has no `--rebuild` semantics — the
//! index is owned by the daemon, not `bench all`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;

use crate::enrich_cmd::eval::{score_corpus, EvalReport, PhaseFilter};
use crate::eval_cmd::runner::EvalRun;
use sovereign_cli_shared::help::{self, Help, HelpSection};

use super::baselines::{read_latest, write_dated_and_update_latest};
use super::discover::{discover_benches, BenchSurface, CorpusState, DiscoveredBench};

const HELP: Help = Help {
    command: "svrn bench all",
    summary: "Run every discovered bench (enrichment + retrieval), diff vs baseline, exit 0/1.",
    sections: &[
        HelpSection::Usage(
            "svrn bench all [--bench-root <path>] [--filter <pattern>] [--update-baseline] [--report <path>]",
        ),
        HelpSection::Flags(&[
            (
                "--bench-root <path>",
                "Directory containing per-group bench dirs. Default: sovereign/bench.",
            ),
            (
                "--filter <pattern>",
                "Substring filter against `<group>/<bench-id>`. Default: run every discovered bench.",
            ),
            (
                "--update-baseline",
                "After scoring, write the current run as the new baseline (dated snapshot + retarget latest.json symlink).",
            ),
            (
                "--report <path>",
                "Persist the combined results bundle as JSON.",
            ),
            (
                "--regression-threshold <pt>",
                "Threshold below which a delta is treated as noise. Default: 0.5 (pt of F1).",
            ),
            (
                "--rebuild",
                "Before scoring, re-extract the enrichment-lane corpus's atlas via `svrn enrich build <id>`. \
                 GPU-bound, sequential. No-op for retrieval-lane benches (index is owned by the daemon).",
            ),
            (
                "--retrieval-limit <N>",
                "Top-K cap passed to `svrn eval run --limit` on retrieval-lane benches. Default 30 \
                 (matches the chunk counts the synced pre-monorepo baselines were captured at).",
            ),
            (
                "--synth",
                "Drive the FULL chat pipeline (intent classifier → router → search → synthesis) via \
                 `svrn eval run --synth` instead of bare retrieval. Most faithful proxy for \
                 desktop-chat propagation — same `runtime.handle_message_stream` entry point. Costs \
                 one LLM chat call per question. Synth baselines stored at `baselines/<bench>-synth/`.",
            ),
            (
                "--sample-questions <N>",
                "Lean-QA cap (with --synth): forward `--sample-questions N` to each bank's synth \
                 run, down-sampling to at most N questions stratified by category. Trades \
                 exhaustiveness for wall time (SEP's 35-question synth ≈ 100 min → a few min at \
                 N=5). The sampled run is ADVISORY — not baseline-comparable. No-op without --synth.",
            ),
            (
                "--prod-pipeline",
                "Drive the PRODUCTION KnowledgeQuery retrieval pipeline in-process per question \
                 via `svrn eval run --prod-pipeline` (no synthesis) and score its evidence pool — \
                 the bench-prod parity lane (RETRIEVAL_REDESIGN.md §7.1). Deterministic; pair with \
                 --isolate for cross-box stability. Baselines stored at `baselines/<bench>-prod/` \
                 (`-prod-isolated` with --isolate).",
            ),
            (
                "--routing-only",
                "Drive ONLY the intent classifier (no retrieval, no synthesis) via `svrn eval \
                 run --routing-only`. Fastest iteration loop for classifier-prompt tuning: ~0.5-2s \
                 per question. Mutually exclusive with --synth. Baselines stored at \
                 `baselines/<bench>-routing/`.",
            ),
            (
                "--isolate",
                "Per-corpus isolation (with --synth). Scopes each bank's retrieval to its target \
                 corpus so the run measures that corpus's INTEGRITY in isolation (does it hold + \
                 retrieve the facts its queries need?) rather than its performance amid \
                 cross-corpus competition. The unscoped --synth run remains the cross-corpus UX \
                 measure. Baselines stored at `baselines/<bench>-synth-isolated/`.",
            ),
        ]),
        HelpSection::Notes(
            "Enrichment lane reads ~/.sovereign/indexes/<corpus>/atlas/atoms.json directly. \
             Retrieval lane subprocesses `svrn eval run` which needs a live daemon at \
             localhost:9741. Retrieval-lane benches whose corpus index is missing get marked \
             stale; the report prints `svrn corpus install <id>` hints.",
        ),
    ],
};

/// Combined per-bench result. One of the two arms is always set
/// based on the bench's surface; the baseline arm may be `None` when
/// no baseline exists yet (first run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchOutcome {
    pub id: String,
    pub group: String,
    pub corpus_id: String,
    pub surface: String,
    pub status: BenchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrichment: Option<EnrichmentOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalOutcome>,
    pub levers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Capture date (YYYY-MM-DD) of the baseline this run was diffed
    /// against, from the dated snapshot filename — `None` on first-run
    /// and stale outcomes. See `baselines::baseline_age`; rendered with
    /// a staleness warning past `SOVEREIGN_BASELINE_MAX_AGE_DAYS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_captured: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_age_days: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchStatus {
    /// All levers within regression threshold of baseline.
    Green,
    /// At least one lever moved past the regression threshold
    /// downward.
    Regressed,
    /// At least one lever improved past the threshold (no
    /// regressions).
    Improved,
    /// Ran but no baseline existed; current run was just written as
    /// the new baseline.
    FirstRun,
    /// Couldn't run — corpus missing, subprocess failed, etc.
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentOutcome {
    pub current: EvalReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<EvalReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalOutcome {
    pub current: EvalRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<EvalRun>,
}

#[derive(Debug, Clone)]
struct Opts {
    bench_root: PathBuf,
    filter: Option<String>,
    update_baseline: bool,
    rebuild: bool,
    report: Option<PathBuf>,
    regression_threshold: f32,
    /// Top-K cap passed to `svrn eval run --limit` on retrieval
    /// lane subprocess invocations. Default 30 to match the chunk
    /// counts the pre-monorepo baselines were captured at; CLI
    /// default (10) was producing apples-to-oranges source_recall
    /// regressions.
    retrieval_limit: usize,
    /// Lean-QA cap forwarded to `svrn eval run --sample-questions` on synth
    /// lanes: down-sample each bank to at most N questions, stratified by
    /// category. None = full bank. Only affects `--synth` runs (the slow
    /// lane); retrieval/routing keep the full set for stable HARD-gate
    /// denominators. A sampled synth run is advisory, not baseline-comparable.
    sample_questions: Option<usize>,
    /// When true, retrieval-lane benches drive the FULL chat pipeline
    /// (intent classifier → router → search tools → synthesis) via
    /// `svrn eval run --synth` instead of the bare embed→search
    /// path. Synth mode is the most faithful proxy for "does this
    /// bench improvement propagate to the desktop chat experience?"
    /// — same `runtime.handle_message_stream` entry point.
    ///
    /// Cost: one LLM chat call per question (~5-30s on the loaded
    /// model). Default off; opt in for end-to-end regression gates.
    /// Baselines are stored under `<bench-id>-synth/` to keep
    /// retrieval-mode and synth-mode runs from overwriting each
    /// other.
    synth: bool,
    /// When true, retrieval-lane benches drive ONLY the router
    /// classifier (no retrieval, no synthesis) via
    /// `svrn eval run --routing-only`. Fastest possible
    /// iteration loop for classifier-prompt tuning — ~0.5-2s per
    /// question (one fast-slot call). Mutually exclusive with
    /// --synth.
    ///
    /// Output is a `RoutingRun` (per-question intent decision vs
    /// expected). Bench-all renders a compact accuracy table +
    /// flags misroutes for the operator to investigate. Baselines
    /// stored at `baselines/<bench>-routing/` to keep separate
    /// from retrieval and synth modes.
    routing_only: bool,
    /// Bench-prod parity mode. Passes `--prod-pipeline` to `eval run`:
    /// each question drives the production KQ retrieval pipeline
    /// in-process (no synthesis) and the composed evidence pool is
    /// scored. Deterministic (intent pinned). Baselines stored at
    /// `baselines/<bench>-prod/` (`-prod-isolated` with --isolate).
    prod_pipeline: bool,
    /// Per-corpus isolation (synth only). Passes `--isolate` to
    /// `eval run`, scoping each bank's retrieval to its target corpus
    /// to measure that corpus's integrity in isolation rather than its
    /// performance amid cross-corpus competition. Baselines stored at
    /// `baselines/<bench>-synth-isolated/`.
    isolate: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            bench_root: PathBuf::from("sovereign/bench"),
            filter: None,
            update_baseline: false,
            rebuild: false,
            report: None,
            regression_threshold: 0.005, // 0.5 pt
            retrieval_limit: 30,
            sample_questions: None,
            synth: false,
            prod_pipeline: false,
            routing_only: false,
            isolate: false,
        }
    }
}

/// Tag the bench's id with the active retrieval mode (`-synth`,
/// `-routing`) so the three modes don't overwrite each other's
/// baselines. Enrichment-lane benches are unaffected (modes have no
/// meaning there).
fn baseline_bench(bench: &DiscoveredBench, opts: &Opts) -> DiscoveredBench {
    if bench.surface != BenchSurface::RetrievalJudge {
        return bench.clone();
    }
    let suffix = if opts.routing_only {
        "-routing"
    } else if opts.prod_pipeline && opts.isolate {
        "-prod-isolated"
    } else if opts.prod_pipeline {
        "-prod"
    } else if opts.synth && opts.isolate {
        "-synth-isolated"
    } else if opts.synth {
        "-synth"
    } else {
        ""
    };
    if suffix.is_empty() {
        bench.clone()
    } else {
        let mut tagged = bench.clone();
        tagged.id = format!("{}{}", bench.id, suffix);
        tagged
    }
}

pub async fn cmd_all(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    let benches = discover_benches(&opts.bench_root);
    if benches.is_empty() {
        eprintln!(
            "error: no benches discovered under {}",
            opts.bench_root.display()
        );
        return 1;
    }

    let filtered: Vec<_> = benches
        .into_iter()
        .filter(|b| match &opts.filter {
            None => true,
            Some(pat) => {
                let target = format!("{}/{}", b.group, b.id);
                target.contains(pat.as_str())
            }
        })
        .collect();

    if filtered.is_empty() {
        eprintln!(
            "error: no benches matched filter {:?}",
            opts.filter.as_deref().unwrap_or("")
        );
        return 1;
    }

    eprintln!(
        "discovered {} bench{}",
        filtered.len(),
        if filtered.len() == 1 { "" } else { "es" }
    );
    for b in &filtered {
        eprintln!(
            "  · {:<10} {}/{} → corpus `{}` ({})",
            b.surface.label(),
            b.group,
            b.id,
            b.corpus_id,
            corpus_state_tag(b.corpus_state),
        );
    }
    eprintln!();

    let mut outcomes = Vec::with_capacity(filtered.len());
    for bench in &filtered {
        let outcome = run_one(bench, &opts).await;
        outcomes.push(outcome);
    }

    super::render::print_two_pane_scoreboard(&outcomes);

    if let Some(report_path) = &opts.report {
        if let Err(e) = persist_report(report_path, &outcomes) {
            eprintln!("error: write report {}: {e}", report_path.display());
            return 1;
        }
    }

    exit_code_from(&outcomes)
}

async fn run_one(bench: &DiscoveredBench, opts: &Opts) -> BenchOutcome {
    // Rebuild pathway. Only enrichment lane has rebuild semantics —
    // retrieval-lane corpora are owned by the daemon's installed
    // indexes, not by `bench all`. Calling --rebuild against a
    // retrieval bench warn-logs and continues to score against the
    // existing index.
    if opts.rebuild && bench.surface == BenchSurface::Enrichment {
        if let Err(e) = rebuild_corpus(bench).await {
            return BenchOutcome {
                id: bench.id.clone(),
                group: bench.group.clone(),
                corpus_id: bench.corpus_id.clone(),
                surface: bench.surface.label().to_string(),
                status: BenchStatus::Stale,
                enrichment: None,
                retrieval: None,
                levers: bench.levers.clone(),
                baseline_captured: None,
                baseline_age_days: None,
                note: Some(format!("rebuild failed: {e}")),
            };
        }
    } else if opts.rebuild {
        eprintln!(
            "warn: --rebuild has no effect on retrieval-lane bench {}/{} \
             (index ownership lives in the daemon; run `svrn corpus refresh` or \
             restart the daemon to re-index)",
            bench.group, bench.id
        );
    }

    if !bench.corpus_state.is_ready_for(bench.surface) {
        return BenchOutcome {
            id: bench.id.clone(),
            group: bench.group.clone(),
            corpus_id: bench.corpus_id.clone(),
            surface: bench.surface.label().to_string(),
            status: BenchStatus::Stale,
            enrichment: None,
            retrieval: None,
            levers: bench.levers.clone(),
            baseline_captured: None,
            baseline_age_days: None,
            note: Some(stale_hint(bench)),
        };
    }

    match bench.surface {
        BenchSurface::Enrichment => run_enrichment(bench, opts).await,
        BenchSurface::RetrievalJudge => {
            if opts.routing_only {
                run_routing_only(bench, opts).await
            } else {
                run_retrieval(bench, opts).await
            }
        }
    }
}

/// Routing-only mode: drives `svrn eval run --routing-only`,
/// captures the per-question intent decision, renders a compact
/// accuracy line. No retrieval, no synthesis — ~0.5-2s per question.
async fn run_routing_only(bench: &DiscoveredBench, opts: &Opts) -> BenchOutcome {
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return outcome_subprocess_fail(bench, format!("tempdir: {e}")),
    };
    let out_json = tmp.path().join("run.json");
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return outcome_subprocess_fail(bench, format!("current_exe: {e}")),
    };

    let status = Command::new(&exe)
        .args([
            "eval",
            "run",
            "--bank",
            bench.bench_path.to_str().unwrap_or(""),
            "--routing-only",
            "--output",
            out_json.to_str().unwrap_or(""),
        ])
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return outcome_subprocess_fail(
                bench,
                format!(
                    "`eval run --routing-only` exited {}",
                    s.code().unwrap_or(-1)
                ),
            )
        }
        Err(e) => return outcome_subprocess_fail(bench, format!("spawn: {e}")),
    }

    let bytes = match std::fs::read(&out_json) {
        Ok(b) => b,
        Err(e) => return outcome_subprocess_fail(bench, format!("read output: {e}")),
    };
    // RoutingRun is the subprocess output shape; parse loosely via
    // serde_json::Value so we don't pull the type into all.rs.
    let parsed: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return outcome_subprocess_fail(bench, format!("parse: {e}")),
    };
    let results = parsed
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let total = results.len();
    let correct = results
        .iter()
        .filter(|r| r.get("correct").and_then(|c| c.as_bool()).unwrap_or(false))
        .count();
    let misroutes: Vec<String> = results
        .iter()
        .filter(|r| !r.get("correct").and_then(|c| c.as_bool()).unwrap_or(true))
        .map(|r| {
            let id = r.get("question_id").and_then(|v| v.as_str()).unwrap_or("?");
            let expected = r.get("expected").and_then(|v| v.as_str()).unwrap_or("?");
            let actual = r
                .get("actual_intent")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{id}: expected={expected} actual={actual}")
        })
        .collect();

    // Persist as baseline so future runs can diff (under -routing
    // subdir; see `baseline_bench`).
    let baseline_view = baseline_bench(bench, opts);
    let prior: Option<serde_json::Value> =
        read_latest(&opts.bench_root, &baseline_view).ok().flatten();
    if opts.update_baseline || prior.is_none() {
        if let Err(e) = write_dated_and_update_latest(&opts.bench_root, &baseline_view, &parsed) {
            eprintln!("warn: writing routing baseline: {e}");
        }
    }
    let prior_correct = prior
        .as_ref()
        .and_then(|p| p.get("results")?.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|r| r.get("correct").and_then(|c| c.as_bool()).unwrap_or(false))
                .count()
        });

    let status = match prior_correct {
        None => BenchStatus::FirstRun,
        Some(prev) if correct < prev => BenchStatus::Regressed,
        Some(prev) if correct > prev => BenchStatus::Improved,
        Some(_) => BenchStatus::Green,
    };
    let note = if misroutes.is_empty() {
        Some(format!("routing accuracy {}/{} ✓", correct, total))
    } else {
        Some(format!(
            "routing {}/{} correct · {} misroute(s):\n     {}",
            correct,
            total,
            misroutes.len(),
            misroutes.join("\n     ")
        ))
    };

    let mut outcome = BenchOutcome {
        id: bench.id.clone(),
        group: bench.group.clone(),
        corpus_id: bench.corpus_id.clone(),
        surface: bench.surface.label().to_string(),
        status,
        enrichment: None,
        retrieval: None,
        levers: bench.levers.clone(),
        baseline_captured: None,
        baseline_age_days: None,
        note,
    };
    stamp_baseline_age(&mut outcome, &opts.bench_root, &baseline_view);
    outcome
}

/// Shell out to `svrn enrich build <corpus_id>`. Sequential
/// because LLM workers are GPU-bound. Captures duration; pipes stdout
/// to a log file under `target/sov-bench/runs/<ts>/<bench-id>.log`.
async fn rebuild_corpus(bench: &DiscoveredBench) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let log_dir = PathBuf::from("target/sov-bench/runs").join(ts);
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("mkdir {}: {e}", log_dir.display()))?;
    let log_path = log_dir.join(format!("{}-rebuild.log", bench.id));
    let log_file = std::fs::File::create(&log_path)
        .map_err(|e| format!("create log {}: {e}", log_path.display()))?;
    let stderr_log = log_file
        .try_clone()
        .map_err(|e| format!("clone log fd: {e}"))?;

    eprintln!(
        "rebuilding `{}` (logs → {})",
        bench.corpus_id,
        log_path.display()
    );

    let start = std::time::Instant::now();
    let status = Command::new(&exe)
        .args(["enrich", "build", &bench.corpus_id])
        .stdout(log_file)
        .stderr(stderr_log)
        .status()
        .map_err(|e| format!("spawn enrich build: {e}"))?;

    let elapsed = start.elapsed();
    if !status.success() {
        return Err(format!(
            "exit {} after {:.1}s; see {}",
            status.code().unwrap_or(-1),
            elapsed.as_secs_f32(),
            log_path.display()
        ));
    }
    eprintln!("  rebuilt in {:.1}s", elapsed.as_secs_f32());
    Ok(())
}

async fn run_enrichment(bench: &DiscoveredBench, opts: &Opts) -> BenchOutcome {
    let current = match score_corpus(&bench.corpus_id, &bench.bench_path, PhaseFilter::All) {
        Ok(r) => r,
        Err(e) => {
            return BenchOutcome {
                id: bench.id.clone(),
                group: bench.group.clone(),
                corpus_id: bench.corpus_id.clone(),
                surface: bench.surface.label().to_string(),
                status: BenchStatus::Stale,
                enrichment: None,
                retrieval: None,
                levers: bench.levers.clone(),
                baseline_captured: None,
                baseline_age_days: None,
                note: Some(format!("score_corpus failed: {e}")),
            };
        }
    };

    let baseline: Option<EvalReport> = read_latest(&opts.bench_root, bench).unwrap_or_else(|e| {
        eprintln!(
            "warn: reading {} baseline failed: {e} — treating as first run",
            bench.id
        );
        None
    });

    if opts.update_baseline || baseline.is_none() {
        if let Err(e) = write_dated_and_update_latest(&opts.bench_root, bench, &current) {
            eprintln!(
                "warn: writing baseline for {}/{} failed: {e}",
                bench.group, bench.id
            );
        }
    }

    let status = match &baseline {
        None => BenchStatus::FirstRun,
        Some(prev) => classify_enrichment(prev, &current, opts.regression_threshold),
    };

    let mut outcome = BenchOutcome {
        id: bench.id.clone(),
        group: bench.group.clone(),
        corpus_id: bench.corpus_id.clone(),
        surface: bench.surface.label().to_string(),
        status,
        enrichment: Some(EnrichmentOutcome { current, baseline }),
        retrieval: None,
        levers: bench.levers.clone(),
        baseline_captured: None,
        baseline_age_days: None,
        note: None,
    };
    stamp_baseline_age(&mut outcome, &opts.bench_root, bench);
    outcome
}

async fn run_retrieval(bench: &DiscoveredBench, opts: &Opts) -> BenchOutcome {
    // Subprocess into `svrn eval run`. The cli binary is the
    // current executable; assume `current_exe` is the canonical
    // path. Daemon must already be running at localhost:9741 — we
    // surface a clear error if not.
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return outcome_subprocess_fail(bench, format!("tempdir: {e}")),
    };
    let out_json = tmp.path().join("run.json");

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return outcome_subprocess_fail(bench, format!("current_exe: {e}")),
    };

    let limit_str = opts.retrieval_limit.to_string();
    // Bound to the fn scope so &str refs into it survive in cmd_args below.
    let sample_str = opts.sample_questions.map(|n| n.to_string());
    let mut cmd_args: Vec<&str> = vec![
        "eval",
        "run",
        "--bank",
        bench.bench_path.to_str().unwrap_or(""),
        "--limit",
        &limit_str,
        "--output",
        out_json.to_str().unwrap_or(""),
    ];
    if opts.synth {
        cmd_args.push("--synth");
        // Lean-QA sampling is synth-only (eval run ignores it otherwise, but
        // gate here too so the intent is legible at the call site).
        if let Some(s) = sample_str.as_deref() {
            cmd_args.push("--sample-questions");
            cmd_args.push(s);
        }
    }
    if opts.prod_pipeline {
        cmd_args.push("--prod-pipeline");
    }
    if opts.isolate {
        cmd_args.push("--isolate");
    }
    let status = Command::new(&exe).args(&cmd_args).status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return outcome_subprocess_fail(
                bench,
                format!(
                    "`{} eval run --bank {}` exited {}",
                    exe.display(),
                    bench.bench_path.display(),
                    s.code().unwrap_or(-1),
                ),
            )
        }
        Err(e) => return outcome_subprocess_fail(bench, format!("spawn eval run: {e}")),
    }

    let bytes = match std::fs::read(&out_json) {
        Ok(b) => b,
        Err(e) => {
            return outcome_subprocess_fail(bench, format!("read {}: {e}", out_json.display()))
        }
    };
    let current: EvalRun = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => return outcome_subprocess_fail(bench, format!("parse eval run output: {e}")),
    };

    let baseline_view = baseline_bench(bench, opts);
    let baseline: Option<EvalRun> =
        read_latest(&opts.bench_root, &baseline_view).unwrap_or_else(|e| {
            eprintln!(
                "warn: reading {} baseline failed: {e} — treating as first run",
                baseline_view.id
            );
            None
        });

    if opts.update_baseline || baseline.is_none() {
        if let Err(e) = write_dated_and_update_latest(&opts.bench_root, &baseline_view, &current) {
            eprintln!(
                "warn: writing baseline for {}/{} failed: {e}",
                baseline_view.group, baseline_view.id
            );
        }
    }

    // Cross-corpus synth is judged on answer quality (answer-equiv);
    // isolation + bare retrieval are judged on strict source/fact recall
    // (per-corpus integrity). See `mean_score`.
    let gate_on_answer_equiv = opts.synth && !opts.isolate;
    // Answer-equiv is an LLM judge verdict and varies ~±0.05 run-to-run
    // (see book-report variance findings) — a 0.5pt threshold just flags
    // noise. Floor the cross-corpus synth gate so it catches real moves,
    // not jitter. Isolation/retrieval stay at the operator's threshold
    // (strict recall is far more stable). `.max` respects a wider
    // operator-set threshold.
    const SYNTH_ANSWER_EQUIV_MIN_THRESHOLD: f32 = 0.05;
    let effective_threshold = if gate_on_answer_equiv {
        opts.regression_threshold
            .max(SYNTH_ANSWER_EQUIV_MIN_THRESHOLD)
    } else {
        opts.regression_threshold
    };
    let status = match &baseline {
        None => BenchStatus::FirstRun,
        Some(prev) => classify_retrieval(prev, &current, effective_threshold, gate_on_answer_equiv),
    };

    let mut outcome = BenchOutcome {
        id: bench.id.clone(),
        group: bench.group.clone(),
        corpus_id: bench.corpus_id.clone(),
        surface: bench.surface.label().to_string(),
        status,
        enrichment: None,
        retrieval: Some(RetrievalOutcome { current, baseline }),
        levers: bench.levers.clone(),
        baseline_captured: None,
        baseline_age_days: None,
        note: None,
    };
    stamp_baseline_age(&mut outcome, &opts.bench_root, &baseline_view);
    outcome
}

/// Stamp the capture date + age of the baseline directory this
/// outcome was diffed against. After an `--update-baseline` write the
/// age reads 0 — correct by construction (the symlink now points at
/// today's snapshot). First-run / stale outcomes keep `None`.
fn stamp_baseline_age(
    outcome: &mut BenchOutcome,
    bench_root: &Path,
    baseline_view: &DiscoveredBench,
) {
    let dir = super::baselines::baseline_dir_for(bench_root, baseline_view);
    if let Some((captured, age_days)) = super::baselines::baseline_age(&dir) {
        outcome.baseline_captured = Some(captured);
        outcome.baseline_age_days = Some(age_days);
    }
}

fn outcome_subprocess_fail(bench: &DiscoveredBench, msg: String) -> BenchOutcome {
    BenchOutcome {
        id: bench.id.clone(),
        group: bench.group.clone(),
        corpus_id: bench.corpus_id.clone(),
        surface: bench.surface.label().to_string(),
        status: BenchStatus::Stale,
        enrichment: None,
        retrieval: None,
        levers: bench.levers.clone(),
        baseline_captured: None,
        baseline_age_days: None,
        note: Some(msg),
    }
}

/// Classify an enrichment bench's run against its baseline. Walks
/// every catalog-aligned axis present in either side, sums up axis
/// F1 deltas vs the threshold, returns the worst-case status.
fn classify_enrichment(prev: &EvalReport, cur: &EvalReport, threshold: f32) -> BenchStatus {
    let mut regressed = false;
    let mut improved = false;
    let axes: Vec<&String> = cur
        .axis_scores
        .keys()
        .chain(prev.axis_scores.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for axis in axes {
        let cur_f1 = cur
            .axis_scores
            .get(axis)
            .and_then(|s| s.f1())
            .unwrap_or(0.0);
        let prev_f1 = prev
            .axis_scores
            .get(axis)
            .and_then(|s| s.f1())
            .unwrap_or(0.0);
        let delta = cur_f1 - prev_f1;
        if delta < -threshold {
            regressed = true;
        } else if delta > threshold {
            improved = true;
        }
    }
    if regressed {
        BenchStatus::Regressed
    } else if improved {
        BenchStatus::Improved
    } else {
        BenchStatus::Green
    }
}

/// Classify a retrieval bench's run. Compares per-question
/// source_score.ratio + fact_score.ratio averaged across all
/// questions in the bank.
fn classify_retrieval(
    prev: &EvalRun,
    cur: &EvalRun,
    threshold: f32,
    use_answer_equiv: bool,
) -> BenchStatus {
    let prev_mean = mean_score(prev, use_answer_equiv);
    let cur_mean = mean_score(cur, use_answer_equiv);
    let delta = cur_mean - prev_mean;
    if delta < -threshold {
        BenchStatus::Regressed
    } else if delta > threshold {
        BenchStatus::Improved
    } else {
        BenchStatus::Green
    }
}

/// Mean per-question score driving the lane verdict.
///
/// The headline metric is **mode-dependent**:
/// - **Cross-corpus synth** (`use_answer_equiv = true`) gates on
///   *answer quality* — the instructor-mode judge (`judge_fact_score`).
///   With every corpus installed, a chunk from a *different* corpus that
///   correctly answers the question is a UX win, so strict per-corpus
///   source/fact recall is the wrong lens (it flags SEP-vs-wiki
///   cross-talk as a "regression" even when the answer is correct).
/// - **Isolation + bare retrieval** (`use_answer_equiv = false`) gates on
///   strict source + fact recall — the per-corpus *integrity* question:
///   "does this corpus hold and retrieve the facts its queries need?"
///
/// None ratios fall back to 0.0; the answer-equiv path falls back to
/// strict fact recall when the judge wasn't run (`--no-judge`).
fn mean_score(run: &EvalRun, use_answer_equiv: bool) -> f32 {
    if run.results.is_empty() {
        return 0.0;
    }
    let sum: f32 = run
        .results
        .iter()
        .map(|r| {
            if use_answer_equiv {
                // Answer-equiv lives on the synth sub-object
                // (`synth.judge_fact_score`), matching the scoreboard's
                // "answer-equiv [judge]" lever (render.rs). The top-level
                // `judge_fact_score` is a different (retrieval-lane)
                // field and is null under synth. Fall back to strict
                // fact recall when the judge didn't run.
                r.synth
                    .as_ref()
                    .and_then(|s| s.judge_fact_score.as_ref())
                    .and_then(|s| s.ratio)
                    .or(r.fact_score.ratio)
                    .unwrap_or(0.0)
            } else {
                let s = r.source_score.ratio.unwrap_or(0.0);
                let f = r.fact_score.ratio.unwrap_or(0.0);
                (s + f) / 2.0
            }
        })
        .sum();
    sum / run.results.len() as f32
}

fn corpus_state_tag(s: CorpusState) -> &'static str {
    match s {
        CorpusState::Ready => "ready",
        CorpusState::IndexedNoAtlas => "indexed, no atlas",
        CorpusState::Unindexed => "unindexed",
    }
}

fn stale_hint(bench: &DiscoveredBench) -> String {
    match bench.corpus_state {
        CorpusState::Unindexed => format!(
            "corpus `{}` not installed locally. Run `svrn corpus install {}` (or sync from a mesh peer).",
            bench.corpus_id, bench.corpus_id
        ),
        CorpusState::IndexedNoAtlas => format!(
            "corpus `{}` indexed but no atlas. Run `svrn enrich build {}` to extract.",
            bench.corpus_id, bench.corpus_id
        ),
        CorpusState::Ready => format!("corpus `{}` ready but bench errored", bench.corpus_id),
    }
}

fn persist_report(path: &Path, outcomes: &[BenchOutcome]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(outcomes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

fn exit_code_from(outcomes: &[BenchOutcome]) -> i32 {
    let any_red = outcomes
        .iter()
        .any(|o| matches!(o.status, BenchStatus::Regressed | BenchStatus::Stale));
    if any_red {
        1
    } else {
        0
    }
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bench-root" => {
                let v = args.get(i + 1).ok_or("--bench-root requires a path")?;
                opts.bench_root = PathBuf::from(v);
                i += 2;
            }
            "--filter" => {
                let v = args.get(i + 1).ok_or("--filter requires a pattern")?;
                opts.filter = Some(v.clone());
                i += 2;
            }
            "--update-baseline" => {
                opts.update_baseline = true;
                i += 1;
            }
            "--rebuild" => {
                opts.rebuild = true;
                i += 1;
            }
            "--report" => {
                let v = args.get(i + 1).ok_or("--report requires a path")?;
                opts.report = Some(PathBuf::from(v));
                i += 2;
            }
            "--regression-threshold" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--regression-threshold requires a number")?;
                opts.regression_threshold = v
                    .parse::<f32>()
                    .map_err(|e| format!("--regression-threshold: {e}"))?;
                i += 2;
            }
            "--retrieval-limit" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--retrieval-limit requires a number")?;
                opts.retrieval_limit = v
                    .parse::<usize>()
                    .map_err(|e| format!("--retrieval-limit: {e}"))?;
                i += 2;
            }
            "--synth" => {
                opts.synth = true;
                i += 1;
            }
            "--prod-pipeline" => {
                opts.prod_pipeline = true;
                i += 1;
            }
            "--sample-questions" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--sample-questions requires a number")?;
                opts.sample_questions = Some(
                    v.parse::<usize>()
                        .map_err(|e| format!("--sample-questions: {e}"))?,
                );
                i += 2;
            }
            "--routing-only" => {
                opts.routing_only = true;
                i += 1;
            }
            "--isolate" => {
                opts.isolate = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                return Err(format!("unexpected positional argument: {other}"));
            }
        }
    }
    // The three retrieval modes are mutually exclusive — each writes its
    // own baseline family and drives a different code path.
    if opts.prod_pipeline && (opts.synth || opts.routing_only) {
        return Err("--prod-pipeline is mutually exclusive with --synth / --routing-only".into());
    }
    Ok(opts)
}

// Silence unused-import false-positive when ATLAS_DIRNAME isn't
// referenced directly (kept here so future helpers can use it).
#[allow(dead_code)]
const _ATLAS_DIRNAME_REF: &str = ATLAS_DIRNAME;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults() {
        let o = parse_args(&[]).unwrap();
        assert_eq!(o.bench_root, PathBuf::from("sovereign/bench"));
        assert!(o.filter.is_none());
        assert!(!o.update_baseline);
        assert!(o.report.is_none());
        assert!((o.regression_threshold - 0.005).abs() < 1e-6);
    }

    #[test]
    fn parse_args_filter_and_report() {
        let o = parse_args(&[
            "--filter".into(),
            "obsidian".into(),
            "--report".into(),
            "/tmp/r.json".into(),
            "--update-baseline".into(),
        ])
        .unwrap();
        assert_eq!(o.filter.as_deref(), Some("obsidian"));
        assert_eq!(o.report.as_deref(), Some(Path::new("/tmp/r.json")));
        assert!(o.update_baseline);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn parse_args_rebuild_flag() {
        let o = parse_args(&["--rebuild".into()]).unwrap();
        assert!(o.rebuild);
    }
}
