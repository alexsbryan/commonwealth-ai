//! Two-pane scoreboard + cross-corpus matrices for `bench all`.
//!
//! Layout:
//!   1. Enrichment-eval scoreboard (one row per enrichment bench)
//!   2. Retrieval + LLM-judge scoreboard (one row per retrieval bench)
//!   3. Per-axis cross-corpus matrix (Enrichment lane)
//!   4. Per-category cross-corpus matrix (Retrieval lane)
//!   5. Summary line: green / improved / regressed / first-run / stale + exit code
//!
//! Per the lever framing, NO single aggregate F1 across corpora or
//! across surfaces. Per-corpus / per-axis / per-category only.

use std::collections::{BTreeMap, BTreeSet};

use super::all::{BenchOutcome, BenchStatus, EnrichmentOutcome, RetrievalOutcome};

const REGRESSION_THRESHOLD: f32 = 0.005; // 0.5 pt; renderer-side; matches all.rs default

pub(super) fn print_two_pane_scoreboard(outcomes: &[BenchOutcome]) {
    let enrichment: Vec<&BenchOutcome> = outcomes
        .iter()
        .filter(|o| o.surface == "enrichment")
        .collect();
    let retrieval: Vec<&BenchOutcome> = outcomes
        .iter()
        .filter(|o| o.surface == "retrieval")
        .collect();

    if !enrichment.is_empty() {
        print_enrichment_scoreboard(&enrichment);
        print_enrichment_matrix(&enrichment);
    }
    if !retrieval.is_empty() {
        print_retrieval_scoreboard(&retrieval);
        print_retrieval_matrix(&retrieval);
    }
    print_summary(outcomes);
}

// ── Per-pane scoreboards ──────────────────────────────────────────

fn print_enrichment_scoreboard(rows: &[&BenchOutcome]) {
    println!();
    println!("  Enrichment-eval scoreboard (atom F1 per axis vs hand-authored goldens)");
    println!("  ─────────────────────────────────────────────────────────────────────");
    for o in rows {
        let label = format!(
            "{} ({}/{})",
            o.corpus_id, o.group, o.id,
        );
        let summary = enrichment_summary(o);
        println!("  {label:<46}  {summary}");
        if let Some(note) = &o.note {
            println!("                                                  note: {note}");
        }
    }
    println!();
}

fn enrichment_summary(o: &BenchOutcome) -> String {
    let status_tag = status_glyph(o.status);
    let Some(enr) = &o.enrichment else {
        return format!("{status_tag}");
    };

    let cur = &enr.current;
    let total_axes = cur.axis_scores.len();
    if total_axes == 0 {
        return format!("{status_tag}  (no typed axes in golden)");
    }

    let mut within = 0;
    let mut regressed = 0;
    let mut improved = 0;
    for (key, cur_score) in &cur.axis_scores {
        let cur_f1 = cur_score.f1().unwrap_or(0.0);
        let prev_f1 = enr
            .baseline
            .as_ref()
            .and_then(|b| b.axis_scores.get(key))
            .and_then(|s| s.f1())
            .unwrap_or(cur_f1);
        let delta = cur_f1 - prev_f1;
        if delta < -REGRESSION_THRESHOLD {
            regressed += 1;
        } else if delta > REGRESSION_THRESHOLD {
            improved += 1;
        } else {
            within += 1;
        }
    }

    let mut parts = vec![format!("{within}/{total_axes} axes within baseline")];
    if regressed > 0 {
        parts.push(format!("{regressed} regressed"));
    }
    if improved > 0 {
        parts.push(format!("{improved} improved"));
    }
    format!("{status_tag}  {}", parts.join(" · "))
}

fn print_retrieval_scoreboard(rows: &[&BenchOutcome]) {
    println!();
    println!("  Retrieval + LLM-judge scoreboard (per-question recall / readiness)");
    println!("  ─────────────────────────────────────────────────────────────────────");
    for o in rows {
        let label = format!("{} ({}/{})", o.corpus_id, o.group, o.id);
        let summary = retrieval_summary(o);
        println!("  {label:<46}  {summary}");
        if let Some(note) = &o.note {
            println!("                                                  note: {note}");
        }
    }
    println!();
}

