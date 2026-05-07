//! Render an `EvalRun` for human consumption.
//!
//! Two output modes today:
//!   - text  — per-question one-line summary + a category rollup. Default.
//!   - json  — pretty-printed `EvalRun` for archival + diff against
//!             a later run.
//!
//! Inspect-style detail (which fact missed, which expected source
//! never showed up) is always printed in text mode under the `--inspect`
//! flag in `mod.rs`. Without it, the per-question line is just a
//! pass/fail signal; with it, you see exactly what tripped.

use std::collections::BTreeMap;

use crate::eval_cmd::bank::{EvalBank, LatencyBudget};
use crate::eval_cmd::runner::{EvalResult, EvalRun};

pub fn print_text(run: &EvalRun, inspect: bool, bank: Option<&EvalBank>) {
    println!("═══ {} (corpus={}) — {} questions ═══",
        run.bank_name, run.corpus, run.results.len());
    println!();

    for r in &run.results {
        print_question_row(r, inspect);
    }

    println!();
    print_category_rollup(run);
    println!();
    print_overall(run);

    // Latency rollup is synth-only — there's no meaningful per-question
    // latency in retrieval mode (search is sub-second). Skip when no
    // synth row carries timings.
    let any_synth = run.results.iter().any(|r| r.synth.is_some());
    if any_synth {
        println!();
        let budget = bank.and_then(|b| b.bank.latency_budget.as_ref());
        print_latency_rollup(run, budget);
    }
}

fn print_question_row(r: &EvalResult, inspect: bool) {
    let src = score_label(&r.source_score.matched.len(), r.source_score.total_expected);
    let fact = score_label(&r.fact_score.matched.len(), r.fact_score.total_expected);

    if let Some(s) = r.synth.as_ref() {
        // Synth mode: timing column shows total wall + intent. The
        // chunks-fact-score is shown as a parenthetical delta so a
        // glance tells you "model surfaced N of M facts the chunks
        // already contained."
        let chunks_fact = score_label(
            &s.chunks_fact_score.matched.len(),
            s.chunks_fact_score.total_expected,
        );
        let intent = s.intent.as_deref().unwrap_or("?");
        let latency_s = s.total_latency_ms.map(|m| m as f64 / 1000.0).unwrap_or(0.0);
        let wall_s = s.stream_wall_ms as f64 / 1000.0;
        println!(
            "  [{id:30}] sources {src:>7}  facts {fact:>7}  (chunks {chunks_fact:>7})  intent={intent:<16} {latency:>5.1}s rt / {wall:>5.1}s wall  chunks={chunks}",
            id = r.question_id,
            src = src,
            fact = fact,
            chunks_fact = chunks_fact,
            intent = intent,
            latency = latency_s,
            wall = wall_s,
            chunks = s.retrieved_chunk_count,
        );
    } else {
        let vec_tag = if r.vector_eligible { "vec+fts" } else { "fts-only" };
        // When `--loose-source-judge` was on, render the loose score
        // as a parenthetical delta next to the rigid one so a glance
        // tells you "rigid X / Y; loose Z / Y" — never lower than X.
        let loose_tag = r
            .loose_source_score
            .as_ref()
            .map(|l| {
                let label = score_label(&l.matched.len(), l.total_expected);
                format!(" (loose {label:>7})")
            })
            .unwrap_or_default();
        // When `--essay-judge` was on, render the four-axis total +
        // per-axis breakdown after facts. Format: "essay 9/12 [t3/p2/d1/a3]"
        // — total first for at-a-glance scanning, axes for detail.
        let essay_tag = r
            .essay_readiness
            .as_ref()
            .map(|e| {
                format!(
                    "  essay {total}/12 [t{t}/p{p}/d{d}/a{a}]",
                    total = e.total,
                    t = e.topical_coverage,
                    p = e.position_attribution,
                    d = e.dialectical_breadth,
                    a = e.argument_depth,
                )
            })
            .unwrap_or_default();
        println!(
            "  [{id:30}] sources {src:>7}{loose_tag}  facts {fact:>7}{essay_tag}  {vec_tag:>8}  ({embed_ms}ms embed, {search_ms}ms search)",
            id = r.question_id,
            src = src,
            loose_tag = loose_tag,
            fact = fact,
            essay_tag = essay_tag,
            vec_tag = vec_tag,
            embed_ms = r.embed_ms,
            search_ms = r.search_ms,
        );
    }

    if !inspect {
        return;
    }

    println!("       Q: {}", r.question);
    if !r.source_score.missing.is_empty() {
        println!("       missing sources: {:?}", r.source_score.missing);
    }
    if !r.fact_score.missing.is_empty() {
        println!("       missing facts:   {:?}", r.fact_score.missing);
    }
    if let Some(s) = r.synth.as_ref() {
        if !s.source_origins.is_empty() {
            println!("       origins:         {:?}", s.source_origins);
        }
        if !s.chunks_fact_score.missing.is_empty() {
            println!(
                "       missing in chunks too: {:?}",
                s.chunks_fact_score.missing
            );
        }
        // Truncate the answer for the inspect block — full text is in
        // the JSON output for detailed review.
        let answer_preview: String = s.answer.chars().take(280).collect();
        println!(
            "       answer ({} chars, {} reasoning chars):",
            s.answer.chars().count(),
            s.reasoning_chars,
        );
        for line in answer_preview.lines().take(6) {
            println!("         {line}");
        }
        if s.answer.chars().count() > 280 {
            println!("         …");
        }
    }
    if r.retrieved.is_empty() {
        println!("       (no chunks retrieved)");
    } else {
        println!("       top retrieved:");
        for (i, c) in r.retrieved.iter().take(5).enumerate() {
            let title = c.title.as_deref().unwrap_or("<untitled>");
            // In synth mode the metadata snippets don't carry a score,
            // so we hide the `score=` prefix when it's stuck at 0.0.
            if r.synth.is_some() {
                println!(
                    "         [{rank:>2}] {title}",
                    rank = i + 1,
                    title = title,
                );
            } else {
                println!(
                    "         [{rank:>2}] score={score:.3} {title}",
                    rank = i + 1,
                    score = c.score,
                    title = title,
                );
            }
        }
    }
    println!();
}

