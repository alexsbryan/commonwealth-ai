//! Aggregate report for one cognitive-bank run.
//!
//! The shape mirrors `crate::workflow::WorkflowReport` and friends —
//! a flat JSON struct with per-category sub-aggregates and a vector
//! of per-item outcomes. Operators load two of these and diff them
//! to see whether a system change moved any category.

use crate::cognitive::scorer::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub run_id: String,
    pub started_at: String,
    pub ended_at: String,
    pub model: String,
    pub daemon_url: String,
    pub temperature: f32,
    pub seed: u64,
    pub items_total: usize,
    pub items_passed: usize,
    pub pass_rate: f32,
    pub elapsed_ms_total: u64,
    pub elapsed_ms_p50: u64,
    pub elapsed_ms_p95: u64,
    pub per_category: BTreeMap<String, CategoryAggregate>,
    pub outcomes: Vec<Outcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryAggregate {
    pub items_total: usize,
    pub items_passed: usize,
    pub pass_rate: f32,
}

#[derive(Debug, Clone)]
pub struct BuildOpts<'a> {
    pub run_id: &'a str,
    pub started_at: &'a str,
    pub ended_at: &'a str,
    pub model: &'a str,
    pub daemon_url: &'a str,
    pub temperature: f32,
    pub seed: u64,
}

pub fn build(opts: BuildOpts<'_>, outcomes: Vec<Outcome>) -> Report {
    let items_total = outcomes.len();
    let items_passed = outcomes.iter().filter(|o| o.passed).count();
    let pass_rate = if items_total == 0 {
        0.0
    } else {
        items_passed as f32 / items_total as f32
    };

    let mut per_category: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for o in &outcomes {
        let e = per_category.entry(o.category.clone()).or_insert((0, 0));
        e.0 += 1;
        if o.passed {
            e.1 += 1;
        }
    }
    let per_category = per_category
        .into_iter()
        .map(|(k, (total, passed))| {
            let rate = if total == 0 {
                0.0
            } else {
                passed as f32 / total as f32
            };
            (
                k,
                CategoryAggregate {
                    items_total: total,
                    items_passed: passed,
                    pass_rate: rate,
                },
            )
        })
        .collect();

    let mut elapsed: Vec<u64> = outcomes.iter().map(|o| o.elapsed_ms).collect();
    elapsed.sort_unstable();
    let elapsed_ms_total: u64 = elapsed.iter().sum();
    let elapsed_ms_p50 = percentile(&elapsed, 0.50);
    let elapsed_ms_p95 = percentile(&elapsed, 0.95);

    Report {
        run_id: opts.run_id.to_string(),
        started_at: opts.started_at.to_string(),
        ended_at: opts.ended_at.to_string(),
        model: opts.model.to_string(),
        daemon_url: opts.daemon_url.to_string(),
        temperature: opts.temperature,
        seed: opts.seed,
        items_total,
        items_passed,
        pass_rate,
        elapsed_ms_total,
        elapsed_ms_p50,
        elapsed_ms_p95,
        per_category,
        outcomes,
    }
}

