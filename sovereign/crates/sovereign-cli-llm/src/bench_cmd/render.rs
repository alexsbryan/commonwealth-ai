// SPDX-License-Identifier: AGPL-3.0-or-later
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
        let label = format!("{} ({}/{})", o.corpus_id, o.group, o.id,);
        let summary = enrichment_summary(o);
        println!("  {label:<46}  {summary}");
        if let Some(warn) = baseline_age_warning(o) {
            println!("                                                  {warn}");
        }
        if let Some(note) = &o.note {
            println!("                                                  note: {note}");
        }
    }
    println!();
}

fn enrichment_summary(o: &BenchOutcome) -> String {
    let status_tag = status_glyph(o.status);
    let Some(enr) = &o.enrichment else {
        return status_tag.to_string();
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
        if let Some(warn) = baseline_age_warning(o) {
            println!("                                                  {warn}");
        }
        if let Some(note) = &o.note {
            println!("                                                  note: {note}");
        }
    }
    println!();
}

fn retrieval_summary(o: &BenchOutcome) -> String {
    let status_tag = status_glyph(o.status);
    let Some(ret) = &o.retrieval else {
        return status_tag.to_string();
    };
    let cur = &ret.current;
    let q = cur.results.len();
    // Three views of the same retrieval event:
    //   - answer-equiv (judge): "did the answer convey the expected
    //     fact?" Synth-only — semantic equivalence credit. The
    //     headline number when present because it measures user
    //     value, not bench-internal title coverage.
    //   - title-coverage (rigid src): "was the bank's canonical
    //     source title in the retrieved bag?" A useful diagnostic
    //     of retrieval-axis coverage; misleading as a quality
    //     metric when other corpora carry equivalent content.
    //   - keyword-match (strict fact): "did the answer text contain
    //     the expected keyword?" Penalises paraphrase even when the
    //     fact is conveyed; treat as a calibration metric, not a
    //     quality metric.
    let judge = mean(&cur.results, |r| {
        r.synth
            .as_ref()
            .and_then(|s| s.judge_fact_score.as_ref())
            .and_then(|s| s.ratio)
    });
    let fact = mean(&cur.results, |r| r.fact_score.ratio);
    let src = mean(&cur.results, |r| r.source_score.ratio);
    let essay = mean(&cur.results, |r| {
        r.essay_readiness.as_ref().map(|e| e.ratio())
    });

    let mut parts = vec![format!("{q}Q")];
    // Lead with judge when available (synth mode). Otherwise the
    // strict columns are the only signal.
    if let Some(judge_val) = judge {
        let prev = ret.baseline.as_ref().and_then(mean_run_judge);
        parts.push(retrieval_lever_cell("answer-equiv", Some(judge_val), prev));
    }
    parts.push(retrieval_lever_cell(
        "keyword-match",
        fact,
        ret.baseline.as_ref().map(mean_run_fact),
    ));
    parts.push(retrieval_lever_cell(
        "title-coverage",
        src,
        ret.baseline.as_ref().map(mean_run_src),
    ));
    if let Some(essay_val) = essay {
        let prev = ret.baseline.as_ref().and_then(mean_run_essay);
        parts.push(retrieval_lever_cell("essay", Some(essay_val), prev));
    }
    format!("{status_tag}  {}", parts.join("  "))
}