fn print_category_rollup(run: &EvalRun) {
    println!("─── per category ───");
    // (sources_m, sources_t, strict_m, strict_t, judge_m, judge_t,
    //  judge_present_count, loose_source_m, loose_source_present_count)
    //  — the `*_present_count`s let us skip a column for categories
    //  where every row was --no-judge or had no loose pass.
    let mut by_cat: BTreeMap<&str, (usize, usize, usize, usize, usize, usize, usize, usize, usize)> =
        BTreeMap::new();
    for r in &run.results {
        let entry = by_cat
            .entry(r.category.as_str())
            .or_insert((0, 0, 0, 0, 0, 0, 0, 0, 0));
        entry.0 += r.source_score.matched.len();
        entry.1 += r.source_score.total_expected;
        entry.2 += r.fact_score.matched.len();
        entry.3 += r.fact_score.total_expected;
        if let Some(s) = r.synth.as_ref() {
            if let Some(j) = s.judge_fact_score.as_ref() {
                entry.4 += j.matched.len();
                entry.5 += j.total_expected;
                entry.6 += 1;
            }
        }
        if let Some(l) = r.loose_source_score.as_ref() {
            entry.7 += l.matched.len();
            entry.8 += 1;
        }
    }
    for (cat, (sm, st, fm, ft, jm, jt, jc, lm, lc)) in by_cat {
        let loose_seg = if lc > 0 {
            format!(" / loose {lm}/{st}")
        } else {
            String::new()
        };
        if jc > 0 {
            println!(
                "  {cat:30} sources {sm}/{st}{loose_seg}  facts strict {fm}/{ft}  judge {jm}/{jt}",
            );
        } else {
            println!("  {cat:30} sources {sm}/{st}{loose_seg}  facts {fm}/{ft}");
        }
    }
}

