//! `sovereign bench gate <lane> --report <artifact>` — the baseline-relative
//! CI gate for the *absolute-verdict* lanes.
//!
//! Three lanes in the CI suite (chaos-monkey, mechanism-fidelity, the
//! multi-turn degradation threads) carry verdicts that are true findings for
//! the *current* system rather than regression signals — so gating CI on their
//! own pass/fail would pin the build red forever (see
//! `scripts/sovereign-ci-bench.sh`). This command promotes them to honest
//! gates: it reads the artifact the lane *already wrote*, recomputes its
//! headline scalars with the lane's *own* pure scorer, and diffs them against a
//! committed baseline. It fails only when a metric moved the wrong way past its
//! tolerance.
//!
//! Separation of concerns: the orchestrators *measure* (and keep their absolute
//! glassbox verdict); this command *judges vs baseline*. All the baseline logic
//! lives here + in [`super::lane_baseline`] — the orchestrators are untouched.
//!
//! ```text
//! # capture (once, on a healthy daemon):
//! sovereign bench gate chaos-monkey --report chaos.jsonl --update-baseline
//! # gate (every CI run):
//! sovereign bench gate chaos-monkey --report chaos.jsonl
//!   → exit 0 if no metric regressed vs baseline (first-run also passes),
//!     exit 1 if a metric regressed.
//! ```

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::baselines::{baseline_dir, read_latest_at, write_dated_and_update_latest_at};
use super::lane_baseline::{diff, render_and_exit_code, LaneBaseline, LaneMetric};
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench gate",
    summary: "Baseline-relative CI gate for the absolute-verdict lanes (chaos-monkey, mechanism-fidelity, multiturn).",
    sections: &[
        HelpSection::Usage(
            "sovereign bench gate <lane> --report <artifact> [--bench-root <dir>] [--id <baseline-id>] [--update-baseline] [--regression-threshold <f>]",
        ),
        HelpSection::Subcommands(&[
            ("chaos-monkey", "Gate the chaos JSONL on {competence, honesty, hallucination_rate}."),
            ("mechanism-fidelity", "Gate the mechanism JSONL on the control Δ̄≈0 witness (+ P1 collapse, informational)."),
            ("multiturn", "Gate the threads JSON on {min first-failure turn, mean fact-recall slope, mean judge coverage}."),
        ]),
        HelpSection::Notes(
            "The lane's own absolute verdict (e.g. chaos NO-GO) stays advisory; this gate fails ONLY on regression vs the committed baseline at <bench-root>/<group>/baselines/<id>/latest.json. First-run (no baseline) passes — capture one with --update-baseline.",
        ),
    ],
};

/// `bench gate` entry point. Synchronous — all work is file IO + arithmetic.
pub fn cmd_gate(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    let lane = args[0].as_str();
    let rest = &args[1..];

    let mut report: Option<PathBuf> = None;
    let mut bench_root = PathBuf::from("sovereign/bench");
    let mut id_override: Option<String> = None;
    let mut update_baseline = false;
    let mut threshold: Option<f64> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i) {
                Some(v) => v.clone(),
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--report" => report = Some(PathBuf::from(val!("--report"))),
            "--bench-root" => bench_root = PathBuf::from(val!("--bench-root")),
            "--id" => id_override = Some(val!("--id")),
            "--update-baseline" => update_baseline = true,
            "--regression-threshold" => {
                threshold = Some(match val!("--regression-threshold").parse() {
                    Ok(f) => f,
                    Err(_) => {
                        eprintln!("error: --regression-threshold must be a float");
                        return 2;
                    }
                })
            }
            "--help" | "-h" => {
                help::print(&HELP);
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    let Some(report) = report else {
        eprintln!("error: --report <artifact> is required");
        help::print(&HELP);
        return 2;
    };

    // Build the current run's headline metrics from the lane's own artifact.
    let built = match lane {
        "chaos-monkey" | "chaos" => chaos_summary(&report).map(|b| ("chaos_monkey", "secret_agent", b)),
        "mechanism-fidelity" | "mechanism" | "mf" => {
            mechanism_summary(&report).map(|b| ("mechanism_fidelity", "dev", b))
        }
        "multiturn" | "threads" | "multi-turn" => {
            multiturn_summary(&report).map(|b| ("wikipedia_learn", "threads", b))
        }
        other => {
            eprintln!("error: unknown lane `{other}` (expected chaos-monkey | mechanism-fidelity | multiturn)");
            return 2;
        }
    };
    let (group, default_id, mut current) = match built {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // A uniform --regression-threshold override replaces every metric's
    // per-metric tolerance (escape hatch; default keeps the adapter's tuned
    // tolerances, which differ by metric — see the adapters below).
    if let Some(t) = threshold {
        for m in current.metrics.values_mut() {
            m.tolerance = t;
        }
    }

    let id = id_override.as_deref().unwrap_or(default_id);
    let dir = baseline_dir(&bench_root, group, id);

    if update_baseline {
        match write_dated_and_update_latest_at(&dir, &current) {
            Ok(path) => {
                eprintln!(
                    "[gate] captured baseline for {lane} ({} metrics) → {}",
                    current.metrics.len(),
                    path.display()
                );
                for (k, m) in &current.metrics {
                    eprintln!("       {k} = {:.4} ({:?}, tol {:.4})", m.value, m.direction, m.tolerance);
                }
                0
            }
            Err(e) => {
                eprintln!("error: could not write baseline to {}: {e}", dir.display());
                1
            }
        }
    } else {
        let prev: Option<LaneBaseline> = match read_latest_at(&dir) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: could not read baseline at {}: {e}", dir.display());
                return 2;
            }
        };
        let d = diff(prev.as_ref(), &current);
        render_and_exit_code(&d, lane)
    }
}

