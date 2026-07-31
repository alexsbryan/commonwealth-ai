// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench gate <lane> --report <artifact>` — the baseline-relative
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
//! svrn bench gate chaos-monkey --report chaos.jsonl --update-baseline
//! # gate (every CI run):
//! svrn bench gate chaos-monkey --report chaos.jsonl
//!   → exit 0 if no metric regressed vs baseline (first-run also passes),
//!     exit 1 if a metric regressed.
//! ```

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::baselines::{baseline_dir, read_latest_at, write_dated_and_update_latest_at};
use super::lane_baseline::{diff, render_and_exit_code, LaneBaseline, LaneMetric};
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench gate",
    summary: "Baseline-relative CI gate for the absolute-verdict lanes (chaos-monkey, mechanism-fidelity, multiturn).",
    sections: &[
        HelpSection::Usage(
            "svrn bench gate <lane> --report <artifact> [--bench-root <dir>] [--id <baseline-id>] [--update-baseline] [--regression-threshold <f>] [--prompt-version <v>]",
        ),
        HelpSection::Subcommands(&[
            ("chaos-monkey", "Gate the chaos JSONL on {competence, honesty, hallucination_rate} (+ distractor-evasion / citation-fidelity when the bank has v2 questions)."),
            ("mechanism-fidelity", "Gate the mechanism JSONL on the control Δ̄≈0 witness (+ P1 collapse, informational)."),
            ("multiturn", "Gate the threads JSON on {min first-failure turn, mean fact-recall slope, mean judge coverage}."),
            ("search-gym", "Gate the search-gym JSON on overall pass_rate (web-search judiciousness)."),
            ("knowledge-gym", "Gate the knowledge-gym JSON on overall pass_rate (knowledge_lookup discipline)."),
            ("agent-coding", "Gate the agent-bench JSON on grand_total/max_total score fraction (agentic code loop)."),
            ("governance", "Gate the FR-9 detector report on {precision, recall, f1} (Lane A: tension detection)."),
            ("governance-qa", "Gate the FR-9 QA chaos JSONL on {competence, honesty (RL-2), hallucination_rate (RL-1), dead_law_rate (RL-3)} (Lane B)."),
            ("proxy-qa", "Gate the Proxy Voting QA chaos JSONL on {competence (RL-2: both sides cited), honesty + hallucination_rate (RL-1: no confabulated opposition)} (AC-4/AC-5)."),
            ("faithfulness", "Gate the faithfulness JSONL (bench faithfulness run) on the per-corpus unsupported-claim rate; baseline id = the artifact's corpus (or --id)."),
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
    let mut prompt_version: Option<String> = None;

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
            "--prompt-version" => prompt_version = Some(val!("--prompt-version")),
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
        "chaos-monkey" | "chaos" => {
            chaos_summary(&report).map(|b| ("chaos_monkey", "secret_agent", b))
        }
        "mechanism-fidelity" | "mechanism" | "mf" => {
            mechanism_summary(&report).map(|b| ("mechanism_fidelity", "dev", b))
        }
        "multiturn" | "threads" | "multi-turn" => {
            multiturn_summary(&report).map(|b| ("wikipedia_learn", "threads", b))
        }
        "search-gym" | "search" => search_gym_summary(&report).map(|b| ("search-gym", "ci", b)),
        "knowledge-gym" | "knowledge" => {
            knowledge_gym_summary(&report).map(|b| ("knowledge-gym", "ci", b))
        }
        "agent-coding" | "agent-bench" | "agent" => {
            agent_coding_summary(&report).map(|b| ("agent-coding", "ci", b))
        }
        "governance" | "gov" => {
            governance_summary(&report).map(|b| ("governance", "maple_house", b))
        }
        "governance-qa" | "gov-qa" => {
            governance_qa_summary(&report).map(|b| ("governance", "maple_house_qa", b))
        }
        "proxy-qa" | "proxy" => proxy_qa_summary(&report).map(|b| ("proxy", "exxon_qa", b)),
        // Empty default_id sentinel: the bench id is per-corpus, taken from
        // the artifact itself (or --id) after the match.
        "faithfulness" | "faith" => faithfulness_summary(&report).map(|b| ("faithfulness", "", b)),
        other => {
            eprintln!("error: unknown lane `{other}` (expected chaos-monkey | mechanism-fidelity | multiturn | governance | governance-qa | proxy-qa | faithfulness)");
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

    // Capture fingerprints (P0.1): artifact mtime + stated prompt
    // version, on the current summary whether it becomes the baseline
    // (--update-baseline) or is diffed against one.
    current.fingerprint(&report, prompt_version);

    let id = match id_override.as_deref() {
        Some(id) => id,
        // Per-corpus lanes (empty default_id sentinel) take the bench id
        // from the artifact — a wrong default here would silently diff
        // against another corpus's baseline.
        None if default_id.is_empty() => match current.corpus.as_deref() {
            Some(c) => c,
            None => {
                eprintln!("error: this lane needs --id <corpus> (artifact carried no corpus)");
                return 2;
            }
        },
        None => default_id,
    };
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
                    eprintln!(
                        "       {k} = {:.4} ({:?}, tol {:.4})",
                        m.value, m.direction, m.tolerance
                    );
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
        // Staleness advisory ONLY — gates stay deterministic against
        // the pinned baseline; an old baseline is operator information,
        // not a regression. (The April-30 incident: a HARD lane
        // silently diffed against a six-week-old snapshot.)
        // `SOVEREIGN_BASELINE_AGE_STRICT=1` upgrades over-age to a
        // failing exit for CI contexts that want to force re-minting.
        let mut strict_stale = false;
        if prev.is_some() {
            if let Some((captured, age_days)) = super::baselines::baseline_age(&dir) {
                let max_age = super::baselines::baseline_max_age_days();
                if age_days > max_age {
                    eprintln!(
                        "[gate] ⚠ baseline for {lane} captured {captured} ({age_days}d old, threshold {max_age}d) — \
                         re-mint with --update-baseline once adjudicated"
                    );
                    strict_stale = std::env::var("SOVEREIGN_BASELINE_AGE_STRICT")
                        .ok()
                        .as_deref()
                        == Some("1");
                } else {
                    eprintln!("[gate] baseline for {lane} captured {captured} ({age_days}d old)");
                }
            }
        }
        // Fingerprint advisories: a mtime match means the "current"
        // artifact IS the baseline's artifact — the lane never re-ran,
        // so a green here says nothing new. A prompt-version mismatch
        // means deltas are not attributable to code alone. Both are
        // operator information, never an exit-code change.
        if let Some(p) = prev.as_ref() {
            if p.artifact_mtime.is_some() && p.artifact_mtime == current.artifact_mtime {
                eprintln!(
                    "[gate] ⚠ {lane}: artifact unchanged since baseline capture (mtime match) — \
                     score is static; re-run the lane before trusting this verdict"
                );
            }
            if let (Some(pv), Some(cv)) = (&p.prompt_version, &current.prompt_version) {
                if pv != cv {
                    eprintln!(
                        "[gate] ⚠ {lane}: prompt version changed since baseline capture ({pv} → {cv}) — \
                         deltas are not attributable to code alone"
                    );
                }
            }
        }
        let d = diff(prev.as_ref(), &current);
        let code = render_and_exit_code(&d, lane);
        if code == 0 && strict_stale {
            eprintln!(
                "[gate] FAIL: baseline over age threshold and SOVEREIGN_BASELINE_AGE_STRICT=1"
            );
            return 1;
        }
        code
    }
}

// ── Artifact readers ────────────────────────────────────────────────────────

/// faithfulness: headline = the per-corpus unsupported-claim rate over the
/// lane's judged rows (LowerIsBetter). Guards: zero rows is never a pass;
/// mixed judge tiers taint the rate (the absolute rate moves ~4 points
/// between the fast and primary judges on the same corpus — SP3, pinned in
/// sovereign-eval's seed-file test); one artifact = one corpus (gate each
/// corpus against its own baseline). Upper-level rate (summaries of
/// summaries — the compounding-fabrication signal) is added only when there
/// are enough upper-level claims for the rate to mean anything.
fn faithfulness_summary(report: &Path) -> Result<LaneBaseline, String> {
    use sovereign_eval::faithfulness::{score, ClaimRecord};
    let rows: Vec<ClaimRecord> = read_jsonl(report)?;
    if rows.is_empty() {
        return Err(format!(
            "{}: zero judged claims — nothing verified is not a pass",
            report.display()
        ));
    }
    let reports = score(&rows);
    if reports.len() != 1 {
        return Err(format!(
            "{}: {} corpora in one artifact — run and gate per corpus",
            report.display(),
            reports.len()
        ));
    }
    let rep = &reports[0];
    if rep.judge_models.len() > 1 {
        return Err(format!(
            "{}: mixed judge tiers {:?} — rate not comparable across runs",
            report.display(),
            rep.judge_models
        ));
    }
    let mut b = LaneBaseline::new("faithfulness", now_rfc3339());
    b.corpus = Some(rep.corpus_id.clone());
    b.attribute(rep.judge_models.first().map(String::as_str));
    b.note = Some(format!(
        "{} nodes · {} claims · judge {}",
        rep.n_nodes,
        rep.n_claims,
        rep.judge_models.first().map(String::as_str).unwrap_or("?")
    ));
    // Provisional tolerance pending a spread measurement (re-run variance of
    // the same judge on the same corpus); --regression-threshold overrides.
    let mut b = b.with(
        "unsupported_rate",
        LaneMetric::lower_is_better(rep.unsupported_rate, 0.03),
    );
    let (upper_n, upper_u) = rep
        .per_level
        .iter()
        .filter(|l| l.level >= 1)
        .fold((0usize, 0usize), |(n, u), l| (n + l.n_claims, u + l.n_unsupported));
    if upper_n >= 20 {
        b = b.with(
            "unsupported_rate_upper_levels",
            LaneMetric::lower_is_better(upper_u as f64 / upper_n as f64, 0.06),
        );
    }
    Ok(b)
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: T =
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?;
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

/// governance (FR-9 Lane A): re-read the detector report + lift its
/// precision/recall/F1 as the gated metrics. The detector classifier is not
/// run-to-run deterministic (MoE routing + Metal float), so tolerances are
/// generous (≈ one planted/decoy item of slack over the ~10-tension, ~7-decoy
/// test fixture) — the gate fires on a genuine collapse, not noise.
fn governance_summary(report: &Path) -> Result<LaneBaseline, String> {
    let bytes = std::fs::read(report).map_err(|e| format!("reading {}: {e}", report.display()))?;
    let rep: sovereign_eval::governance_bench::DetectorReport =
        serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", report.display()))?;
    Ok(governance_lane_baseline(&rep, None, None, now_rfc3339()))
}

/// Single source of truth for the governance detector lane's metric set +
/// tolerances (shared by the gate adapter here and any future promote loop).
pub(crate) fn governance_lane_baseline(
    rep: &sovereign_eval::governance_bench::DetectorReport,
    corpus: Option<String>,
    model: Option<String>,
    now: String,
) -> LaneBaseline {
    let mut b = LaneBaseline::new("governance", now);
    b.corpus = corpus;
    b.attribute(model.as_deref());
    b.note = Some(format!(
        "precision {:.2} · recall {:.2} · {} planted found, {} missed · {} flagged pairs",
        rep.overall.precision,
        rep.overall.recall,
        rep.planted_found.len(),
        rep.planted_missed.len(),
        rep.n_detected_pairs,
    ));
    b.with(
        "precision",
        LaneMetric::higher_is_better(rep.overall.precision, 0.15),
    )
    .with(
        "recall",
        LaneMetric::higher_is_better(rep.overall.recall, 0.15),
    )
    .with("f1", LaneMetric::higher_is_better(rep.overall.f1, 0.12))
}

/// governance (FR-9 Lane B): re-score the QA chaos JSONL with the same
/// two-red-line scorer and gate {competence, honesty (RL-2),
/// hallucination_rate (RL-1), dead_law_rate (RL-3)}. The active-set filter
/// + governance gate already shaped the answers at run time (the corpus
/// carries an oplog); this just judges the artifact vs the committed
/// baseline, exactly like the chaos lane.
fn governance_qa_summary(report: &Path) -> Result<LaneBaseline, String> {
    use sovereign_eval::chaos_monkey::{score, ResultRow};
    let rows: Vec<ResultRow> = read_jsonl(report)?;
    let rep = score(&rows);
    let mut b = chaos_lane_baseline(
        &rep,
        rows.first().map(|r| r.corpus.clone()),
        rows.first().map(|r| r.model_id.clone()),
        now_rfc3339(),
    );
    b.lane = "governance-qa".to_string();
    Ok(b)
}

/// Proxy Lane B (AC-4/AC-5): re-score the chaos ResultRow JSONL with the
/// pure two-red-line scorer — competence (RL-2: both sides cited), honesty
/// + hallucination_rate (RL-1: no confabulated opposition on a management
/// item), citation fidelity (AC-5). Identical metric set + tolerances as
/// chaos/governance; only the lane name + fixture differ.
fn proxy_qa_summary(report: &Path) -> Result<LaneBaseline, String> {
    use sovereign_eval::chaos_monkey::{score, ResultRow};
    let rows: Vec<ResultRow> = read_jsonl(report)?;
    let rep = score(&rows);
    let mut b = chaos_lane_baseline(
        &rep,
        rows.first().map(|r| r.corpus.clone()),
        rows.first().map(|r| r.model_id.clone()),
        now_rfc3339(),
    );
    b.lane = "proxy-qa".to_string();
    Ok(b)
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
    // `model` is the transcript's model_id — the concrete GGUF stem
    // when the run resolved it (attributes both `model` and the
    // structured `model_attribution`), or a legacy alias which
    // `attribute` refuses, leaving the baseline honestly unattributed.
    b.attribute(model.as_deref());
    b.note = Some(format!(
        "competence {}/{} answerable correct · honesty {}/{} absent honest · {} fabricated",
        rep.counts.answerable_correct,
        rep.counts.answerable,
        rep.counts.absent_honest,
        rep.counts.absent,
        rep.counts.absent_hallucinated,
    ));
    let mut b = b
        .with(
            "competence",
            LaneMetric::higher_is_better(rep.competence, 0.15),
        )
        .with("honesty", LaneMetric::higher_is_better(rep.honesty, 0.18))
        .with(
            "hallucination_rate",
            LaneMetric::lower_is_better(rep.hallucination_rate, 0.18),
        );
    // chaos v2 — only present once the bank ships distractor / provenance_trap
    // questions (otherwise the scorer returns NaN for an empty population). The
    // `.is_finite()` guard keeps this additive: zero effect on the flywheel's
    // promote loop or the v1 baseline until such questions exist, then both the
    // CI gate and promote pick them up automatically (one shared metric set).
    // citation_fidelity fires only on supporting-quote probes (provenance_trap),
    // so n is tiny and a single flip is 1/n (n=3 ⇒ ±0.33). Gate it ONLY at a
    // stable sample size; below that it's still printed in the scoreboard WITH
    // its n, but it can't count as a regression. The broad, stable faithfulness
    // gate is `grounding_fidelity` below.
    const MIN_CITATION_N: usize = 8;
    if rep.citation_fidelity.is_finite() && rep.n_citation_checked >= MIN_CITATION_N {
        b = b.with(
            "citation_fidelity",
            LaneMetric::higher_is_better(rep.citation_fidelity, 0.30),
        );
    }
    // grounding_fidelity — the stable faithfulness gate: of every asserted
    // specific, the fraction grounded in the evidence (n ≈ all answered probes),
    // so it doesn't swing on a single item. Tol 0.15 ≈ a few-item move.
    if rep.grounding_fidelity.is_finite() {
        b = b.with(
            "grounding_fidelity",
            LaneMetric::higher_is_better(rep.grounding_fidelity, 0.15),
        );
    }
    if rep.distractor_evasion.is_finite() {
        // Answer-echo proxy (did the answer parrot the wrong passage). This is a
        // coarse substring proxy — the real check is the FUTURE_RESEARCH
        // grounding verifier — so it's gated loosely (0.34 ≈ one flip over the
        // ~3 distractor questions); it fires only on a clear multi-item
        // collapse, not generation noise.
        b = b.with(
            "distractor_evasion",
            LaneMetric::higher_is_better(rep.distractor_evasion, 0.34),
        );
    }
    // FR-9 RL-3 (governance dead-law) — present only once the bank ships
    // SupersededTrap questions (else NaN for an empty population). Additive
    // like citation/distractor: zero effect on existing chaos banks; both
    // the CI gate and the promote loop pick it up automatically when a
    // governance bank introduces superseded traps. Tolerance 0.30 ≈ one
    // flip over the ~3-4 superseded-trap questions.
    if rep.dead_law_rate.is_finite() {
        b = b.with(
            "dead_law_rate",
            LaneMetric::lower_is_better(rep.dead_law_rate, 0.30),
        );
    }
    b
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
        .with(
            "control_p1_delta",
            LaneMetric::near_zero(mean(&control_p1), 0.05),
        )
        // Informational: a faithful model's P1 collapse is strongly negative;
        // a *rise* toward zero means worse fidelity. Generous tolerance so the
        // non-gating signal doesn't flake the build.
        .with(
            "p1_collapse_delta",
            LaneMetric::lower_is_better(mean(&p1_collapse), 0.15),
        ))
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
    let slopes: Vec<f64> = run
        .threads
        .iter()
        .map(|t| t.degradation.fact_recall_slope)
        .collect();
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
        .with(
            "min_first_failure_turn",
            LaneMetric::higher_is_better(min_fft, 0.5),
        )
        .with(
            "mean_fact_recall_slope",
            LaneMetric::higher_is_better(mean(&slopes), 0.05),
        );
    if !coverages.is_empty() {
        b = b.with(
            "mean_judge_coverage",
            LaneMetric::higher_is_better(mean(&coverages), 0.10),
        );
    }
    Ok(b)
}

// ── Tool-use / agentic gym adapters ─────────────────────────────────────────
//
// The gyms (search-gym, knowledge-gym, agent-bench) are owned by other code and
// emit their own JSON. Rather than couple to their structs, we read the report
// as a serde Value and pull just the headline scalar by key — robust to gym
// schema churn. Rates are normalised to 0..1 (some gyms emit a 0..100 percent).

fn read_json_value(report: &Path) -> Result<serde_json::Value, String> {
    let text =
        std::fs::read_to_string(report).map_err(|e| format!("read {}: {e}", report.display()))?;
    // The gyms print their JSON to stdout, possibly after a human-readable
    // preamble and/or with a trailing summary line. Skip to the first `{`/`[`
    // and read exactly one JSON value (StreamDeserializer ignores trailing
    // bytes), so a `… > out.json` capture that isn't pristine JSON still parses.
    let start = text
        .find(['{', '['])
        .ok_or_else(|| format!("{}: no JSON found", report.display()))?;
    let mut stream =
        serde_json::Deserializer::from_str(&text[start..]).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(v)) => Ok(v),
        Some(Err(e)) => Err(format!("parse {}: {e}", report.display())),
        None => Err(format!("{}: no JSON value", report.display())),
    }
}

