// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rubric report IO and the shared rendering blocks.
//!
//! A lane owns its report STRUCT (a moral scenario carries a dilemma
//! source; a situated probe will carry a corpus and a question type)
//! but never its own copy of the numbers or of how they are
//! presented. Everything here is lane-agnostic: JSON round-trip, the
//! per-dimension table with its Wilson intervals, the could-not-judge
//! disclosure, and the disjoint-CI diff.
//!
//! The diff is the surface that decides whether a change counts.
//! Its rule is [`super::score::DimensionAggregate::separates_from`]:
//! a delta earns a `*` only when the two 95% intervals are disjoint.
//! Everything else prints its magnitude with no significance claim.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::score::{Aggregate, RubricItem};

/// A completed lane run, as the shared reporters need to see it.
/// `subject_model` is the model UNDER TEST; `judge_model` is the
/// pinned rater. Keeping them distinct in the trait is what lets the
/// diff warn when a comparison silently changed its own instrument.
pub trait RubricRun {
    /// The lane's judged unit — a moral scenario, a situated probe.
    type Item: RubricItem;
    fn subject_model(&self) -> &str;
    fn judge_model(&self) -> &str;
    fn judge_trials(&self) -> u8;
    fn aggregate(&self) -> &Aggregate;
    /// The per-item records behind [`Self::aggregate`]. The aggregate alone
    /// cannot support a PAIRED comparison: it has already summed away which
    /// probe produced which verdict, which is exactly the information the
    /// paired test needs (see [`super::paired`]).
    fn items(&self) -> &[Self::Item];
}

pub fn write_json_report<R: Serialize>(path: &Path, run: &R) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(run)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, body)
}

pub fn load_report<R: DeserializeOwned>(path: &Path) -> std::io::Result<R> {
    let body = std::fs::read_to_string(path)?;
    serde_json::from_str(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Mean / median / stddev, plus the on-run judge-reliability signal.
pub fn print_score_summary(a: &Aggregate) {
    println!();
    println!(
        "Overall score: mean {:.1}  median {:.1}  stddev {:.1}  (n={})",
        a.overall_mean, a.score_median, a.score_stddev, a.scenarios
    );
    if let Some(u) = a.unanimity {
        println!("Judge unanimity across trials: {:.1}%", 100.0 * u);
    }
}

/// Per-dimension fulfillment with its Wilson interval — the table a
/// harness change is read against.
pub fn print_dimension_table(a: &Aggregate) {
    println!();
    println!("By dimension (criterion fulfillment %, Wilson 95% CI):");
    for (dim, d) in &a.by_dimension {
        println!(
            "  {dim:<18} {:5.1}%  [{:4.1}, {:4.1}]  ({}/{} fulfilled{})",
            d.rate,
            d.ci95_low,
            d.ci95_high,
            d.fulfilled,
            d.criteria - d.could_not_judge,
            if d.could_not_judge > 0 {
                format!(", {} could-not-judge", d.could_not_judge)
            } else {
                String::new()
            }
        );
    }
}

/// The lane's secondary slice. `label` names the axis ("role domain"
/// for moral); the block is skipped entirely when the lane doesn't
/// slice or nothing scored.
pub fn print_group_table(a: &Aggregate, label: &str) {
    if a.by_role_domain.is_empty() {
        return;
    }
    println!();
    println!("By {label} (mean score):");
    for (group, mean) in &a.by_role_domain {
        println!("  {group:<18} {mean:5.1}");
    }
}

/// Absence is reported, never defaulted (ARCH_PRINCIPLES §18.3) — a
/// score computed over a shrunken denominator says so on its face.
pub fn print_could_not_judge(a: &Aggregate) {
    if a.could_not_judge == 0 {
        return;
    }
    println!();
    println!(
        "Could-not-judge: {}/{} criteria ({:.1}%){}",
        a.could_not_judge,
        a.criteria_total,
        100.0 * a.could_not_judge_rate,
        if a.degraded {
            "  — RUN DEGRADED (threshold 10%): scores not comparable to a clean run"
        } else {
            ""
        }
    );
}

/// Per-dimension + overall delta against a stored baseline report.
/// The A/B surface: run arm A with `--report a.json`, run arm B with
/// `--diff a.json`.
pub fn print_diff<R: RubricRun>(baseline: &R, current: &R) {
    println!();
    println!("Diff vs baseline ({} → {}):", baseline.subject_model(), current.subject_model());
    if baseline.judge_model() != current.judge_model()
        || baseline.judge_trials() != current.judge_trials()
    {
        println!(
            "  WARNING: judge differs (baseline {} x{}, current {} x{}) — deltas conflate \
             model change with judge change",
            baseline.judge_model(),
            baseline.judge_trials(),
            current.judge_model(),
            current.judge_trials()
        );
    }
    println!("  {:<18} {:>10} {:>10} {:>10}", "metric", "baseline", "current", "delta");
    let row = |name: &str, b: f64, c: f64| {
        let delta = c - b;
        let marker = if delta.abs() < 0.5 {
            "·"
        } else if delta > 0.0 {
            "+"
        } else {
            "-"
        };
        println!("  {name:<18} {b:>10.1} {c:>10.1} {delta:>+10.1} {marker}");
    };
    let (ba, ca) = (baseline.aggregate(), current.aggregate());
    row("overall", ba.overall_mean, ca.overall_mean);
    let dims: std::collections::BTreeSet<&String> =
        ba.by_dimension.keys().chain(ca.by_dimension.keys()).collect();
    let mut any_sig = false;
    for dim in dims {
        let b = ba.by_dimension.get(dim);
        let c = ca.by_dimension.get(dim);
        let br = b.map(|d| d.rate).unwrap_or(0.0);
        let cr = c.map(|d| d.rate).unwrap_or(0.0);
        // Disjoint 95% CIs = the delta clears sampling noise. An
        // overlapping pair isn't proof of no difference — it's
        // "this bank can't tell them apart on this dimension".
        let significant = match (b, c) {
            (Some(b), Some(c)) => b.separates_from(c),
            _ => false,
        };
        let delta = cr - br;
        let marker = if delta.abs() < 0.5 {
            "·"
        } else if delta > 0.0 {
            "+"
        } else {
            "-"
        };
        let sig = if significant {
            any_sig = true;
            " *"
        } else {
            ""
        };
        println!("  {dim:<18} {br:>10.1} {cr:>10.1} {delta:>+10.1} {marker}{sig}");
    }
    if any_sig {
        println!("  (* = 95% confidence intervals do not overlap)");
    }
    // Both readings, always, side by side. The table above treats the two arms
    // as independent samples and reports RATES; the block below uses the fact
    // that they ran the same probes and reports FLIPS. They answer different
    // questions and can disagree — a dimension can move several points on the
    // rate while its flips are 4-better/2-worse, which is a heterogeneous
    // effect wearing the costume of a clean one. Printing only the first is
    // what let that read as a win (measured on arm C, 2026-08-05).
    super::paired::print_paired(baseline, current);
}
