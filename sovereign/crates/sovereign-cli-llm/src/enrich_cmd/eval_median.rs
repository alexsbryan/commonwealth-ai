// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign enrich eval-median <corpus> <golden> --runs N` —
//! variance-aware version of `enrich eval`.
//!
//! Single-run F1 numbers are noisy: a temperature-driven LLM emits
//! different atoms each pass, especially on borderline concepts.
//! When a tuning iteration shows a 5-point F1 swing, it can be hard
//! to tell whether the change was real signal or run-to-run noise.
//! This harness re-runs the pipeline N times against an
//! already-initialised corpus and reports the **median** F1 per
//! phase, alongside min/max so the spread is visible.
//!
//! Each run follows the same shape:
//!
//!   1. `enrich reset <corpus> --full --yes` — clear cache + atlas
//!      + runs/, but keep `chapters.json`, `config.json`, and the
//!      materialised source.txt.
//!   2. `enrich build <corpus> --full --skip seed` — full pipeline.
//!      `--skip seed` mirrors what the philosophy templates need
//!      since Stage 1a tends to JSON-malform on Gemma-4B.
//!   3. Score the resolved atlas via `eval::score_corpus()`.
//!
//! After N runs, per-phase F1 vectors are aggregated:
//!
//!     phase             min     median    max     spread
//!     person atoms      87.5%   100.0%   100.0%   12.5pp
//!     concept atoms     50.0%    66.7%    66.7%   16.7pp
//!     fault lines (P6)    —        —        —      —
//!     ...
//!
//! Spread is `max − min` in percentage points. A wide spread on a
//! phase that's the load-bearing signal for a prompt change is the
//! cue that a single-run delta wasn't trustworthy.
//!
//! Cost is linear in `--runs`. Default is 3 — three runs is the
//! minimum that lets median be meaningfully different from
//! min/max.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::config::EnrichConfig;
use super::eval::{score_corpus, EvalReport, PhaseFilter, PhaseScore};
use super::{build, paths};
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich eval-median",
    summary: "Run the enrichment pipeline N times against an initialised corpus and report median F1 per phase.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich eval-median <corpus-id> <golden-set-path> \\\n  [--runs <N>] [--phase positions|atoms|fault-lines|gaps|configurations|all] \\\n  [--report <json-path>] [--keep-state]",
        ),
        HelpSection::Flags(&[
            ("--runs <N>", "Number of full pipeline runs to aggregate. Default: 3."),
            (
                "--phase <id>",
                "Restrict scoring to one phase (same vocabulary as `enrich eval`). Default: all.",
            ),
            (
                "--report <path>",
                "Write the aggregated JSON report to this path.",
            ),
            (
                "--keep-state",
                "Do not run `enrich reset --full` between runs. Use when you want to score the same atlas repeatedly (mostly useful for testing this command itself).",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich eval-median fwd-test bench/philosophy/free-will-debate.toml",
                "Three runs (~5-7 min each on Gemma-4B), per-phase median F1.",
            ),
            (
                "sovereign enrich eval-median fwd-test bench/philosophy/free-will-debate.toml --runs 5 --report /tmp/median.json",
                "Five runs for tighter spread estimates; persist aggregated report.",
            ),
        ]),
        HelpSection::Notes(
            "The corpus must already be initialised (`enrich init` first). Each run does reset → build → score, so cost is roughly N × the cost of one `enrich build`. Stage 1a (seed) is skipped automatically because the philosophy pipeline's seed prompt regularly fails on smaller chat models — match that contract here so the harness stays useful for prompt-tuning iteration.",
        ),
    ],
};