fn print_overall(run: &EvalRun) {
    let (sm, st, fm, ft) = run.results.iter().fold((0, 0, 0, 0), |acc, r| {
        (
            acc.0 + r.source_score.matched.len(),
            acc.1 + r.source_score.total_expected,
            acc.2 + r.fact_score.matched.len(),
            acc.3 + r.fact_score.total_expected,
        )
    });
    let (jm, jt, jc) = run.results.iter().fold((0usize, 0usize, 0usize), |acc, r| {
        if let Some(s) = r.synth.as_ref() {
            if let Some(j) = s.judge_fact_score.as_ref() {
                return (
                    acc.0 + j.matched.len(),
                    acc.1 + j.total_expected,
                    acc.2 + 1,
                );
            }
        }
        acc
    });
    let (lm, lc) = run.results.iter().fold((0usize, 0usize), |acc, r| {
        if let Some(l) = r.loose_source_score.as_ref() {
            (acc.0 + l.matched.len(), acc.1 + 1)
        } else {
            acc
        }
    });
    // Essay-readiness rollup: per-axis sums + total. Only meaningful
    // when at least one row had the judge enabled.
    let (et, ep, ed, ea, etot, ec) =
        run.results
            .iter()
            .fold((0u32, 0u32, 0u32, 0u32, 0u32, 0u32), |acc, r| {
                if let Some(e) = r.essay_readiness.as_ref() {
                    (
                        acc.0 + e.topical_coverage as u32,
                        acc.1 + e.position_attribution as u32,
                        acc.2 + e.dialectical_breadth as u32,
                        acc.3 + e.argument_depth as u32,
                        acc.4 + e.total as u32,
                        acc.5 + 1,
                    )
                } else {
                    acc
                }
            });
    println!("─── overall ───");
    println!("  sources {sm}/{st}  ({:.0}%)", percent(sm, st));
    if lc > 0 {
        println!(
            "  loose   {lm}/{st}  ({:.0}%)  ← LLM-judge loose source-credit",
            percent(lm, st)
        );
    }
    if ec > 0 {
        let max = ec * 12;
        println!(
            "  essay   {etot}/{max}  ({:.0}%)  axes: t={et}/{ax3} p={ep}/{ax3} d={ed}/{ax3} a={ea}/{ax3}  ← essay-readiness",
            percent(etot as usize, max as usize),
            ax3 = ec * 3,
        );
    }
    if jc > 0 {
        println!("  facts strict {fm}/{ft}  ({:.0}%)", percent(fm, ft));
        println!(
            "  facts judge  {jm}/{jt}  ({:.0}%)  ← instructor mode",
            percent(jm, jt)
        );
    } else {
        println!("  facts   {fm}/{ft}  ({:.0}%)", percent(fm, ft));
    }

    // Synth-only rollups: chunks-vs-answer fact gap (= "how often did
    // retrieval surface the fact but the model failed to use it"), and
    // total wall time across the run. Only emit when at least one row
    // came back with synth data.
    let has_synth = run.results.iter().any(|r| r.synth.is_some());
    if !has_synth {
        return;
    }
    let (cfm, cft) = run
        .results
        .iter()
        .filter_map(|r| r.synth.as_ref())
        .fold((0usize, 0usize), |acc, s| {
            (
                acc.0 + s.chunks_fact_score.matched.len(),
                acc.1 + s.chunks_fact_score.total_expected,
            )
        });
    let total_wall_ms: u64 = run
        .results
        .iter()
        .filter_map(|r| r.synth.as_ref())
        .map(|s| s.stream_wall_ms)
        .sum();
    let avg_wall_s = if run.results.is_empty() {
        0.0
    } else {
        (total_wall_ms as f64 / 1000.0) / run.results.len() as f64
    };
    println!(
        "  facts-in-chunks {cfm}/{cft}  ({:.0}%)  ← upper bound from snippet haystack",
        percent(cfm, cft)
    );
    println!(
        "  wall            {:.1}s total / {:.1}s avg per question",
        total_wall_ms as f64 / 1000.0,
        avg_wall_s
    );
}