fn retrieval_summary(o: &BenchOutcome) -> String {
    let status_tag = status_glyph(o.status);
    let Some(ret) = &o.retrieval else {
        return format!("{status_tag}");
    };
    let cur = &ret.current;
    let q = cur.results.len();
    let fact = mean(&cur.results, |r| r.fact_score.ratio);
    let src = mean(&cur.results, |r| r.source_score.ratio);
    let essay = mean(&cur.results, |r| {
        r.essay_readiness.as_ref().and_then(|e| Some(e.ratio()))
    });

    let mut parts = vec![format!("{q}Q")];
    parts.push(retrieval_lever_cell("fact", fact, ret.baseline.as_ref().map(mean_run_fact)));
    parts.push(retrieval_lever_cell("src", src, ret.baseline.as_ref().map(mean_run_src)));
    if let Some(essay_val) = essay {
        let prev = ret.baseline.as_ref().and_then(mean_run_essay);
        parts.push(retrieval_lever_cell("essay", Some(essay_val), prev));
    }
    format!("{status_tag}  {}", parts.join("  "))
}

fn retrieval_lever_cell(label: &str, cur: Option<f32>, prev: Option<f32>) -> String {
    let Some(c) = cur else { return format!("{label} —") };
    let delta = prev.map(|p| c - p);
    let glyph = match delta {
        Some(d) if d < -REGRESSION_THRESHOLD => format!("↓{:.2}", -d),
        Some(d) if d > REGRESSION_THRESHOLD => format!("↑{:.2}", d),
        _ => "✓".into(),
    };
    format!("{label} {c:.2} {glyph}")
}

fn mean_run_fact(run: &crate::eval_cmd::runner::EvalRun) -> f32 {
    mean(&run.results, |r| r.fact_score.ratio).unwrap_or(0.0)
}
fn mean_run_src(run: &crate::eval_cmd::runner::EvalRun) -> f32 {
    mean(&run.results, |r| r.source_score.ratio).unwrap_or(0.0)
}
fn mean_run_essay(run: &crate::eval_cmd::runner::EvalRun) -> Option<f32> {
    mean(&run.results, |r| r.essay_readiness.as_ref().and_then(|e| Some(e.ratio())))
}

// ── Cross-corpus matrices ─────────────────────────────────────────

fn print_enrichment_matrix(rows: &[&BenchOutcome]) {
    // Collect all axes across all benches
    let mut axes: BTreeSet<&str> = BTreeSet::new();
    for o in rows {
        if let Some(enr) = &o.enrichment {
            for key in enr.current.axis_scores.keys() {
                axes.insert(key.as_str());
            }
        }
    }
    if axes.is_empty() {
        return;
    }

    println!();
    println!("  Per-axis cross-corpus (Enrichment lane · F1)");
    println!("  ─────────────────────────────────────────────────────────────────────");

    // Header: axis + corpus columns
    let corpus_cols: Vec<&str> = rows
        .iter()
        .filter(|o| o.enrichment.is_some())
        .map(|o| o.corpus_id.as_str())
        .collect();

    let axis_w = axes.iter().map(|a| a.len()).max().unwrap_or(10).max(10);
    let col_w = corpus_cols.iter().map(|c| c.len()).max().unwrap_or(12).max(12);

    print!("  {:<width$}", "axis", width = axis_w + 2);
    for c in &corpus_cols {
        print!("  {:>width$}", c, width = col_w);
    }
    println!();

    for axis in &axes {
        print!("  {:<width$}", axis, width = axis_w + 2);
        for o in rows {
            let Some(enr) = &o.enrichment else { continue };
            let cell = format_axis_cell(enr, axis);
            print!("  {:>width$}", cell, width = col_w);
        }
        println!();
    }
}

fn format_axis_cell(enr: &EnrichmentOutcome, axis: &str) -> String {
    let Some(cur_score) = enr.current.axis_scores.get(axis) else {
        return "—".into();
    };
    let Some(cur_f1) = cur_score.f1() else {
        return "—".into();
    };
    let prev_f1 = enr
        .baseline
        .as_ref()
        .and_then(|b| b.axis_scores.get(axis))
        .and_then(|s| s.f1());
    let glyph = match prev_f1 {
        None => "·".to_string(),
        Some(p) => {
            let d = cur_f1 - p;
            if d < -REGRESSION_THRESHOLD {
                format!("↓{:.0}", -d * 100.0)
            } else if d > REGRESSION_THRESHOLD {
                format!("↑{:.0}", d * 100.0)
            } else {
                "✓".into()
            }
        }
    };
    format!("{:.1} {}", cur_f1 * 100.0, glyph)
}

