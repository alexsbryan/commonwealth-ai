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

use crate::eval_cmd::runner::{EvalResult, EvalRun};

pub fn print_text(run: &EvalRun, inspect: bool) {
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
}

fn print_question_row(r: &EvalResult, inspect: bool) {
    let src = score_label(&r.source_score.matched.len(), r.source_score.total_expected);
    let fact = score_label(&r.fact_score.matched.len(), r.fact_score.total_expected);
    let vec_tag = if r.vector_eligible { "vec+fts" } else { "fts-only" };

    println!(
        "  [{id:30}] sources {src:>7}  facts {fact:>7}  {vec_tag:>8}  ({embed_ms}ms embed, {search_ms}ms search)",
        id = r.question_id,
        src = src,
        fact = fact,
        vec_tag = vec_tag,
        embed_ms = r.embed_ms,
        search_ms = r.search_ms,
    );

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
    if r.retrieved.is_empty() {
        println!("       (no chunks retrieved)");
    } else {
        println!("       top retrieved:");
        for (i, c) in r.retrieved.iter().take(5).enumerate() {
            let title = c.title.as_deref().unwrap_or("<untitled>");
            println!(
                "         [{rank:>2}] score={score:.3} {title}",
                rank = i + 1,
                score = c.score,
                title = title,
            );
        }
    }
    println!();
}

fn print_category_rollup(run: &EvalRun) {
    println!("─── per category ───");
    let mut by_cat: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for r in &run.results {
        let entry = by_cat.entry(r.category.as_str()).or_insert((0, 0, 0, 0));
        entry.0 += r.source_score.matched.len();
        entry.1 += r.source_score.total_expected;
        entry.2 += r.fact_score.matched.len();
        entry.3 += r.fact_score.total_expected;
    }
    for (cat, (sm, st, fm, ft)) in by_cat {
        println!(
            "  {cat:30} sources {sm}/{st}  facts {fm}/{ft}",
        );
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
    println!("─── overall ───");
    println!("  sources {sm}/{st}  ({:.0}%)", percent(sm, st));
    println!("  facts   {fm}/{ft}  ({:.0}%)", percent(fm, ft));
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