pub async fn cmd_eval_median(args: &[String]) -> i32 {
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

    if EnrichConfig::require(&parsed.corpus_id).is_err() {
        eprintln!(
            "error: no enrichment config for corpus '{}' — run `sovereign enrich init {}` first",
            parsed.corpus_id, parsed.corpus_id
        );
        return 1;
    }

    println!();
    println!(
        "  Median-of-{} eval — corpus {} against {}",
        parsed.runs,
        parsed.corpus_id,
        parsed.golden_path.display()
    );
    println!();

    let mut runs: Vec<EvalReport> = Vec::with_capacity(parsed.runs);
    let mut durations: Vec<Duration> = Vec::with_capacity(parsed.runs);

    for i in 0..parsed.runs {
        let run_no = i + 1;
        println!("  ── Run {run_no}/{} ──", parsed.runs);
        let started = Instant::now();

        if !parsed.keep_state {
            if let Err(e) = clear_run_state(&parsed.corpus_id) {
                eprintln!("  ✗ run {run_no}: clearing run state failed: {e}");
                return 1;
            }
        }

        let build_args = vec![
            parsed.corpus_id.clone(),
            "--full".into(),
            "--skip".into(),
            "seed".into(),
        ];
        let code = build::cmd_build(&build_args).await;
        if code != 0 {
            eprintln!("  ✗ run {run_no}: `enrich build` exited with code {code}");
            return code;
        }

        match score_corpus(&parsed.corpus_id, &parsed.golden_path, parsed.phase) {
            Ok(report) => {
                let elapsed = started.elapsed();
                println!(
                    "  ✓ run {run_no} scored ({:.1}s; aggregate F1 {})",
                    elapsed.as_secs_f32(),
                    fmt_pct(aggregate_f1(&report))
                );
                runs.push(report);
                durations.push(elapsed);
            }
            Err(e) => {
                eprintln!("  ✗ run {run_no}: scoring failed: {e}");
                return 1;
            }
        }
        println!();
    }

    let aggregated = aggregate(&runs);
    print_text_report(&aggregated, &durations);

    if let Some(path) = parsed.report_path.as_ref() {
        match write_json_report(path, &aggregated) {
            Ok(_) => println!("\n  ✓ wrote {}", path.display()),
            Err(e) => {
                eprintln!("error: writing report {}: {e}", path.display());
                return 1;
            }
        }
    }

    0
}

/// Wipe everything the pipeline regenerates — phase caches, run
/// records, the resolved atlas, the field skeleton — while
/// preserving anything the operator authored or pinned at init time:
/// `config.json`, `source.txt`, `chapters.json`, and the per-phase
/// `exemplars/` bank. Idempotent: missing paths are silently skipped.
fn clear_run_state(corpus_id: &str) -> std::io::Result<()> {
    use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;

    let cache = paths::cache_dir(corpus_id);
    let runs = paths::runs_dir(corpus_id);
    let atlas = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
    let skeleton = paths::index_root(corpus_id).join("field_skeleton.json");

    for dir in [&cache, &runs, &atlas] {
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
    }
    if skeleton.exists() {
        std::fs::remove_file(&skeleton)?;
    }
    // Recreate the dirs build expects to find.
    std::fs::create_dir_all(&cache)?;
    std::fs::create_dir_all(&runs)?;
    Ok(())
}

// ── Argument parsing ───────────────────────────────────────────────

#[derive(Debug)]
struct ParsedMedian {
    corpus_id: String,
    golden_path: PathBuf,
    runs: usize,
    phase: PhaseFilter,
    report_path: Option<PathBuf>,
    keep_state: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedMedian, String> {
    let mut corpus_id: Option<String> = None;
    let mut golden_path: Option<PathBuf> = None;
    let mut runs: usize = 3;
    let mut phase = PhaseFilter::All;
    let mut report_path: Option<PathBuf> = None;
    let mut keep_state = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--runs" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--runs requires a value".to_string())?;
                runs = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--runs must be a positive integer: {e}"))?;
                if runs == 0 {
                    return Err("--runs must be ≥ 1".to_string());
                }
                i += 2;
            }
            "--phase" => {
                phase = PhaseFilter::parse(
                    args.get(i + 1)
                        .ok_or("--phase requires a value".to_string())?,
                )?;
                i += 2;
            }
            "--report" => {
                report_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--report requires a path".to_string())?,
                ));
                i += 2;
            }
            "--keep-state" => {
                keep_state = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if golden_path.is_none() {
                    golden_path = Some(PathBuf::from(other));
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                i += 1;
            }
        }
    }
    Ok(ParsedMedian {
        corpus_id: corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?,
        golden_path: golden_path.ok_or_else(|| "missing <golden-set-path>".to_string())?,
        runs,
        phase,
        report_path,
        keep_state,
    })
}