fn print_retrieval_matrix(rows: &[&BenchOutcome]) {
    // Collect all categories across all benches
    let mut categories: BTreeSet<String> = BTreeSet::new();
    for o in rows {
        if let Some(ret) = &o.retrieval {
            for r in &ret.current.results {
                categories.insert(r.category.clone());
            }
        }
    }
    if categories.is_empty() {
        return;
    }

    println!();
    println!("  Per-category cross-corpus (Retrieval lane · fact_recall)");
    println!("  ─────────────────────────────────────────────────────────────────────");

    let corpus_cols: Vec<&str> = rows
        .iter()
        .filter(|o| o.retrieval.is_some())
        .map(|o| o.corpus_id.as_str())
        .collect();

    let cat_w = categories.iter().map(|c| c.len()).max().unwrap_or(20).max(20);
    let col_w = corpus_cols.iter().map(|c| c.len()).max().unwrap_or(12).max(12);

    print!("  {:<width$}", "category", width = cat_w + 2);
    for c in &corpus_cols {
        print!("  {:>width$}", c, width = col_w);
    }
    println!();

    for cat in &categories {
        print!("  {:<width$}", cat, width = cat_w + 2);
        for o in rows {
            let Some(ret) = &o.retrieval else { continue };
            let cell = format_category_cell(ret, cat);
            print!("  {:>width$}", cell, width = col_w);
        }
        println!();
    }
}

fn format_category_cell(ret: &RetrievalOutcome, category: &str) -> String {
    let cur: Vec<&_> = ret
        .current
        .results
        .iter()
        .filter(|r| r.category == category)
        .collect();
    if cur.is_empty() {
        return "—".into();
    }
    let cur_mean: f32 = cur
        .iter()
        .filter_map(|r| r.fact_score.ratio)
        .sum::<f32>()
        / cur.len().max(1) as f32;
    let prev_mean: Option<f32> = ret.baseline.as_ref().map(|b| {
        let prev: Vec<&_> = b.results.iter().filter(|r| r.category == category).collect();
        if prev.is_empty() {
            return 0.0;
        }
        prev.iter().filter_map(|r| r.fact_score.ratio).sum::<f32>() / prev.len().max(1) as f32
    });
    let glyph = match prev_mean {
        None => "·".to_string(),
        Some(p) => {
            let d = cur_mean - p;
            if d < -REGRESSION_THRESHOLD {
                format!("↓{:.0}", -d * 100.0)
            } else if d > REGRESSION_THRESHOLD {
                format!("↑{:.0}", d * 100.0)
            } else {
                "✓".into()
            }
        }
    };
    format!("{:.2} {}", cur_mean, glyph)
}

// ── Summary line ──────────────────────────────────────────────────

fn print_summary(outcomes: &[BenchOutcome]) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for o in outcomes {
        let tag = match o.status {
            BenchStatus::Green => "green",
            BenchStatus::Regressed => "regressed",
            BenchStatus::Improved => "improved",
            BenchStatus::FirstRun => "first-run",
            BenchStatus::Stale => "stale",
        };
        *counts.entry(tag).or_insert(0) += 1;
    }
    println!();
    let s = ["green", "improved", "regressed", "first-run", "stale"]
        .iter()
        .map(|tag| format!("{} {tag}", counts.get(tag).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(" · ");
    println!("  {s}");
}

fn status_glyph(s: BenchStatus) -> &'static str {
    match s {
        BenchStatus::Green => "✓",
        BenchStatus::Regressed => "⚠",
        BenchStatus::Improved => "↑",
        BenchStatus::FirstRun => "·",
        BenchStatus::Stale => "⊘",
    }
}

// ── Helpers ───────────────────────────────────────────────────────

fn mean<T, F>(items: &[T], f: F) -> Option<f32>
where
    F: Fn(&T) -> Option<f32>,
{
    let vals: Vec<f32> = items.iter().filter_map(&f).collect();
    if vals.is_empty() {
        return None;
    }
    Some(vals.iter().sum::<f32>() / vals.len() as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrich_cmd::eval::{EvalReport, PhaseScore};

    #[test]
    fn enrichment_summary_no_axes() {
        let o = BenchOutcome {
            id: "x".into(),
            group: "g".into(),
            corpus_id: "c".into(),
            surface: "enrichment".into(),
            status: BenchStatus::Green,
            enrichment: Some(EnrichmentOutcome {
                current: EvalReport::default(),
                baseline: None,
            }),
            retrieval: None,
            levers: vec![],
            note: None,
        };
        let s = enrichment_summary(&o);
        assert!(s.contains("no typed axes"));
    }

    #[test]
    fn enrichment_summary_counts_axes() {
        let mut current = EvalReport::default();
        let mut s1 = PhaseScore::default();
        s1.expected = 5;
        s1.matched = 4;
        current.axis_scores.insert("mechanism".into(), s1);

        let o = BenchOutcome {
            id: "x".into(),
            group: "g".into(),
            corpus_id: "c".into(),
            surface: "enrichment".into(),
            status: BenchStatus::Green,
            enrichment: Some(EnrichmentOutcome {
                current,
                baseline: None,
            }),
            retrieval: None,
            levers: vec!["mechanism".into()],
            note: None,
        };
        let s = enrichment_summary(&o);
        assert!(s.contains("1/1 axes"));
    }
}