fn retrieval_lever_cell(label: &str, cur: Option<f32>, prev: Option<f32>) -> String {
    let Some(c) = cur else {
        return format!("{label} —");
    };
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
    mean(&run.results, |r| {
        r.essay_readiness.as_ref().map(|e| e.ratio())
    })
}
fn mean_run_judge(run: &crate::eval_cmd::runner::EvalRun) -> Option<f32> {
    mean(&run.results, |r| {
        r.synth
            .as_ref()
            .and_then(|s| s.judge_fact_score.as_ref())
            .and_then(|s| s.ratio)
    })
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
    let col_w = corpus_cols
        .iter()
        .map(|c| c.len())
        .max()
        .unwrap_or(12)
        .max(12);

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

/// Per-category matrix renders one of three views of the same
/// retrieval event. Default `judge` when synth produced judge
/// scores, else falls back to `fact_strict`. Two other levers
/// (`title_coverage`, `keyword_match`) surface via the same
/// matrix when an operator wants to drill on a specific axis.
#[derive(Debug, Clone, Copy)]
enum RetrievalLever {
    /// Judge-validated answer equivalence — "did the answer convey
    /// the expected fact?" Strongest correlate of user value.
    AnswerEquiv,
    /// Strict keyword match in answer text — "did the answer
    /// contain the expected substring?" Penalises paraphrase.
    KeywordMatch,
    /// Strict source-title coverage — "was the bank's declared
    /// canonical source in the retrieved bag?" Useful diagnostic of
    /// retrieval reach; misleading when a sibling corpus carries
    /// equivalent content.
    TitleCoverage,
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

    // Pick the lead lever for the matrix. When any current run has
    // judge scores (i.e. ran in synth mode), lead with judge; else
    // fall back to keyword-match. Title-coverage is always rendered
    // as a supporting matrix below the lead.
    let has_judge = rows.iter().any(|o| {
        o.retrieval
            .as_ref()
            .map(|r| {
                r.current.results.iter().any(|x| {
                    x.synth
                        .as_ref()
                        .and_then(|s| s.judge_fact_score.as_ref())
                        .is_some()
                })
            })
            .unwrap_or(false)
    });

    let (lead, lead_header) = if has_judge {
        (
            RetrievalLever::AnswerEquiv,
            "Per-category cross-corpus (Retrieval · answer-equiv [judge])",
        )
    } else {
        (
            RetrievalLever::KeywordMatch,
            "Per-category cross-corpus (Retrieval · keyword-match [strict fact])",
        )
    };
    print_retrieval_matrix_for(&categories, rows, lead, lead_header);
    if has_judge {
        // Supporting view: title coverage. Names the bench's
        // narrative claim (bank-declared sources) so divergences
        // are legible rather than treated as primary signal.
        print_retrieval_matrix_for(
            &categories,
            rows,
            RetrievalLever::TitleCoverage,
            "Per-category cross-corpus (Retrieval · title-coverage [bank-declared sources])",
        );
    } else {
        // No judge available — still show title-coverage as a
        // supporting view so the operator sees retrieval reach
        // separately from answer keyword-match.
        print_retrieval_matrix_for(
            &categories,
            rows,
            RetrievalLever::TitleCoverage,
            "Per-category cross-corpus (Retrieval · title-coverage [bank-declared sources])",
        );
    }
}

fn print_retrieval_matrix_for(
    categories: &BTreeSet<String>,
    rows: &[&BenchOutcome],
    lever: RetrievalLever,
    header: &str,
) {
    println!();
    println!("  {header}");
    println!("  ─────────────────────────────────────────────────────────────────────");

    let corpus_cols: Vec<&str> = rows
        .iter()
        .filter(|o| o.retrieval.is_some())
        .map(|o| o.corpus_id.as_str())
        .collect();

    let cat_w = categories
        .iter()
        .map(|c| c.len())
        .max()
        .unwrap_or(20)
        .max(20);
    let col_w = corpus_cols
        .iter()
        .map(|c| c.len())
        .max()
        .unwrap_or(12)
        .max(12);

    print!("  {:<width$}", "category", width = cat_w + 2);
    for c in &corpus_cols {
        print!("  {:>width$}", c, width = col_w);
    }
    println!();

    for cat in categories {
        print!("  {:<width$}", cat, width = cat_w + 2);
        for o in rows {
            let Some(ret) = &o.retrieval else { continue };
            let cell = format_category_cell(ret, cat, lever);
            print!("  {:>width$}", cell, width = col_w);
        }
        println!();
    }
}

fn format_category_cell(ret: &RetrievalOutcome, category: &str, lever: RetrievalLever) -> String {
    let cur: Vec<&_> = ret
        .current
        .results
        .iter()
        .filter(|r| r.category == category)
        .collect();
    if cur.is_empty() {
        return "—".into();
    }
    let extract = |r: &crate::eval_cmd::runner::EvalResult| -> Option<f32> {
        match lever {
            RetrievalLever::AnswerEquiv => r
                .synth
                .as_ref()
                .and_then(|s| s.judge_fact_score.as_ref())
                .and_then(|s| s.ratio),
            RetrievalLever::KeywordMatch => r.fact_score.ratio,
            RetrievalLever::TitleCoverage => r.source_score.ratio,
        }
    };
    let vals: Vec<f32> = cur.iter().filter_map(|r| extract(r)).collect();
    if vals.is_empty() {
        return "—".into();
    }
    let cur_mean: f32 = vals.iter().sum::<f32>() / vals.len() as f32;
    let prev_mean: Option<f32> = ret.baseline.as_ref().map(|b| {
        let prev: Vec<&_> = b
            .results
            .iter()
            .filter(|r| r.category == category)
            .collect();
        if prev.is_empty() {
            return 0.0;
        }
        let pvals: Vec<f32> = prev.iter().filter_map(|r| extract(r)).collect();
        if pvals.is_empty() {
            return 0.0;
        }
        pvals.iter().sum::<f32>() / pvals.len() as f32
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

    // Stale-baseline footer: name every bench whose diffed baseline is
    // over the age threshold, at the moment the operator is reading
    // the verdict. Warn-only — the counts above are the gate.
    let stale: Vec<String> = outcomes
        .iter()
        .filter_map(|o| {
            baseline_age_warning(o).map(|_| {
                format!(
                    "{}/{} {}d ({})",
                    o.group,
                    o.id,
                    o.baseline_age_days.unwrap_or(0),
                    o.baseline_captured.as_deref().unwrap_or("?"),
                )
            })
        })
        .collect();
    if !stale.is_empty() {
        println!(
            "  ⚠ stale baselines (> {}d): {} — re-mint with --update-baseline once adjudicated (see RUNBOOK)",
            super::baselines::baseline_max_age_days(),
            stale.join(", ")
        );
    }
}

/// `Some("⚠ baseline 41d old (2026-04-30)")` when the baseline this
/// outcome was diffed against exceeds `SOVEREIGN_BASELINE_MAX_AGE_DAYS`
/// (default 14). Warn-only: staleness is operator information — the
/// April-30-baseline incident was a HARD lane silently diffing against
/// a six-week-old snapshot — and never changes an exit code.
fn baseline_age_warning(o: &BenchOutcome) -> Option<String> {
    let age = o.baseline_age_days?;
    let max_age = super::baselines::baseline_max_age_days();
    if age <= max_age {
        return None;
    }
    let captured = o.baseline_captured.as_deref().unwrap_or("?");
    Some(format!("⚠ baseline {age}d old ({captured})"))
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
            baseline_captured: None,
            baseline_age_days: None,
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
            baseline_captured: None,
            baseline_age_days: None,
            levers: vec!["mechanism".into()],
            note: None,
        };
        let s = enrichment_summary(&o);
        assert!(s.contains("1/1 axes"));
    }
}