// ── Aggregation ────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
struct PhaseSummary {
    /// Per-run F1 in `[0.0, 1.0]`. `None` for runs where the phase
    /// produced no scoreable artefacts. Length always equals
    /// `runs.len()`.
    f1s: Vec<Option<f32>>,
    /// Per-run match counts (`matched / expected`) for the human
    /// breakdown.
    match_counts: Vec<(usize, usize)>,
    /// Total forbidden hits across runs — surfaces when the model
    /// occasionally produces a forbidden atom even if the median
    /// run avoids it.
    forbidden_hits: usize,
    /// Notes from any run (deduplicated). A note like
    /// "field_skeleton.json not present" appearing across all runs
    /// vs only one is itself signal.
    notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct AggregatedReport {
    corpus_id: String,
    golden_path: String,
    runs: usize,
    positions: PhaseSummary,
    person_atoms: PhaseSummary,
    concept_atoms: PhaseSummary,
    work_atoms: PhaseSummary,
    event_atoms: PhaseSummary,
    state_atoms: PhaseSummary,
    relation_atoms: PhaseSummary,
    question_atoms: PhaseSummary,
    claim_atoms: PhaseSummary,
    fault_lines: PhaseSummary,
    open_questions: PhaseSummary,
    configurations: PhaseSummary,
    /// Per-run aggregate F1 across all scoreable phases (mirrors
    /// what `enrich eval` prints at the bottom).
    aggregate_f1s: Vec<Option<f32>>,
}

fn aggregate(runs: &[EvalReport]) -> AggregatedReport {
    let mut a = AggregatedReport {
        corpus_id: runs
            .first()
            .map(|r| r.corpus_id.clone())
            .unwrap_or_default(),
        golden_path: runs
            .first()
            .map(|r| r.golden_path.clone())
            .unwrap_or_default(),
        runs: runs.len(),
        ..Default::default()
    };
    for r in runs {
        push_phase(&mut a.positions, r.positions.as_ref());
        push_phase(&mut a.person_atoms, r.person_atoms.as_ref());
        push_phase(&mut a.concept_atoms, r.concept_atoms.as_ref());
        push_phase(&mut a.work_atoms, r.work_atoms.as_ref());
        push_phase(&mut a.event_atoms, r.event_atoms.as_ref());
        push_phase(&mut a.state_atoms, r.state_atoms.as_ref());
        push_phase(&mut a.relation_atoms, r.relation_atoms.as_ref());
        push_phase(&mut a.question_atoms, r.question_atoms.as_ref());
        push_phase(&mut a.claim_atoms, r.claim_atoms.as_ref());
        push_phase(&mut a.fault_lines, r.fault_lines.as_ref());
        push_phase(&mut a.open_questions, r.open_questions.as_ref());
        push_phase(&mut a.configurations, r.configurations.as_ref());
        a.aggregate_f1s.push(aggregate_f1(r));
    }
    a
}

fn push_phase(summary: &mut PhaseSummary, score: Option<&PhaseScore>) {
    match score {
        None => {
            summary.f1s.push(None);
            summary.match_counts.push((0, 0));
        }
        Some(s) => {
            summary.f1s.push(s.f1());
            summary.match_counts.push((s.matched, s.expected));
            summary.forbidden_hits += s.forbidden_hit;
            for n in &s.notes {
                if !summary.notes.contains(n) {
                    summary.notes.push(n.clone());
                }
            }
        }
    }
}

fn aggregate_f1(r: &EvalReport) -> Option<f32> {
    let phase_f1s: Vec<f32> = [
        r.positions.as_ref().and_then(|s| s.f1()),
        r.person_atoms.as_ref().and_then(|s| s.f1()),
        r.concept_atoms.as_ref().and_then(|s| s.f1()),
        r.work_atoms.as_ref().and_then(|s| s.f1()),
        r.event_atoms.as_ref().and_then(|s| s.f1()),
        r.state_atoms.as_ref().and_then(|s| s.f1()),
        r.relation_atoms.as_ref().and_then(|s| s.f1()),
        r.question_atoms.as_ref().and_then(|s| s.f1()),
        r.claim_atoms.as_ref().and_then(|s| s.f1()),
        r.fault_lines.as_ref().and_then(|s| s.f1()),
        r.open_questions.as_ref().and_then(|s| s.f1()),
        r.configurations.as_ref().and_then(|s| s.f1()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if phase_f1s.is_empty() {
        return None;
    }
    Some(phase_f1s.iter().sum::<f32>() / phase_f1s.len() as f32)
}

// ── Statistics ─────────────────────────────────────────────────────

fn min_median_max(values: &[Option<f32>]) -> Option<(f32, f32, f32)> {
    let defined: Vec<f32> = values.iter().filter_map(|v| *v).collect();
    if defined.is_empty() {
        return None;
    }
    let mut sorted = defined.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min = *sorted.first().unwrap();
    let max = *sorted.last().unwrap();
    let median = if sorted.len() % 2 == 1 {
        sorted[sorted.len() / 2]
    } else {
        let lo = sorted[sorted.len() / 2 - 1];
        let hi = sorted[sorted.len() / 2];
        (lo + hi) / 2.0
    };
    Some((min, median, max))
}

// ── Reporting ──────────────────────────────────────────────────────

fn fmt_pct(v: Option<f32>) -> String {
    match v {
        None => "  —  ".to_string(),
        Some(x) => format!("{:>5.1}%", x * 100.0),
    }
}

fn print_phase_row(label: &str, summary: &PhaseSummary) {
    let stats = min_median_max(&summary.f1s);
    let (min, median, max) = match stats {
        None => {
            // Nothing to report — phase produced no scoreable
            // artefacts in any run. Print a one-row note and bail.
            println!("  {label:<22}  —  (no scoreable artefacts in any run)");
            for note in &summary.notes {
                println!("                          note: {note}");
            }
            return;
        }
        Some(s) => s,
    };
    let spread = (max - min) * 100.0;
    let counts: Vec<String> = summary
        .match_counts
        .iter()
        .map(|(m, e)| format!("{m}/{e}"))
        .collect();
    println!(
        "  {label:<22}  min {}   median {}   max {}   spread {:>4.1}pp   per-run {}",
        fmt_pct(Some(min)),
        fmt_pct(Some(median)),
        fmt_pct(Some(max)),
        spread,
        counts.join(",")
    );
    if summary.forbidden_hits > 0 {
        println!(
            "                          forbidden hits across runs: {}",
            summary.forbidden_hits
        );
    }
    for note in &summary.notes {
        println!("                          note: {note}");
    }
}

fn print_text_report(a: &AggregatedReport, durations: &[Duration]) {
    println!();
    println!("  Median-of-{} scoreboard", a.runs);
    println!("  ─────────────────────────────────────────────────────────────");
    print_phase_row("positions (Phase 1)", &a.positions);
    print_phase_row("person atoms", &a.person_atoms);
    print_phase_row("concept atoms", &a.concept_atoms);
    print_phase_row("work atoms", &a.work_atoms);
    print_phase_row("event atoms", &a.event_atoms);
    print_phase_row("state atoms", &a.state_atoms);
    print_phase_row("relation atoms", &a.relation_atoms);
    print_phase_row("question atoms", &a.question_atoms);
    print_phase_row("claim atoms", &a.claim_atoms);
    print_phase_row("fault lines (Phase 6)", &a.fault_lines);
    print_phase_row("open questions (P7)", &a.open_questions);
    print_phase_row("configurations (P8)", &a.configurations);

    if let Some((min, median, max)) = min_median_max(&a.aggregate_f1s) {
        let spread = (max - min) * 100.0;
        println!();
        println!(
            "  Aggregate F1 (mean of scored phases per run):  min {}   median {}   max {}   spread {:>4.1}pp",
            fmt_pct(Some(min)),
            fmt_pct(Some(median)),
            fmt_pct(Some(max)),
            spread
        );
    }

    if !durations.is_empty() {
        let total: f32 = durations.iter().map(|d| d.as_secs_f32()).sum();
        let avg = total / durations.len() as f32;
        println!();
        println!("  Wall-clock: {:.1}s total, {:.1}s avg per run", total, avg);
    }
}

fn write_json_report(path: &std::path::Path, report: &AggregatedReport) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_median_max_with_all_defined() {
        let vs = vec![Some(0.5), Some(0.8), Some(0.6)];
        let (min, median, max) = min_median_max(&vs).unwrap();
        assert!((min - 0.5).abs() < 1e-4);
        assert!((median - 0.6).abs() < 1e-4);
        assert!((max - 0.8).abs() < 1e-4);
    }

    #[test]
    fn min_median_max_skips_none_values() {
        let vs = vec![Some(0.5), None, Some(0.7)];
        let (min, median, max) = min_median_max(&vs).unwrap();
        // Only Some(0.5) and Some(0.7) participate; sorted = [0.5, 0.7];
        // even count → average of the two middle values.
        assert!((min - 0.5).abs() < 1e-4);
        assert!((median - 0.6).abs() < 1e-4);
        assert!((max - 0.7).abs() < 1e-4);
    }

    #[test]
    fn min_median_max_returns_none_when_all_undefined() {
        let vs = vec![None, None, None];
        assert!(min_median_max(&vs).is_none());
    }

    #[test]
    fn min_median_max_even_count_averages_middles() {
        let vs = vec![Some(0.2), Some(0.4), Some(0.6), Some(0.8)];
        let (_, median, _) = min_median_max(&vs).unwrap();
        assert!((median - 0.5).abs() < 1e-4); // (0.4 + 0.6)/2
    }

    #[test]
    fn parse_args_minimal_form() {
        let args: Vec<String> = ["fwd", "/tmp/g.toml"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "fwd");
        assert_eq!(p.golden_path, PathBuf::from("/tmp/g.toml"));
        assert_eq!(p.runs, 3);
        assert!(!p.keep_state);
    }

    #[test]
    fn parse_args_runs_and_keep_state() {
        let args: Vec<String> = ["fwd", "/tmp/g.toml", "--runs", "5", "--keep-state"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse_args(&args).unwrap();
        assert_eq!(p.runs, 5);
        assert!(p.keep_state);
    }

    #[test]
    fn parse_args_rejects_zero_runs() {
        let err = parse_args(&[
            "fwd".into(),
            "/tmp/g.toml".into(),
            "--runs".into(),
            "0".into(),
        ])
        .unwrap_err();
        assert!(err.contains("≥ 1"), "err: {err}");
    }

    #[test]
    fn parse_args_requires_corpus_and_golden() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
        let err = parse_args(&["fwd".into()]).unwrap_err();
        assert!(err.contains("golden-set-path"));
    }

    #[test]
    fn aggregate_f1_returns_none_when_no_phases_scored() {
        let r = EvalReport::default();
        assert!(aggregate_f1(&r).is_none());
    }
}