fn percentile(sorted: &[u64], p: f32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f32 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineDiff {
    pub baseline_run_id: String,
    pub current_run_id: String,
    pub pass_rate_delta: f32,
    pub per_category_delta: BTreeMap<String, f32>,
    pub items_flipped_to_pass: Vec<String>,
    pub items_flipped_to_fail: Vec<String>,
}

pub fn diff_baseline(baseline: &Report, current: &Report) -> BaselineDiff {
    let mut baseline_pass: std::collections::HashMap<&str, bool> =
        std::collections::HashMap::new();
    for o in &baseline.outcomes {
        baseline_pass.insert(&o.item_id, o.passed);
    }
    let mut flipped_to_pass = Vec::new();
    let mut flipped_to_fail = Vec::new();
    for o in &current.outcomes {
        match baseline_pass.get(o.item_id.as_str()) {
            Some(&was) if was != o.passed => {
                if o.passed {
                    flipped_to_pass.push(o.item_id.clone());
                } else {
                    flipped_to_fail.push(o.item_id.clone());
                }
            }
            _ => {}
        }
    }
    let pass_rate_delta = current.pass_rate - baseline.pass_rate;
    let mut per_category_delta = BTreeMap::new();
    for (cat, cur) in &current.per_category {
        let prev = baseline
            .per_category
            .get(cat)
            .map(|a| a.pass_rate)
            .unwrap_or(0.0);
        per_category_delta.insert(cat.clone(), cur.pass_rate - prev);
    }
    BaselineDiff {
        baseline_run_id: baseline.run_id.clone(),
        current_run_id: current.run_id.clone(),
        pass_rate_delta,
        per_category_delta,
        items_flipped_to_pass: flipped_to_pass,
        items_flipped_to_fail: flipped_to_fail,
    }
}

pub fn render_text(report: &Report, diff: Option<&BaselineDiff>) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "=== cognitive bank run {} ===", report.run_id);
    let _ = writeln!(out, "  model        : {}", report.model);
    let _ = writeln!(out, "  daemon       : {}", report.daemon_url);
    let _ = writeln!(out, "  temperature  : {}", report.temperature);
    let _ = writeln!(out, "  seed         : {:#x}", report.seed);
    let _ = writeln!(
        out,
        "  total        : {}/{} passed ({:.1}%)",
        report.items_passed,
        report.items_total,
        report.pass_rate * 100.0
    );
    let _ = writeln!(
        out,
        "  elapsed      : {} ms total · p50 {} ms · p95 {} ms",
        report.elapsed_ms_total, report.elapsed_ms_p50, report.elapsed_ms_p95
    );
    let _ = writeln!(out, "  per-category :");
    for (cat, agg) in &report.per_category {
        let _ = writeln!(
            out,
            "    {:<22}  {}/{}  ({:.1}%)",
            cat,
            agg.items_passed,
            agg.items_total,
            agg.pass_rate * 100.0
        );
    }
    if let Some(d) = diff {
        let _ = writeln!(out, "\n--- vs baseline {} ---", d.baseline_run_id);
        let _ = writeln!(
            out,
            "  pass-rate delta: {:+.1}%",
            d.pass_rate_delta * 100.0
        );
        for (cat, delta) in &d.per_category_delta {
            let _ = writeln!(out, "    {:<22}  {:+.1}%", cat, delta * 100.0);
        }
        if !d.items_flipped_to_pass.is_empty() {
            let _ = writeln!(
                out,
                "  newly passing  : {}",
                d.items_flipped_to_pass.join(", ")
            );
        }
        if !d.items_flipped_to_fail.is_empty() {
            let _ = writeln!(
                out,
                "  newly failing  : {}",
                d.items_flipped_to_fail.join(", ")
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, cat: &str, passed: bool) -> Outcome {
        Outcome {
            item_id: id.into(),
            category: cat.into(),
            passed,
            reason: "ok".into(),
            response_raw: String::new(),
            elapsed_ms: 100,
            model: "m".into(),
        }
    }

    #[test]
    fn aggregate_groups_per_category() {
        let outcomes = vec![
            outcome("a", "decision_quality", true),
            outcome("b", "decision_quality", false),
            outcome("c", "honesty_calibration", true),
        ];
        let report = build(
            BuildOpts {
                run_id: "r1",
                started_at: "",
                ended_at: "",
                model: "m",
                daemon_url: "d",
                temperature: 0.0,
                seed: 0,
            },
            outcomes,
        );
        assert_eq!(report.items_total, 3);
        assert_eq!(report.items_passed, 2);
        let dq = report.per_category.get("decision_quality").unwrap();
        assert_eq!(dq.items_total, 2);
        assert_eq!(dq.items_passed, 1);
    }

    #[test]
    fn diff_detects_flips() {
        let baseline_outcomes = vec![
            outcome("a", "decision_quality", true),
            outcome("b", "decision_quality", false),
        ];
        let current_outcomes = vec![
            outcome("a", "decision_quality", false),
            outcome("b", "decision_quality", true),
        ];
        let baseline = build(
            BuildOpts {
                run_id: "r0",
                started_at: "",
                ended_at: "",
                model: "m",
                daemon_url: "d",
                temperature: 0.0,
                seed: 0,
            },
            baseline_outcomes,
        );
        let current = build(
            BuildOpts {
                run_id: "r1",
                started_at: "",
                ended_at: "",
                model: "m",
                daemon_url: "d",
                temperature: 0.0,
                seed: 0,
            },
            current_outcomes,
        );
        let d = diff_baseline(&baseline, &current);
        assert_eq!(d.items_flipped_to_pass, vec!["b".to_string()]);
        assert_eq!(d.items_flipped_to_fail, vec!["a".to_string()]);
    }
}