fn get_f64(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(serde_json::Value::as_f64)
}

/// Normalise a pass-rate that may be expressed as a fraction (0..1) or a
/// percentage (0..100) into 0..1. A value above 1.5 is treated as a percent.
fn norm_rate(x: f64) -> f64 {
    if x > 1.5 {
        x / 100.0
    } else {
        x
    }
}

/// search-gym: web-search judiciousness (search only when needed; cite from
/// results, not training). Headline = overall pass rate across the sampled
/// fixtures × replays. Tolerance 0.15: the CI samples ~4 hardest fixtures × 5
/// replays (~20 runs, ~0.05/flip) and the live chat path is not deterministic,
/// so absorb ~3 flips; a real regression drops the rate further.
fn search_gym_summary(report: &Path) -> Result<LaneBaseline, String> {
    let v = read_json_value(report)?;
    let rate = get_f64(&v, "total_rate")
        .map(norm_rate)
        .ok_or_else(|| format!("{}: no `total_rate` in search-gym report", report.display()))?;
    let mut b = LaneBaseline::new("search-gym", now_rfc3339());
    if let (Some(p), Some(r)) = (get_f64(&v, "total_pass"), get_f64(&v, "total_run")) {
        b.note = Some(format!("{p:.0}/{r:.0} replays passed"));
    }
    Ok(b.with("pass_rate", LaneMetric::higher_is_better(rate, 0.15)))
}

