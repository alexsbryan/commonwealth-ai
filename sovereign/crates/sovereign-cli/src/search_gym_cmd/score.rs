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

    // ─── Query shape ────────────────────────────────────────────
    if let Some(first_search) = search_calls.first() {
        let query = first_search
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        for needed in &predicate.expected_query_contains {
            if !query.contains(&needed.to_lowercase()) {
                reasons.push(format!(
                    "expected_query_contains={needed:?} missing from query={:?}",
                    first_search.arguments.get("query")
                ));
            }
        }
        for forbidden in &predicate.expected_query_not_contains {
            if query.contains(&forbidden.to_lowercase()) {
                reasons.push(format!(
                    "expected_query_not_contains={forbidden:?} present in query={:?}",
                    first_search.arguments.get("query")
                ));
            }
        }
        if let Some(cap) = predicate.expected_query_max_tokens {
            let token_count = query.split_whitespace().count();
            if token_count > cap {
                reasons.push(format!(
                    "expected_query_max_tokens={cap} but query has {token_count} tokens"
                ));
            }
        }
    } else if !predicate.expected_query_contains.is_empty()
        || !predicate.expected_query_not_contains.is_empty()
        || predicate.expected_query_max_tokens.is_some()
    {
        // Query-shape predicates require a search to have happened.
        // Don't double-report — `should_call_search` already failed
        // above if that's the actual mismatch.
        if predicate.should_call_search != Some(false) {
            reasons.push("query-shape predicates set but no search call observed".into());
        }
    }

    // ─── Result handling ────────────────────────────────────────
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

    for phrase in &predicate.contradiction_phrases {
        if !tx
            .final_message
            .to_lowercase()
            .contains(&phrase.to_lowercase())
        {
            reasons.push(format!(
                "contradiction_phrases requires {phrase:?} in final message but absent"
            ));
        }
    }

    if predicate.zero_results_must_be_acknowledged == Some(true) {
        let msg = tx.final_message.to_lowercase();
        let acknowledged = ["no results", "zero results", "no relevant", "couldn't find", "could not find", "nothing found"]
            .iter()
            .any(|p| msg.contains(p));
        if !acknowledged {
            reasons.push("zero_results_must_be_acknowledged but final message doesn't say so".into());
        }
    }

    // ─── Final-message content ──────────────────────────────────
    let final_lc = tx.final_message.to_lowercase();
    for needed in &predicate.final_message_contains {
        if !final_lc.contains(&needed.to_lowercase()) {
            reasons.push(format!(
                "final_message_contains={needed:?} missing from final message"
            ));
        }
    }
    for forbidden in &predicate.final_message_not_contains {
        if final_lc.contains(&forbidden.to_lowercase()) {
            reasons.push(format!(
                "final_message_not_contains={forbidden:?} present in final message"
            ));
        }
    }

    // ─── Refusal path ───────────────────────────────────────────
    if predicate.must_decline_gracefully == Some(true) {
        if !search_calls.is_empty() {
            reasons.push("must_decline_gracefully but model attempted to search anyway".into());
        }
        let msg = tx.final_message.to_lowercase();
        let declined = ["can't", "cannot", "unable", "not configured", "no api key", "search isn't available"]
            .iter()
            .any(|p| msg.contains(p));
        if !declined {
            reasons.push("must_decline_gracefully but no decline phrasing in final message".into());
        }
    }

    if !predicate.final_message_satisfies.is_empty()
        || !predicate.query_satisfies.is_empty()
    {
        reasons.push(format!(
            "semantic predicates set ({} on final_message, {} on query) but no judge \
             provided — re-run with judge enabled or use score_with_judge",
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

    // Drop the placeholder "no judge provided" reasons we added in
    // score() — we ARE the judge path.
    scored
        .reasons
        .retain(|r| !r.contains("semantic predicates set"));

    for assertion in &predicate.final_message_satisfies {
        match judge.judge(assertion, &tx.final_message).await {
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
    // predicates' policy from §3.3 of the v3 design.
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

/// Discard the `Verdict` type from the public API — the scorer
/// returns `JudgeRecord` which has the same shape but is gym-owned.
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
    fn expected_query_contains_is_case_insensitive() {
        let mut p = pred_default();
        p.should_call_search = Some(true);
        p.expected_query_contains = vec!["SpaceX".into(), "Starship".into()];
        let tx = tx_with_calls(
            vec![ObservedToolCall {
                name: "search".into(),
                arguments: serde_json::json!({"query": "spacex starship flight 12"}),
                turn: 0,
            }],
            "",
        );
        let s = score("01", &p, &tx);
        assert!(s.pass, "reasons={:?}", s.reasons);
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
        use super::super::judge::Verdict;
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
        use super::super::judge::Verdict;
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
        use super::super::runner::ObservedToolCall;
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
        use super::super::judge::Verdict;
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
        use super::super::judge::Verdict;
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

    #[test]
    fn final_message_contains_is_case_insensitive() {
        let mut p = pred_default();
        p.final_message_contains = vec!["96 GB".into(), "unified memory".into()];
        let tx = tx_with_calls(vec![], "Your system has 96 gb of UNIFIED MEMORY, ample for 70B.");
        let s = score("04", &p, &tx);
        assert!(s.pass, "reasons={:?}", s.reasons);
    }

    #[test]
    fn final_message_contains_catches_missing_phrase() {
        let mut p = pred_default();
        p.final_message_contains = vec!["96 GB".into()];
        let tx = tx_with_calls(vec![], "You have plenty of memory.");
        let s = score("04", &p, &tx);
        assert!(!s.pass);
        assert!(s.reasons[0].contains("96 GB"), "reasons={:?}", s.reasons);
    }

    #[test]
    fn final_message_not_contains_catches_forbidden_phrase() {
        let mut p = pred_default();
        p.final_message_not_contains = vec!["I don't know".into()];
        let tx = tx_with_calls(vec![], "Sorry, I don't know the answer.");
        let s = score("04", &p, &tx);
        assert!(!s.pass);
        assert!(s.reasons[0].contains("I don't know"), "reasons={:?}", s.reasons);
    }

    #[test]
    fn aggregate_computes_rate() {
        let scored = vec![
            Scored {
                slug: "x".into(),
                pass: true,
                reasons: vec![],
                runner_error: None,
                model_ms: 100,
                tool_calls_observed: 0,
                judge_verdicts: vec![],
            },
            Scored {
                slug: "x".into(),
                pass: false,
                reasons: vec!["nope".into()],
                runner_error: None,
                model_ms: 200,
                tool_calls_observed: 0,
                judge_verdicts: vec![],
            },
        ];
        let agg = aggregate("x", &scored);
        assert_eq!(agg.passes, 1);
        assert_eq!(agg.replays, 2);
        assert!((agg.rate - 0.5).abs() < 1e-6);
        assert_eq!(agg.mean_model_ms, 150);
    }
}
