//! Voice eval report writers.
//!
//! Two surfaces:
//!
//! * [`write_json_report`] — durable JSON, suitable for archiving
//!   and diffing against a previous run. Schema mirrors the
//!   structure of `enrich_cmd::eval`'s reports so existing
//!   tooling (`jq`, the desktop's report viewer) can read it
//!   uniformly.
//! * [`print_text_report`] — human-readable terminal summary with
//!   per-scenario pass/fail and per-axis pass rates.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::checks::ScenarioResult;
use super::judge::JudgeScore;

/// One eval run — every scenario's `ScenarioResult` plus the
/// rolled-up aggregate that diff tools chart over time.
///
/// Optional fields (`chat_model`, `judge_model`) tag the run so two
/// JSON reports can be diffed without an out-of-band registry.
/// Latency aggregates are folded in at write time from the
/// per-scenario `runtime_ms` / `judge_ms` so a report that's
/// dropped on disk carries everything a comparison tool needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceEvalRun {
    pub started_at_unix: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_model: Option<String>,
    pub results: Vec<ScenarioResult>,
    /// Per-scenario judge scores (parallel to `results`). Kept
    /// alongside the `ScenarioResult` array — they have to be
    /// stitched back together by index since `ScenarioResult` itself
    /// doesn't carry the judge axes (judge mode can be off).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_scores: Vec<Option<JudgeScore>>,
    /// Per-scenario runtime latency in milliseconds (parallel to
    /// `results`). Headline number for the small-vs-large baseline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_ms: Vec<u64>,
    /// Per-scenario judge latency in milliseconds (parallel to
    /// `results`). `None` when the judge wasn't run for that
    /// scenario.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_ms: Vec<Option<u64>>,
    /// Iter5: per-scenario per-stage runtime breakdown (parallel to
    /// `results`). `None` for scenarios that didn't traverse an
    /// instrumented witness path. Used by the text-report
    /// median-per-stage waterfall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_metrics: Vec<Option<sovereign_core::types::RuntimeMetrics>>,
    pub aggregate: AggregateScore,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregateScore {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    /// Pass rate per probe tag (e.g., "specific-uncertainty",
    /// "contradiction-across-time"). Only counts scenarios that
    /// declare that probe.
    pub by_probe: std::collections::BTreeMap<String, ProbeAggregate>,
    /// Pass rate of each individual deterministic check across
    /// scenarios where the check was enabled.
    pub by_check: ChecksAggregate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeAggregate {
    pub total: usize,
    pub passed: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChecksAggregate {
    pub length: CheckAggregate,
    pub question_density: CheckAggregate,
    pub banned_phrases: CheckAggregate,
    pub required_content: CheckAggregate,
    pub code_identifier: CheckAggregate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckAggregate {
    pub enabled: usize,
    pub passed: usize,
}

impl VoiceEvalRun {
    pub fn new() -> Self {
        Self {
            started_at_unix: chrono::Utc::now().timestamp(),
            chat_model: None,
            judge_model: None,
            results: Vec::new(),
            judge_scores: Vec::new(),
            runtime_ms: Vec::new(),
            judge_ms: Vec::new(),
            stage_metrics: Vec::new(),
            aggregate: AggregateScore::default(),
        }
    }

    /// Stamp the run with the chat / judge model ids. Optional —
    /// the dry-run (`--canned-response`) path leaves both `None`
    /// since no inference happened.
    pub fn with_models(
        mut self,
        chat_model: Option<String>,
        judge_model: Option<String>,
    ) -> Self {
        self.chat_model = chat_model;
        self.judge_model = judge_model;
        self
    }

    /// Live-run variant of `add`. Folds in the per-scenario judge
    /// score and latency alongside the deterministic check result so
    /// the parallel arrays stay aligned.
    pub fn add_live(
        &mut self,
        result: ScenarioResult,
        judge: Option<JudgeScore>,
        runtime_ms: u64,
        judge_ms: Option<u64>,
        stage_metrics: Option<sovereign_core::types::RuntimeMetrics>,
    ) {
        self.judge_scores.push(judge);
        self.runtime_ms.push(runtime_ms);
        self.judge_ms.push(judge_ms);
        self.stage_metrics.push(stage_metrics);
        self.add(result);
    }

    pub fn add(&mut self, result: ScenarioResult) {
        // Update aggregates as we go so `has_failures` is cheap.
        self.aggregate.total += 1;
        if result.passed {
            self.aggregate.passed += 1;
        } else {
            self.aggregate.failed += 1;
        }

        for probe in &result.probes {
            let entry = self
                .aggregate
                .by_probe
                .entry(probe.clone())
                .or_default();
            entry.total += 1;
            if result.passed {
                entry.passed += 1;
            }
        }

        let agg = &mut self.aggregate.by_check;
        if result.length.enabled {
            agg.length.enabled += 1;
            if result.length.passed {
                agg.length.passed += 1;
            }
        }
        if result.question_density.enabled {
            agg.question_density.enabled += 1;
            if result.question_density.passed {
                agg.question_density.passed += 1;
            }
        }
        if result.banned_phrases.enabled {
            agg.banned_phrases.enabled += 1;
            if result.banned_phrases.passed {
                agg.banned_phrases.passed += 1;
            }
        }
        if result.required_content.enabled {
            agg.required_content.enabled += 1;
            if result.required_content.passed {
                agg.required_content.passed += 1;
            }
        }
        if result.code_identifier.enabled {
            agg.code_identifier.enabled += 1;
            if result.code_identifier.passed {
                agg.code_identifier.passed += 1;
            }
        }

        self.results.push(result);
    }

    pub fn has_failures(&self) -> bool {
        self.aggregate.failed > 0
    }
}

pub fn write_json_report(path: &Path, run: &VoiceEvalRun) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(run)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, body)
}

pub fn print_text_report(run: &VoiceEvalRun) {
    println!("voice eval — {} scenarios", run.results.len());
    if let Some(m) = &run.chat_model {
        println!("chat model:  {m}");
    }
    if let Some(m) = &run.judge_model {
        println!("judge model: {m}");
    }
    println!("=========================");
    for (idx, r) in run.results.iter().enumerate() {
        let mark = if r.passed { "PASS" } else { "FAIL" };
        let runtime_ms = run.runtime_ms.get(idx).copied().unwrap_or(0);
        // Lead with the wall-clock so an operator scanning the output
        // can spot tail-latency outliers without doing JSON archaeology.
        if runtime_ms > 0 {
            println!(
                "{mark}  {}  ({})  {:.1}s",
                r.scenario_id,
                r.skill,
                runtime_ms as f64 / 1000.0
            );
        } else {
            println!("{mark}  {}  ({})", r.scenario_id, r.skill);
        }
        if !r.length.passed && r.length.enabled {
            if let Some(max) = r.length.max_chars {
                println!(
                    "      length: {} chars (max {})",
                    r.length.response_chars, max
                );
            }
        }
        if !r.question_density.passed && r.question_density.enabled {
            println!(
                "      question density: {} (min {:?}, max {:?})",
                r.question_density.question_count,
                r.question_density.min,
                r.question_density.max
            );
        }
        if !r.banned_phrases.passed {
            println!("      banned-phrase hits: {:?}", r.banned_phrases.hits);
        }
        if !r.required_content.passed && r.required_content.enabled {
            println!("      required content: none of the must-include phrases matched");
        }
        if !r.code_identifier.passed && r.code_identifier.enabled {
            // Show up to four offenders by name — enough to recognise
            // which corpus the planner pulled from without dumping the
            // full set into the operator's terminal.
            let preview: Vec<&str> = r
                .code_identifier
                .matches
                .iter()
                .take(4)
                .map(String::as_str)
                .collect();
            println!(
                "      code identifiers: {} (max {:?}) — first: {:?}",
                r.code_identifier.count, r.code_identifier.max, preview
            );
        }
    }

    println!();
    println!(
        "Total: {}/{} passed ({} failed)",
        run.aggregate.passed, run.aggregate.total, run.aggregate.failed
    );

    if !run.aggregate.by_probe.is_empty() {
        println!();
        println!("By probe:");
        for (probe, agg) in &run.aggregate.by_probe {
            println!("  {probe:30} {}/{}", agg.passed, agg.total);
        }
    }

    println!();
    println!("By check:");
    let agg = &run.aggregate.by_check;
    print_check_line("length", &agg.length);
    print_check_line("question_density", &agg.question_density);
    print_check_line("banned_phrases", &agg.banned_phrases);
    print_check_line("required_content", &agg.required_content);
    print_check_line("code_identifier", &agg.code_identifier);

    // Latency summary — only printed if any scenario has a non-zero
    // wall-clock (i.e., it's a live run, not a dry-run from canned
    // text). Median + p95 give the operator the typical and tail
    // numbers without dumping the full distribution.
    let runtimes: Vec<u64> = run.runtime_ms.iter().copied().filter(|m| *m > 0).collect();
    if !runtimes.is_empty() {
        let stats = LatencyStats::compute(&runtimes);
        println!();
        println!("Latency (runtime turn):");
        println!("  median {:>5} ms   p95 {:>5} ms   max {:>5} ms   n={}",
            stats.median, stats.p95, stats.max, stats.n);
    }
    let judges: Vec<u64> = run.judge_ms.iter().filter_map(|m| *m).collect();
    if !judges.is_empty() {
        let stats = LatencyStats::compute(&judges);
        println!("Latency (judge):");
        println!("  median {:>5} ms   p95 {:>5} ms   max {:>5} ms   n={}",
            stats.median, stats.p95, stats.max, stats.n);
    }

    // Iter5: per-stage waterfall. Median + max across all witness-
    // path scenarios in this run. Tells the operator where the time
    // is actually going on a relational turn — without this the
    // total runtime is opaque and 4B-vs-9B parsimony tests can't be
    // diagnosed beyond "model size doesn't help".
    let stages: Vec<&sovereign_core::types::RuntimeMetrics> = run
        .stage_metrics
        .iter()
        .filter_map(|m| m.as_ref())
        .collect();
    if !stages.is_empty() {
        println!();
        println!("Per-stage latency (witness path, n={}):", stages.len());
        let stage_stat = |get: fn(&sovereign_core::types::RuntimeMetrics) -> Option<u64>| {
            let xs: Vec<u64> = stages.iter().filter_map(|m| get(m)).collect();
            if xs.is_empty() { None } else { Some(LatencyStats::compute(&xs)) }
        };
        let print_stage = |name: &str, s: Option<LatencyStats>| {
            if let Some(s) = s {
                println!(
                    "  {name:<16} median {:>5} ms   max {:>5} ms   n={}",
                    s.median, s.max, s.n
                );
            }
        };
        print_stage("routing",        stage_stat(|m| m.routing_ms));
        print_stage("memory_recall",  stage_stat(|m| m.memory_recall_ms));
        print_stage("working_memory", stage_stat(|m| m.working_memory_ms));
        print_stage("topic_context",  stage_stat(|m| m.topic_context_ms));
        print_stage("pass_a",         stage_stat(|m| m.pass_a_ms));
        print_stage("tensions",       stage_stat(|m| m.tensions_ms));
        print_stage("synthesis",      stage_stat(|m| m.synthesis_ms));
        print_stage("total_turn",     stage_stat(|m| m.total_turn_ms));

        // Iter6: routing internals breakdown — when the LLM Pass 1
        // fired vs. when a pre-check short-circuited it. The 14% /
        // 6s routing slice from iter5 hides whether it's the LLM
        // call or pre-check evaluation; this surfaces the split.
        let routings: Vec<&sovereign_core::types::RoutingTiming> = stages
            .iter()
            .filter_map(|m| m.routing_breakdown.as_ref())
            .collect();
        if !routings.is_empty() {
            let llm_used = routings.iter().filter(|t| t.used_llm).count();
            let llm_skipped = routings.len() - llm_used;
            println!();
            println!("Routing internals (n={}):", routings.len());
            println!(
                "  LLM Pass 1 fired: {llm_used} | precheck short-circuited: {llm_skipped}"
            );
            let precheck_xs: Vec<u64> = routings.iter().map(|t| t.precheck_ms).collect();
            let llm_xs: Vec<u64> =
                routings.iter().filter_map(|t| if t.used_llm { Some(t.llm_ms) } else { None }).collect();
            let parse_xs: Vec<u64> =
                routings.iter().filter_map(|t| if t.used_llm { Some(t.parse_ms) } else { None }).collect();
            let s = LatencyStats::compute(&precheck_xs);
            println!(
                "  precheck_ms      median {:>5} ms   max {:>5} ms   n={}",
                s.median, s.max, s.n
            );
            if !llm_xs.is_empty() {
                let s = LatencyStats::compute(&llm_xs);
                println!(
                    "  llm_pass1_ms     median {:>5} ms   max {:>5} ms   n={}",
                    s.median, s.max, s.n
                );
            }
            if !parse_xs.is_empty() {
                let s = LatencyStats::compute(&parse_xs);
                println!(
                    "  parse_ms         median {:>5} ms   max {:>5} ms   n={}",
                    s.median, s.max, s.n
                );
            }
        }
    }

    // Per-axis judge averages — surfaces "specificity dropped two
    // points between small and large" without forcing the operator
    // to diff JSON. Only printed if the judge actually ran for at
    // least one scenario; the dry-run path leaves judge_scores empty.
    if let Some(means) = AxisMeans::from_run(run) {
        println!();
        println!("Judge axes (mean over {} scenarios):", means.n);
        means.print_lines("  ");
    }
}

/// Per-axis means over scored judge calls. Refactored out of
/// `print_text_report` so the diff path (`--diff <baseline.json>`)
/// can reuse the same shape against a stored baseline.
///
/// `n` carries the sample size so callers can include it in the
/// header — comparing means over different sample sizes is the
/// most common foot-gun in this kind of bench, and surfacing N
/// makes the comparison legible.
#[derive(Debug, Clone)]
pub struct AxisMeans {
    pub n: usize,
    pub right_attention: f64,
    pub right_specificity: f64,
    pub right_calibration: f64,
    pub right_question: f64,
    pub right_silence: f64,
    pub right_disagreement: f64,
    pub right_edge: f64,
    pub right_self_honesty: f64,
    /// Lower is better — kept as a positive number; the printer
    /// adds the explanatory annotation.
    pub avoid_list_penalty: f64,
}

impl AxisMeans {
    pub fn from_run(run: &VoiceEvalRun) -> Option<Self> {
        let scored: Vec<&JudgeScore> = run
            .judge_scores
            .iter()
            .filter_map(|s| s.as_ref())
            .collect();
        if scored.is_empty() {
            return None;
        }
        let n = scored.len();
        let denom = n as f64;
        let mean = |get: fn(&JudgeScore) -> u8| {
            scored.iter().map(|s| get(s) as f64).sum::<f64>() / denom
        };
        Some(Self {
            n,
            right_attention: mean(|s| s.right_attention),
            right_specificity: mean(|s| s.right_specificity),
            right_calibration: mean(|s| s.right_calibration),
            right_question: mean(|s| s.right_question),
            right_silence: mean(|s| s.right_silence),
            right_disagreement: mean(|s| s.right_disagreement),
            right_edge: mean(|s| s.right_edge),
            right_self_honesty: mean(|s| s.right_self_honesty),
            avoid_list_penalty: mean(|s| s.avoid_list_penalty),
        })
    }

    pub fn print_lines(&self, indent: &str) {
        println!("{indent}right_attention      {:.2}", self.right_attention);
        println!("{indent}right_specificity    {:.2}", self.right_specificity);
        println!("{indent}right_calibration    {:.2}", self.right_calibration);
        println!("{indent}right_question       {:.2}", self.right_question);
        println!("{indent}right_silence        {:.2}", self.right_silence);
        println!("{indent}right_disagreement   {:.2}", self.right_disagreement);
        println!("{indent}right_edge           {:.2}", self.right_edge);
        println!("{indent}right_self_honesty   {:.2}", self.right_self_honesty);
        println!(
            "{indent}avoid_list_penalty   {:.2}  (lower is better)",
            self.avoid_list_penalty
        );
    }

    pub fn axes(&self) -> [(&'static str, f64, bool); 9] {
        // (name, value, higher_is_better)
        [
            ("right_attention", self.right_attention, true),
            ("right_specificity", self.right_specificity, true),
            ("right_calibration", self.right_calibration, true),
            ("right_question", self.right_question, true),
            ("right_silence", self.right_silence, true),
            ("right_disagreement", self.right_disagreement, true),
            ("right_edge", self.right_edge, true),
            ("right_self_honesty", self.right_self_honesty, true),
            ("avoid_list_penalty", self.avoid_list_penalty, false),
        ]
    }
}

/// Print a diff table: baseline vs. current means + per-axis delta
/// flagged by direction-of-better.
///
/// The tuning loop's primary signal — per-scenario pass/fail flips
/// are noisy run-to-run (±2-4 scenarios per the bench README), but
/// axis means pool across all scenarios so they're more stable.
/// "Did this prompt edit move right_silence?" answered with one
/// number is what makes the loop tight enough to actually drive
/// changes from.
pub fn print_axis_diff(baseline: &AxisMeans, current: &AxisMeans) {
    println!();
    println!(
        "Axis diff (current n={} vs. baseline n={}):",
        current.n, baseline.n
    );
    println!(
        "  {:<22} {:>10} {:>10} {:>12}",
        "axis", "baseline", "current", "delta"
    );
    let base_axes = baseline.axes();
    let cur_axes = current.axes();
    for (i, (name, cur, higher_is_better)) in cur_axes.iter().enumerate() {
        let base = base_axes[i].1;
        let delta = cur - base;
        // Visual marker: ↑ when moved in the right direction,
        // ↓ when wrong direction, · when within ±0.05 (noise floor).
        let marker = if delta.abs() < 0.05 {
            "·"
        } else {
            let improved = if *higher_is_better {
                delta > 0.0
            } else {
                delta < 0.0
            };
            if improved {
                "↑"
            } else {
                "↓"
            }
        };
        println!(
            "  {:<22} {:>10.2} {:>10.2} {:>+10.2} {marker}",
            name, base, cur, delta
        );
    }
}

/// Load an `AxisMeans` from a previously-archived JSON report.
/// Used by the `--diff <baseline.json>` flag on `voice eval`.
pub fn load_axis_means_from_report(path: &Path) -> std::io::Result<AxisMeans> {
    let body = std::fs::read_to_string(path)?;
    let run: VoiceEvalRun = serde_json::from_str(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    AxisMeans::from_run(&run).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "report at {} contains no scored scenarios — was --no-judge in effect?",
                path.display()
            ),
        )
    })
}

