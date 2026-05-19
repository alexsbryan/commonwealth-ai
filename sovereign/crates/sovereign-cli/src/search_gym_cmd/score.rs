//! Predicate evaluation + per-fixture / aggregate report.
//!
//! Reads a `Transcript` (from `runner.rs`) and a `Predicate` (from
//! `predicate.rs`), optionally calls a Judge for semantic
//! predicates, returns `Scored`. The structural predicates are pure
//! (no I/O, no async); the semantic predicates only kick in when a
//! `&dyn Judge` is provided.
//!
//! Two scoring entry points:
//!   - `score(...)`             — structural predicates only. Pure.
//!   - `score_with_judge(...)`  — adds judge-evaluated semantic
//!                                 predicates. Async, fallible.
//!
//! Splitting them lets tests exercise structural logic without a
//! judge mock, and lets `--no-judge` runs skip the judge layer
//! cleanly.

use serde::Serialize;

use super::judge::{Judge, Verdict};
use super::predicate::Predicate;
use super::runner::Transcript;

/// One fixture's scored outcome. `reasons` carries every predicate
/// failure observed; `pass` is `true` iff `reasons` is empty AND
/// `runner_error` was `None`.
#[derive(Debug, Clone, Serialize)]
pub struct Scored {
    pub slug: String,
    pub pass: bool,
    pub reasons: Vec<String>,
    /// Copied off the transcript for the JSON report. Runner errors
    /// (HTTP failure, malformed daemon response, missing mock
    /// fixture) are distinct from predicate failures.
    pub runner_error: Option<String>,
    pub model_ms: u128,
    pub tool_calls_observed: usize,
    /// Every judge verdict produced while scoring this fixture.
    /// Empty for `score(...)` calls (no judge); populated for
    /// `score_with_judge(...)`. Operators read this to debug why
    /// a semantic predicate failed; the rationale field is the
    /// load-bearing signal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_verdicts: Vec<JudgeRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JudgeRecord {
    pub predicate: &'static str,
    pub assertion: String,
    pub passes: bool,
    pub rationale: String,
}