// ── Artifact readers ────────────────────────────────────────────────────────

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: T = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?;
        rows.push(row);
    }
    if rows.is_empty() {
        return Err(format!("{} has no rows", path.display()));
    }
    Ok(rows)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn mean(xs: &[f64]) -> f64 {
    let finite: Vec<f64> = xs.iter().copied().filter(|x| x.is_finite()).collect();
    if finite.is_empty() {
        f64::NAN
    } else {
        finite.iter().sum::<f64>() / finite.len() as f64
    }
}

// ── Lane adapters: artifact → headline LaneBaseline ─────────────────────────

/// chaos-monkey: re-score the JSONL with the bench's own two-red-line scorer.
///
/// Tolerance model: `tol ≈ (items of allowed noise) / population`. The bank has
/// ~7 answerable and ~11 absent, and the agent is **not** run-to-run
/// deterministic even at temperature 0 (MoE routing + Metal float). Two clean
/// idle-daemon runs of this exact bank differed by ~2 honesty items
/// (0.36 ↔ 0.55) — the earlier, lower one was captured under concurrent CI
/// load on a churning daemon. So:
///   - competence (n≈7): 0.15 ≈ one item of slack.
///   - honesty / hallucination (n≈11): 0.18 ≈ two items, covering the observed
///     swing so the gate fires only on a genuine ≥3-item collapse, not noise.
/// (Treated as a pre-registration event — see chaos_monkey/manifest.toml. The
/// CI suite runs on a healthy/idle daemon, which tightens the real variance.)
fn chaos_summary(report: &Path) -> Result<LaneBaseline, String> {
    use sovereign_eval::chaos_monkey::{score, ResultRow};
    let rows: Vec<ResultRow> = read_jsonl(report)?;
    let rep = score(&rows);
    Ok(chaos_lane_baseline(
        &rep,
        rows.first().map(|r| r.corpus.clone()),
        rows.first().map(|r| r.model_id.clone()),
        now_rfc3339(),
    ))
}

/// Build the chaos lane's headline metrics from an already-scored report. The
/// single source of truth for the two-red-line metric set + tolerances —
/// shared by the gate adapter (re-scores a JSONL artifact) and the
/// [`super::promote`] controller (scores its arms in-memory), so the CI gate
/// and the promotion loop can never disagree on what "better" means.
///
/// Tolerance model: `tol ≈ (items of allowed noise) / population` (n≈7
/// answerable, n≈11 absent; the agent is not run-to-run deterministic even at
/// temp 0). See `chaos_summary`'s history note.
pub(crate) fn chaos_lane_baseline(
    rep: &sovereign_eval::chaos_monkey::CalibrationReport,
    corpus: Option<String>,
    model: Option<String>,
    now: String,
) -> LaneBaseline {
    let mut b = LaneBaseline::new("chaos-monkey", now);
    b.corpus = corpus;
    b.model = model;
    b.note = Some(format!(
        "competence {}/{} answerable correct · honesty {}/{} absent declined · {} hallucinated",
        rep.counts.answerable_correct,
        rep.counts.answerable,
        rep.counts.absent_abstained,
        rep.counts.absent,
        rep.counts.absent_hallucinated,
    ));
    b.with("competence", LaneMetric::higher_is_better(rep.competence, 0.15))
        .with("honesty", LaneMetric::higher_is_better(rep.honesty, 0.18))
        .with("hallucination_rate", LaneMetric::lower_is_better(rep.hallucination_rate, 0.18))
}