/// Render per-category and overall wall-time percentiles, with
/// budget-violation counts when the bank declares a `LatencyBudget`.
/// Wall time is the synth-mode `stream_wall_ms` field — the time
/// from `handle_message_stream` start to stream-drained end. We
/// prefer this over `provenance.total_latency_ms` because the
/// runtime's own clock can be missing on bail-outs (clarification
/// path), but wall is always present for any row that produced
/// SOMETHING.
fn print_latency_rollup(run: &EvalRun, budget: Option<&LatencyBudget>) {
    println!("─── latency (synth wall_ms) ───");

    // Group wall times by category. Skip rows with no synth payload —
    // those are retrieval-only rows that snuck into a synth-style run
    // (shouldn't happen, but guard anyway).
    let mut by_cat: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut all: Vec<u64> = Vec::with_capacity(run.results.len());
    for r in &run.results {
        let Some(s) = r.synth.as_ref() else { continue };
        if s.stream_wall_ms == 0 {
            // Bail-out path with zero-latency placeholder — exclude
            // from percentiles so it doesn't pull p50 down.
            continue;
        }
        by_cat
            .entry(r.category.as_str())
            .or_default()
            .push(s.stream_wall_ms);
        all.push(s.stream_wall_ms);
    }

    // Header. Width-aligned to category column from upstream rollup
    // for visual continuity in the terminal.
    println!(
        "  {:30}  {:>7}  {:>7}  {:>7}  {:>9}",
        "category", "p50", "p95", "max", "over-p95"
    );
    for (cat, mut vals) in by_cat {
        vals.sort_unstable();
        let p50 = percentile(&vals, 0.50);
        let p95 = percentile(&vals, 0.95);
        let max = *vals.last().unwrap();
        let cat_budget = budget.and_then(|b| b.by_category.get(cat));
        let p95_target = cat_budget
            .map(|cb| cb.p95_ms)
            .or_else(|| budget.and_then(|b| b.default_p95_ms));
        let over = match p95_target {
            Some(t) => vals.iter().filter(|v| **v > t).count(),
            None => 0,
        };
        let over_label = match p95_target {
            Some(t) => {
                if over == 0 {
                    format!("0 (≤{}ms)", t)
                } else {
                    format!("{}/{} (>{}ms)", over, vals.len(), t)
                }
            }
            None => "—".to_string(),
        };
        println!(
            "  {:30}  {:>5.1}s  {:>5.1}s  {:>5.1}s  {:>9}",
            cat,
            p50 as f64 / 1000.0,
            p95 as f64 / 1000.0,
            max as f64 / 1000.0,
            over_label,
        );
    }
    if !all.is_empty() {
        all.sort_unstable();
        let p50 = percentile(&all, 0.50);
        let p95 = percentile(&all, 0.95);
        let max = *all.last().unwrap();
        let total_target = budget.and_then(|b| b.default_p95_ms);
        let max_target = budget.and_then(|b| b.max_per_question_ms);
        let over_max = match max_target {
            Some(t) => all.iter().filter(|v| **v > t).count(),
            None => 0,
        };
        println!(
            "  {:30}  {:>5.1}s  {:>5.1}s  {:>5.1}s  {}",
            "overall",
            p50 as f64 / 1000.0,
            p95 as f64 / 1000.0,
            max as f64 / 1000.0,
            match (total_target, max_target) {
                (Some(t), Some(m)) => format!("p95 budget {}ms · {} over hard cap {}ms", t, over_max, m),
                (Some(t), None) => format!("p95 budget {}ms", t),
                (None, Some(m)) => format!("{} over hard cap {}ms", over_max, m),
                (None, None) => "no budget".to_string(),
            }
        );
    }
}

/// Linear-interpolation percentile on a sorted slice. Returns 0 when
/// `vals` is empty so the caller can short-circuit cleanly.
fn percentile(vals: &[u64], p: f64) -> u64 {
    if vals.is_empty() {
        return 0;
    }
    if vals.len() == 1 {
        return vals[0];
    }
    let n = vals.len() as f64;
    // p95 on 20 samples: rank = 0.95 * 19 = 18.05, between idx 18 and 19.
    let rank = p * (n - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return vals[lo];
    }
    let frac = rank - lo as f64;
    let interp = vals[lo] as f64 + frac * (vals[hi] as f64 - vals[lo] as f64);
    interp.round() as u64
}

fn score_label(matched: &usize, total: usize) -> String {
    if total == 0 {
        return "—".into();
    }
    format!("{matched}/{total}")
}

fn percent(num: usize, denom: usize) -> f32 {
    if denom == 0 {
        return 0.0;
    }
    100.0 * num as f32 / denom as f32
}

pub fn print_json(run: &EvalRun) -> Result<(), String> {
    let s = serde_json::to_string_pretty(run).map_err(|e| format!("serialize run: {e}"))?;
    println!("{s}");
    Ok(())
}