/// Structural-only scoring. Pure, no judge. Use this for unit tests
/// and for `--no-judge` runs. Semantic-predicate evaluation needs
/// `score_with_judge`.
pub fn score(slug: &str, predicate: &Predicate, tx: &Transcript) -> Scored {
    let mut reasons: Vec<String> = Vec::new();

    if let Some(err) = &tx.runner_error {
        return Scored {
            slug: slug.to_string(),
            pass: false,
            reasons: vec![format!("runner_error: {err}")],
            runner_error: Some(err.clone()),
            model_ms: tx.model_ms,
            tool_calls_observed: tx.tool_calls.len(),
            judge_verdicts: Vec::new(),
        };
    }

    let search_calls: Vec<_> = tx
        .tool_calls
        .iter()
        .filter(|tc| tc.name == "search" || tc.name == "web_search")
        .collect();

    // ─── Decision axis ──────────────────────────────────────────
    if let Some(expected) = predicate.should_call_search {
        let actually = !search_calls.is_empty();
        if actually != expected {
            reasons.push(format!(
                "should_call_search={expected} but model_searched={actually}"
            ));
        }
    }

    for forbidden in &predicate.forbidden_tools {
        if tx.tool_calls.iter().any(|tc| &tc.name == forbidden) {
            reasons.push(format!("forbidden_tool_called: {forbidden}"));
        }
    }

    if let Some(first) = &predicate.expected_first_tool {
        match tx.tool_calls.first() {
            Some(tc) if &tc.name == first => {}
            Some(tc) => reasons.push(format!(
                "expected_first_tool={first:?} but first_was={:?}",
                tc.name
            )),
            None => reasons.push(format!("expected_first_tool={first:?} but no tools called")),
        }
    }

    if let Some(cap) = predicate.max_search_calls {
        if search_calls.len() > cap {
            reasons.push(format!(
                "max_search_calls={cap} but observed={}",
                search_calls.len()
            ));
        }
    }

    // ─── Query shape (token count is structural) ────────────────
    if let Some(cap) = predicate.expected_query_max_tokens {
        if let Some(first_search) = search_calls.first() {
            let query = first_search
                .arguments
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let token_count = query.split_whitespace().count();
            if token_count > cap {
                reasons.push(format!(
                    "expected_query_max_tokens={cap} but query has {token_count} tokens ({query:?})"
                ));
            }
        } else if predicate.should_call_search != Some(false) {
            reasons.push(
                "expected_query_max_tokens set but no search call observed".into(),
            );
        }
    }

    // ─── Result handling (URL set membership) ───────────────────
    if let Some(min) = predicate.must_cite_url_from_mock {
        let cited = tx
            .mock_urls
            .iter()
            .filter(|url| tx.final_message.contains(url.as_str()))
            .count();
        if cited < min {
            reasons.push(format!(
                "must_cite_url_from_mock={min} but only {cited} mock URL(s) appeared in final message"
            ));
        }
    }

    if predicate.must_not_cite_url_outside_mock {
        // Pull every http(s) URL out of the final message and check
        // it appears in `mock_urls`. Plain regex on the prose: any
        // sequence starting with http:// or https:// terminated by
        // whitespace, comma, paren, or angle-bracket.
        for cited in extract_urls(&tx.final_message) {
            if !tx.mock_urls.iter().any(|u| u == &cited) {
                reasons.push(format!(
                    "must_not_cite_url_outside_mock but model cited URL not in mock: {cited:?}"
                ));
            }
        }
    }

    // Surface that semantic predicates were set but unevaluated, so
    // operators running `--no-judge` see the asymmetry instead of a
    // misleading green pass. score_with_judge() clears this reason
    // before evaluating the assertions itself.
    if !predicate.final_message_satisfies.is_empty()
        || !predicate.query_satisfies.is_empty()
    {
        reasons.push(format!(
            "semantic predicates set ({} on final_message, {} on query) but no judge \
             provided — re-run without --no-judge to evaluate them",
            predicate.final_message_satisfies.len(),
            predicate.query_satisfies.len(),
        ));
    }

    Scored {
        slug: slug.to_string(),
        pass: reasons.is_empty(),
        reasons,
        runner_error: None,
        model_ms: tx.model_ms,
        tool_calls_observed: tx.tool_calls.len(),
        judge_verdicts: Vec::new(),
    }
}