/// mechanism-fidelity: the gating metric is the **control Δ̄≈0 witness** — the
/// mean signed `d_agent` over the stripped-render P1 control rows. If it drifts
/// from zero the forced-choice scoring join broke (a real instrument
/// regression), independent of the model's GO/NO-GO verdict. P1 collapse is
/// tracked too but with a generous tolerance (informational; the verdict is
/// not the gate). Filters mirror `mechanism_fidelity::print_glassbox_summary`.
fn mechanism_summary(report: &Path) -> Result<LaneBaseline, String> {
    use sovereign_eval::mechanism_fidelity::ResultRow;
    let rows: Vec<ResultRow> = read_jsonl(report)?;

    let control_p1: Vec<f64> = rows
        .iter()
        .filter(|r| r.variant == "dir_p1" && r.control)
        .map(|r| r.d_agent)
        .collect();
    let p1_collapse: Vec<f64> = rows
        .iter()
        .filter(|r| r.variant == "dir_p1" && !r.control && !r.paraphrase)
        .map(|r| r.d_agent)
        .collect();

    if control_p1.is_empty() {
        return Err(format!(
            "{}: no dir_p1 control rows — cannot establish the scoring-join witness",
            report.display()
        ));
    }

    let mut models: Vec<String> = rows.iter().map(|r| r.model_id.clone()).collect();
    models.sort();
    models.dedup();

    let mut b = LaneBaseline::new("mechanism-fidelity", now_rfc3339());
    b.model = Some(models.join(","));
    b.note = Some(format!(
        "pool={} · control Δ̄ over {} stripped-P1 rows · P1 collapse over {} full-P1 rows",
        rows.first().map(|r| r.pool.as_str()).unwrap_or("?"),
        control_p1.len(),
        p1_collapse.len(),
    ));
    Ok(b
        // The witness: must stay near zero. Tight tolerance — this is
        // deterministic (one forced-choice forward pass), not a sampled mean.
        .with("control_p1_delta", LaneMetric::near_zero(mean(&control_p1), 0.05))
        // Informational: a faithful model's P1 collapse is strongly negative;
        // a *rise* toward zero means worse fidelity. Generous tolerance so the
        // non-gating signal doesn't flake the build.
        .with("p1_collapse_delta", LaneMetric::lower_is_better(mean(&p1_collapse), 0.15)))
}

/// multiturn degradation: aggregate the per-thread degradation curve. The worst
/// thread's first-failure turn (earlier = worse) and the mean fact-recall slope
/// (more negative = worse) are the headline signals; judge coverage rides along
/// when the run had a judge.
fn multiturn_summary(report: &Path) -> Result<LaneBaseline, String> {
    use crate::eval_cmd::runner_threads::ThreadEvalRun;
    let text =
        std::fs::read_to_string(report).map_err(|e| format!("read {}: {e}", report.display()))?;
    let run: ThreadEvalRun =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", report.display()))?;
    if run.threads.is_empty() {
        return Err(format!("{}: no threads in the run", report.display()));
    }

    // first-failure: None ("survived all turns") maps to turns.len() so a
    // perfect thread scores best; we then take the worst (min) across threads.
    let min_fft = run
        .threads
        .iter()
        .map(|t| t.degradation.first_failure_turn.unwrap_or(t.turns.len()) as f64)
        .fold(f64::INFINITY, f64::min);
    let slopes: Vec<f64> = run.threads.iter().map(|t| t.degradation.fact_recall_slope).collect();
    let coverages: Vec<f64> = run
        .threads
        .iter()
        .filter_map(|t| t.judge.as_ref().and_then(|j| j.coverage.ratio))
        .map(|r| r as f64)
        .collect();

    let mut b = LaneBaseline::new("multiturn", now_rfc3339());
    b.corpus = Some(run.corpus.clone());
    b.note = Some(format!(
        "{} threads · bank={} · judge coverage on {}/{} threads",
        run.threads.len(),
        run.bank,
        coverages.len(),
        run.threads.len(),
    ));
    b = b
        .with("min_first_failure_turn", LaneMetric::higher_is_better(min_fft, 0.5))
        .with("mean_fact_recall_slope", LaneMetric::higher_is_better(mean(&slopes), 0.05));
    if !coverages.is_empty() {
        b = b.with("mean_judge_coverage", LaneMetric::higher_is_better(mean(&coverages), 0.10));
    }
    Ok(b)
}