pub fn write_json_file(run: &EvalRun, path: &std::path::Path) -> Result<(), String> {
    let s = serde_json::to_string_pretty(run).map_err(|e| format!("serialize run: {e}"))?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Routing-only run printer.
///
/// One row per question with the classifier's decision, the expected
/// intent, a ✓/✗ marker, the router's confidence + rationale, and
/// the per-question latency. Trailing rollup shows accuracy by
/// category — that's the signal a prompt tweak should move.
pub fn print_routing(run: &crate::eval_cmd::runner::RoutingRun) {
    use std::collections::BTreeMap;

    println!(
        "═══ {} — routing-only ({} questions) ═══\n",
        run.bank_name,
        run.results.len()
    );

    for r in &run.results {
        let mark = if r.correct { "✓" } else { "✗" };
        let rationale = r
            .rationale
            .as_deref()
            .map(|s| {
                let trimmed = s.trim();
                if trimmed.len() > 90 {
                    format!("{}…", &trimmed[..90])
                } else {
                    trimmed.to_string()
                }
            })
            .unwrap_or_default();
        println!(
            "  {mark} [{:30}] expected={:14} actual={:14} conf={:.2} {:>5}ms  {}",
            r.question_id,
            r.expected,
            r.actual_intent,
            r.confidence,
            r.latency_ms,
            rationale
        );
    }

    // Per-category rollup.
    let mut by_cat: BTreeMap<String, (usize, usize, u64)> = BTreeMap::new();
    let mut total_correct = 0usize;
    let mut total_latency: u64 = 0;
    for r in &run.results {
        let entry = by_cat.entry(r.category.clone()).or_default();
        entry.0 += if r.correct { 1 } else { 0 };
        entry.1 += 1;
        entry.2 += r.latency_ms;
        if r.correct {
            total_correct += 1;
        }
        total_latency += r.latency_ms;
    }
    println!("\n─── per category ───");
    for (cat, (correct, total, lat_sum)) in &by_cat {
        let avg_ms = if *total > 0 { lat_sum / (*total as u64) } else { 0 };
        println!(
            "  {cat:<26} {correct}/{total} correct  avg {avg_ms}ms"
        );
    }
    let n = run.results.len();
    let avg_total = if n > 0 { total_latency / n as u64 } else { 0 };
    let pct = if n > 0 {
        100.0 * total_correct as f32 / n as f32
    } else {
        0.0
    };
    println!(
        "\n─── overall ───\n  {total_correct}/{n} correct ({pct:.0}%)  avg {avg_total}ms per classify"
    );
}

pub fn write_routing_json_file(
    run: &crate::eval_cmd::runner::RoutingRun,
    path: &std::path::Path,
) -> Result<(), String> {
    let s = serde_json::to_string_pretty(run).map_err(|e| format!("serialize run: {e}"))?;
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval_cmd::runner::ScoreSnapshot;

    fn snapshot(matched: &[&str], missing: &[&str]) -> ScoreSnapshot {
        let total = matched.len() + missing.len();
        ScoreSnapshot {
            matched: matched.iter().map(|s| s.to_string()).collect(),
            missing: missing.iter().map(|s| s.to_string()).collect(),
            total_expected: total,
            ratio: if total == 0 { None } else { Some(matched.len() as f32 / total as f32) },
        }
    }

    fn result(id: &str, cat: &str, src: ScoreSnapshot, facts: ScoreSnapshot) -> EvalResult {
        EvalResult {
            question_id: id.into(),
            category: cat.into(),
            question: format!("Q for {id}"),
            retrieved: Vec::new(),
            source_score: src,
            fact_score: facts,
            embed_ms: 0,
            search_ms: 0,
            corpora_hit: Vec::new(),
            vector_eligible: true,
            synth: None,
            loose_source_score: None,
            loose_source_evidence: Vec::new(),
            essay_readiness: None,
        }
    }

    #[test]
    fn percent_handles_zero_denominator() {
        assert_eq!(percent(0, 0), 0.0);
    }

    #[test]
    fn score_label_dashes_when_no_expected() {
        assert_eq!(score_label(&0, 0), "—");
        assert_eq!(score_label(&3, 5), "3/5");
    }

    #[test]
    fn json_round_trips() {
        let run = EvalRun {
            bank_name: "demo".into(),
            corpus: "wikipedia".into(),
            limit: 5,
            started_at_unix: 0,
            results: vec![result(
                "q1",
                "factual",
                snapshot(&["s1"], &[]),
                snapshot(&["f1"], &["f2"]),
            )],
        };
        let s = serde_json::to_string(&run).unwrap();
        let back: EvalRun = serde_json::from_str(&s).unwrap();
        assert_eq!(back.results.len(), 1);
        assert_eq!(back.results[0].question_id, "q1");
    }
}