/// Score `tx` against `predicate` with judge-evaluated semantic
/// predicates. Calls `score()` for the structural pass first, then
/// adds judge-driven reasons on top. The judge's per-assertion
/// verdicts land in `Scored::judge_verdicts` for operator debugging
/// even when the assertion passed (so reading the report tells the
/// story, not just the verdict).
pub async fn score_with_judge(
    slug: &str,
    predicate: &Predicate,
    tx: &Transcript,
    judge: &dyn Judge,
) -> Scored {
    let mut scored = score(slug, predicate, tx);

    // If the runner errored, structural pass already returned with
    // a single runner_error reason; skip the judge — there's no
    // model output to evaluate.
    if scored.runner_error.is_some() {
        return scored;
    }

    // Drop the placeholder "no judge provided" reason added by
    // score() — we ARE the judge path.
    scored
        .reasons
        .retain(|r| !r.contains("semantic predicates set"));

    // For final_message_satisfies, pass the FULL conversation
    // transcript rather than the bare final assistant message —
    // assertions routinely reference earlier turns ("the user
    // stated X earlier") which the judge can't verify from just
    // the final reply. Falls back to final_message if the runner
    // hasn't populated conversation_view (defensive — should
    // always be set when runner_error is None).
    let final_subject = if tx.conversation_view.is_empty() {
        tx.final_message.as_str()
    } else {
        tx.conversation_view.as_str()
    };

    for assertion in &predicate.final_message_satisfies {
        match judge.judge(assertion, final_subject).await {
            Ok(v) => {
                if !v.passes {
                    scored.reasons.push(format!(
                        "final_message_satisfies fail: {assertion:?} — judge: {}",
                        v.rationale
                    ));
                }
                scored.judge_verdicts.push(JudgeRecord {
                    predicate: "final_message_satisfies",
                    assertion: assertion.clone(),
                    passes: v.passes,
                    rationale: v.rationale,
                });
            }
            Err(e) => {
                scored
                    .reasons
                    .push(format!("judge error on final_message_satisfies: {e}"));
            }
        }
    }

    // Query-satisfies assertions evaluate against the FIRST search
    // call's query string. Mirrors the structural query-shape
    // predicate's policy.
    let first_search_query: Option<String> = tx
        .tool_calls
        .iter()
        .find(|tc| tc.name == "search" || tc.name == "web_search")
        .and_then(|tc| {
            tc.arguments
                .get("query")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    for assertion in &predicate.query_satisfies {
        let subject = match &first_search_query {
            Some(q) => q.as_str(),
            None => {
                scored.reasons.push(format!(
                    "query_satisfies fail: {assertion:?} — no search call observed"
                ));
                continue;
            }
        };
        match judge.judge(assertion, subject).await {
            Ok(v) => {
                if !v.passes {
                    scored.reasons.push(format!(
                        "query_satisfies fail: {assertion:?} — judge: {}",
                        v.rationale
                    ));
                }
                scored.judge_verdicts.push(JudgeRecord {
                    predicate: "query_satisfies",
                    assertion: assertion.clone(),
                    passes: v.passes,
                    rationale: v.rationale,
                });
            }
            Err(e) => {
                scored
                    .reasons
                    .push(format!("judge error on query_satisfies: {e}"));
            }
        }
    }

    scored.pass = scored.reasons.is_empty();
    scored
}

/// Keep `Verdict` in scope so the file compiles even if no other
/// code path references it directly (it does today, but cheap
/// defence against accidental import removal during refactors).
#[allow(dead_code)]
fn _force_verdict_in_scope(_v: Verdict) {}

/// Aggregate result across N replays of one fixture. Mirrors the
/// code gym's `pass_count / N` shape.
#[derive(Debug, Clone, Serialize)]
pub struct AggregatedFixture {
    pub slug: String,
    pub replays: usize,
    pub passes: usize,
    pub rate: f32,
    /// Deduplicated reasons across all replays — useful for binning
    /// failure modes without flooding the report.
    pub failure_reasons: Vec<String>,
    pub mean_model_ms: u128,
}

pub fn aggregate(slug: &str, scored: &[Scored]) -> AggregatedFixture {
    let passes = scored.iter().filter(|s| s.pass).count();
    let mut failure_reasons: Vec<String> = Vec::new();
    for s in scored.iter().filter(|s| !s.pass) {
        for r in &s.reasons {
            if !failure_reasons.contains(r) {
                failure_reasons.push(r.clone());
            }
        }
    }
    let mean_model_ms = if scored.is_empty() {
        0
    } else {
        scored.iter().map(|s| s.model_ms).sum::<u128>() / scored.len() as u128
    };
    AggregatedFixture {
        slug: slug.to_string(),
        replays: scored.len(),
        passes,
        rate: if scored.is_empty() {
            0.0
        } else {
            passes as f32 / scored.len() as f32
        },
        failure_reasons,
        mean_model_ms,
    }
}

/// Render the human-readable summary table. Format mirrors the code
/// gym's output so operators don't context-switch between gyms.
pub fn render_table(aggregates: &[AggregatedFixture]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:>9} {:>6}   fail reasons (first 3)\n",
        "fixture", "pass", "rate"
    ));
    out.push_str(&format!("{:-<40} {:->9} {:->6}   {:-<40}\n", "", "", "", ""));
    let mut total_pass = 0usize;
    let mut total_run = 0usize;
    for a in aggregates {
        let reasons_preview = a
            .failure_reasons
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("  |  ");
        out.push_str(&format!(
            "{:<40} {:>9} {:>5.0}%   {}\n",
            a.slug,
            format!("{}/{}", a.passes, a.replays),
            a.rate * 100.0,
            reasons_preview
        ));
        total_pass += a.passes;
        total_run += a.replays;
    }
    let total_rate = if total_run == 0 {
        0.0
    } else {
        total_pass as f32 / total_run as f32 * 100.0
    };
    out.push_str(&format!(
        "\ntotal: {total_pass}/{total_run} ({:.0}%)\n",
        total_rate
    ));
    out
}

fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["http://", "https://"] {
        let mut search_from = 0usize;
        while let Some(idx) = text[search_from..].find(marker) {
            let abs = search_from + idx;
            let end = text[abs..]
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | '<' | '>' | ')' | '"' | '\''))
                .map(|n| abs + n)
                .unwrap_or(text.len());
            // Trim trailing common-punctuation that gets picked up by
            // citation patterns: a period at the end of a sentence,
            // a closing parenthesis the URL was wrapped in, etc.
            let mut url = text[abs..end].trim_end_matches(|c: char| matches!(c, '.' | ',' | ')' | ']'));
            // Defensive: bail if the trim ate the scheme.
            if !url.starts_with("http") {
                url = &text[abs..end];
            }
            out.push(url.to_string());
            search_from = end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::runner::ObservedToolCall;

    fn tx_with_calls(calls: Vec<ObservedToolCall>, final_msg: &str) -> Transcript {
        Transcript {
            tool_calls: calls,
            final_message: final_msg.to_string(),
            mock_urls: Vec::new(),
            model_ms: 0,
            runner_error: None,
            conversation_view: String::new(),
        }
    }

    fn pred_default() -> Predicate {
        Predicate::from_toml("", std::path::Path::new("test")).unwrap()
    }

    #[test]
    fn passes_when_no_predicates_set() {
        let s = score("01", &pred_default(), &tx_with_calls(vec![], "hello"));
        assert!(s.pass, "reasons={:?}", s.reasons);
    }

    #[test]
    fn fails_when_should_call_search_true_but_didnt() {
        let mut p = pred_default();
        p.should_call_search = Some(true);
        let s = score("01", &p, &tx_with_calls(vec![], "hello"));
        assert!(!s.pass);
        assert!(s.reasons[0].contains("should_call_search=true"));
    }

    #[test]
    fn passes_when_should_call_search_true_and_did() {
        let mut p = pred_default();
        p.should_call_search = Some(true);
        let tx = tx_with_calls(
            vec![ObservedToolCall {
                name: "search".into(),
                arguments: serde_json::json!({"query": "anything"}),
                turn: 0,
            }],
            "answer",
        );
        let s = score("01", &p, &tx);
        assert!(s.pass, "reasons={:?}", s.reasons);
    }

    #[test]
    fn expected_query_max_tokens_caps_query_length() {
        let mut p = pred_default();
        p.expected_query_max_tokens = Some(3);
        let tx = tx_with_calls(
            vec![ObservedToolCall {
                name: "search".into(),
                arguments: serde_json::json!({"query": "one two three four five"}),
                turn: 0,
            }],
            "",
        );
        let s = score("01", &p, &tx);
        assert!(!s.pass);
        assert!(s.reasons[0].contains("expected_query_max_tokens=3"));
    }

    #[test]
    fn must_cite_url_from_mock_counts_distinct_appearances() {
        let mut p = pred_default();
        p.must_cite_url_from_mock = Some(2);
        let mut tx = tx_with_calls(vec![], "Per [1] (https://example.com/a) and [2] (https://example.com/b)…");
        tx.mock_urls = vec![
            "https://example.com/a".into(),
            "https://example.com/b".into(),
            "https://example.com/c".into(),
        ];
        let s = score("01", &p, &tx);
        assert!(s.pass, "reasons={:?}", s.reasons);
    }

    #[test]
    fn must_not_cite_url_outside_mock_catches_fabrication() {
        let mut p = pred_default();
        p.must_not_cite_url_outside_mock = true;
        let mut tx = tx_with_calls(vec![], "Source: https://hallucinated.example.com/x");
        tx.mock_urls = vec!["https://real.example.com/y".into()];
        let s = score("01", &p, &tx);
        assert!(!s.pass);
        assert!(s.reasons[0].contains("hallucinated"), "reasons={:?}", s.reasons);
    }

    #[test]
    fn extract_urls_handles_trailing_punctuation() {
        let urls = extract_urls("see https://example.com/a, and https://example.com/b.");
        assert_eq!(urls, vec!["https://example.com/a", "https://example.com/b"]);
    }

    #[tokio::test]
    async fn score_with_judge_passes_when_judge_passes() {
        use super::super::judge::FixedVerdictJudge;
        let mut p = pred_default();
        p.final_message_satisfies = vec!["The text apologises".into()];
        let tx = tx_with_calls(vec![], "I'm sorry, I couldn't help with that.");
        let judge = FixedVerdictJudge {
            verdict: Verdict {
                passes: true,
                rationale: "Says 'sorry'.".into(),
            },
        };
        let s = score_with_judge("01", &p, &tx, &judge).await;
        assert!(s.pass, "reasons={:?}", s.reasons);
        assert_eq!(s.judge_verdicts.len(), 1);
        assert!(s.judge_verdicts[0].passes);
    }

    #[tokio::test]
    async fn score_with_judge_records_failure_reasons_and_verdicts() {
        use super::super::judge::FixedVerdictJudge;
        let mut p = pred_default();
        p.final_message_satisfies = vec!["The text apologises".into()];
        let tx = tx_with_calls(vec![], "No, that's correct as-is.");
        let judge = FixedVerdictJudge {
            verdict: Verdict {
                passes: false,
                rationale: "No apology phrasing.".into(),
            },
        };
        let s = score_with_judge("01", &p, &tx, &judge).await;
        assert!(!s.pass);
        assert!(s.reasons[0].contains("final_message_satisfies fail"));
        assert!(s.reasons[0].contains("No apology phrasing"));
        assert_eq!(s.judge_verdicts.len(), 1);
        assert!(!s.judge_verdicts[0].passes);
    }

    #[tokio::test]
    async fn score_with_judge_routes_query_assertion_to_first_search() {
        use super::super::judge::ScriptedJudge;
        let mut p = pred_default();
        p.query_satisfies = vec!["The query names a company by ticker".into()];
        let tx = tx_with_calls(
            vec![ObservedToolCall {
                name: "search".into(),
                arguments: serde_json::json!({"query": "NVDA price"}),
                turn: 0,
            }],
            "answer",
        );
        let judge = ScriptedJudge {
            script: vec![("ticker".into(), true, "Yes, NVDA.".into())],
        };
        let s = score_with_judge("02", &p, &tx, &judge).await;
        assert!(s.pass, "reasons={:?}", s.reasons);
        assert_eq!(s.judge_verdicts[0].predicate, "query_satisfies");
    }

    #[tokio::test]
    async fn score_with_judge_fails_query_assertion_when_no_search() {
        use super::super::judge::FixedVerdictJudge;
        let mut p = pred_default();
        p.query_satisfies = vec!["unreachable".into()];
        let tx = tx_with_calls(vec![], "no search happened");
        let judge = FixedVerdictJudge {
            verdict: Verdict {
                passes: true,
                rationale: "n/a".into(),
            },
        };
        let s = score_with_judge("x", &p, &tx, &judge).await;
        assert!(!s.pass);
        assert!(s.reasons[0].contains("no search call observed"));
    }

    #[tokio::test]
    async fn score_with_judge_skips_judge_when_runner_errored() {
        use super::super::judge::FixedVerdictJudge;
        let mut p = pred_default();
        p.final_message_satisfies = vec!["unused".into()];
        let mut tx = tx_with_calls(vec![], "");
        tx.runner_error = Some("mock fixture missing".into());
        let judge = FixedVerdictJudge {
            verdict: Verdict {
                passes: true,
                rationale: "should never be called".into(),
            },
        };
        let s = score_with_judge("x", &p, &tx, &judge).await;
        assert!(!s.pass);
        assert_eq!(s.reasons.len(), 1);
        assert!(s.reasons[0].contains("runner_error"));
        assert!(s.judge_verdicts.is_empty(), "judge skipped on runner_error");
    }

    #[test]
    fn score_without_judge_warns_on_semantic_predicate() {
        // Structural-only `score()` should surface that a semantic
        // predicate was set but not evaluated, so an operator who
        // ran with --no-judge sees the asymmetry.
        let mut p = pred_default();
        p.final_message_satisfies = vec!["x".into()];
        let s = score("01", &p, &tx_with_calls(vec![], "hi"));
        assert!(!s.pass);
        assert!(s.reasons[0].contains("semantic predicates set"));
    }
}