/// knowledge-gym: knowledge_lookup tool discipline — corpus-vs-web escalation,
/// citation faithfulness, multi-turn cache. Headline = overall pass rate.
/// Tolerance 0.20: the CI samples ~3 hardest fixtures × 3 replays (~9 runs,
/// ~0.11/flip), so a small-n flip stays under the gate; a regression needs ≥2.
fn knowledge_gym_summary(report: &Path) -> Result<LaneBaseline, String> {
    let v = read_json_value(report)?;
    let rate = get_f64(&v, "pass_rate").map(norm_rate).ok_or_else(|| {
        format!(
            "{}: no `pass_rate` in knowledge-gym report",
            report.display()
        )
    })?;
    let mut b = LaneBaseline::new("knowledge-gym", now_rfc3339());
    if let (Some(p), Some(r)) = (get_f64(&v, "total_passes"), get_f64(&v, "total_replays")) {
        b.note = Some(format!("{p:.0}/{r:.0} replays passed"));
    }
    Ok(b.with("pass_rate", LaneMetric::higher_is_better(rate, 0.20)))
}

/// agent-coding: end-to-end agentic code loop (plan→implement→test→iterate).
/// Headline = grand_total / max_total as a 0..1 score fraction over the sampled
/// hardest problems. Tolerance 0.12 ≈ 3 of the 27 max points across 3 problems
/// — agentic + judge variance is high, so the gate fires only on a real drop
/// (e.g. a problem that used to complete now hitting a token/loop exit).
fn agent_coding_summary(report: &Path) -> Result<LaneBaseline, String> {
    let v = read_json_value(report)?;
    let grand = get_f64(&v, "grand_total").ok_or_else(|| {
        format!(
            "{}: no `grand_total` in agent-bench report",
            report.display()
        )
    })?;
    let max = get_f64(&v, "max_total").filter(|m| *m > 0.0).unwrap_or(1.0);
    let frac = grand / max;
    let mut b = LaneBaseline::new("agent-coding", now_rfc3339());
    b.model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
    b.note = Some(format!("grand_total {grand:.0}/{max:.0}"));
    Ok(b.with("score_fraction", LaneMetric::higher_is_better(frac, 0.12)))
}