/// Median / p95 / max over a list of latency samples. Pure
/// computation — kept out of the printing function so unit tests
/// can pin the percentile behaviour without capturing stdout.
struct LatencyStats {
    median: u64,
    p95: u64,
    max: u64,
    n: usize,
}

impl LatencyStats {
    fn compute(samples: &[u64]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let n = sorted.len();
        let median = sorted[n / 2];
        // Nearest-rank p95: ceil(0.95 * n) - 1 in zero-indexed
        // terms. For n=12 that's idx 11 → max value, which is
        // honest given the small sample size; with more samples
        // the percentile separates from the max naturally.
        let idx = ((n as f64 * 0.95).ceil() as usize).saturating_sub(1).min(n - 1);
        let p95 = sorted[idx];
        let max = *sorted.last().unwrap();
        Self { median, p95, max, n }
    }
}

fn print_check_line(name: &str, agg: &CheckAggregate) {
    if agg.enabled == 0 {
        println!("  {name:18} (no scenarios enabled this check)");
    } else {
        println!("  {name:18} {}/{}", agg.passed, agg.enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_eval::checks::{
        BannedPhraseCheck, CodeIdentifierCheck, LengthCheck, QuestionDensityCheck,
        RequiredContentCheck,
    };

    fn fake_result(id: &str, passed: bool, probes: Vec<String>) -> ScenarioResult {
        ScenarioResult {
            scenario_id: id.into(),
            skill: "inner-work".into(),
            probes,
            response: "stub response".into(),
            length: LengthCheck {
                enabled: true,
                response_chars: 13,
                max_chars: Some(100),
                passed,
            },
            question_density: QuestionDensityCheck {
                enabled: true,
                question_count: 0,
                min: None,
                max: Some(1),
                passed,
            },
            banned_phrases: BannedPhraseCheck {
                enabled: true,
                hits: Vec::new(),
                passed,
            },
            required_content: RequiredContentCheck {
                enabled: false,
                matched: None,
                passed: true,
            },
            code_identifier: CodeIdentifierCheck {
                enabled: false,
                matches: Vec::new(),
                count: 0,
                max: None,
                passed: true,
            },
            passed,
        }
    }

    #[test]
    fn aggregate_counts_pass_and_fail() {
        let mut run = VoiceEvalRun::new();
        run.add(fake_result("a", true, vec!["specific-uncertainty".into()]));
        run.add(fake_result("b", false, vec!["specific-uncertainty".into()]));
        run.add(fake_result(
            "c",
            true,
            vec!["specific-uncertainty".into(), "self-honesty".into()],
        ));
        assert_eq!(run.aggregate.total, 3);
        assert_eq!(run.aggregate.passed, 2);
        assert_eq!(run.aggregate.failed, 1);
        assert!(run.has_failures());
    }

    #[test]
    fn aggregate_by_probe_tracks_per_tag_pass_rate() {
        let mut run = VoiceEvalRun::new();
        run.add(fake_result("a", true, vec!["specific-uncertainty".into()]));
        run.add(fake_result("b", false, vec!["specific-uncertainty".into()]));
        run.add(fake_result("c", true, vec!["self-honesty".into()]));
        let p = &run.aggregate.by_probe["specific-uncertainty"];
        assert_eq!(p.total, 2);
        assert_eq!(p.passed, 1);
        let s = &run.aggregate.by_probe["self-honesty"];
        assert_eq!(s.total, 1);
        assert_eq!(s.passed, 1);
    }

    #[test]
    fn aggregate_by_check_tracks_each_check_independently() {
        let mut run = VoiceEvalRun::new();
        // First result: all enabled checks pass.
        run.add(fake_result("a", true, vec![]));
        // Second result: all enabled checks fail.
        run.add(fake_result("b", false, vec![]));
        let by = &run.aggregate.by_check;
        assert_eq!(by.length.enabled, 2);
        assert_eq!(by.length.passed, 1);
        assert_eq!(by.question_density.enabled, 2);
        assert_eq!(by.question_density.passed, 1);
        assert_eq!(by.banned_phrases.enabled, 2);
        assert_eq!(by.banned_phrases.passed, 1);
        // required_content was disabled in the fixture.
        assert_eq!(by.required_content.enabled, 0);
        assert_eq!(by.required_content.passed, 0);
    }

    #[test]
    fn write_json_report_produces_valid_json() {
        let mut run = VoiceEvalRun::new();
        run.add(fake_result("a", true, vec![]));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        write_json_report(&path, &run).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["aggregate"]["total"], 1);
        assert_eq!(parsed["aggregate"]["passed"], 1);
    }
}
