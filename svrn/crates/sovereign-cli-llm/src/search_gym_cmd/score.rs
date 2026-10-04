// SPDX-License-Identifier: AGPL-3.0-or-later
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

use super::predicate::Predicate;
use super::runner::Transcript;
use crate::gym_judge::{Judge, Verdict};

/// Number of independent judge trials to run per semantic assertion.
/// 3 lets us take a majority vote: 2-1 or 3-0 either way. Empirical
/// motivation: the judge model (Qwen3.5-9B) produces different
/// verdicts on the same input across calls — same prompt, same
/// model, different output. Observed 2026-05-19 Phase 3c iter6→snap:
/// fixtures 06 and 10 dropped from 5/5 to 2/3 with identical model
/// outputs across replays — pure judge variance. Consensus voting
/// across N=3 trials reduces this noise (Bayes: 3 independent 80%-
/// accurate verdicts give ~89% majority agreement with truth).
///
/// Cost: ~3× judge time per assertion. The fast slot's single-permit
/// semaphore serialises them anyway, so parallelism doesn't help —
/// we just pay 3× the call time. Acceptable trade for cleaner signal.
pub const JUDGE_N_TRIALS: usize = 3;

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
    /// Full transcript captured during this replay: tool_calls,
    /// mock_urls, final_message, conversation_view. Carried into the
    /// `--json` output so an operator can root-cause a failure
    /// without re-running the gym (and without manual curl probing
    /// of the daemon). Human-readable render reaches into this for
    /// the failure-detail section below the summary table.
    pub transcript: Transcript,
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
            transcript: tx.clone(),
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
            reasons.push("expected_query_max_tokens set but no search call observed".into());
        }
    }

    // ─── Result handling (URL set membership) ───────────────────
    if let Some(min) = predicate.must_cite_url_from_mock {
        // A "citation" of mock_urls[i] is satisfied by EITHER:
        //   1. The URL string appearing verbatim in the final message
        //   2. An academic-style numeric marker `[N]` (or `[N, M, ...]`)
        //      where N is a 1-based index into mock_urls. Models
        //      routinely cite results by number rather than inline-URL
        //      and we want the predicate to accept that — the load-
        //      bearing question is "did the model use the results",
        //      not "did it write out the literal URL string".
        // 2026-05-19 Phase 3c iter2 motivation: model emitted
        // `"$872.43, +1.27% for the day [1]. After-hours dipped [2]."`
        // and was failing the predicate even though [1]/[2] mapped to
        // genuine mock_urls. The old check only counted form 1.
        let cited = count_mock_citations(&tx.final_message, &tx.mock_urls);
        if cited < min {
            reasons.push(format!(
                "must_cite_url_from_mock={min} but only {cited} mock URL(s) appeared in final message (checked inline URL + numeric [N] markers)"
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
    if !predicate.final_message_satisfies.is_empty() || !predicate.query_satisfies.is_empty() {
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
        transcript: tx.clone(),
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
        match judge_consensus(judge, assertion, final_subject, JUDGE_N_TRIALS).await {
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
        match judge_consensus(judge, assertion, subject, JUDGE_N_TRIALS).await {
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

/// Run the judge `n` times on the same `(assertion, subject)` and
/// take a majority vote. The judge model (especially smaller / faster
/// ones like Qwen3.5-9B) produces meaningfully different verdicts
/// run-to-run on borderline cases; a single call is too noisy a
/// signal for fixtures we care about pinning at ≥90% pass rate.
/// Three trials with majority vote: 2-1 or 3-0 either way.
///
/// Returns the consensus verdict with a rationale that includes the
/// vote breakdown (`"2/3 judges passed: <rationale from a passing trial>"`)
/// so operators reading the JSON output can see WHICH way the judges
/// split, not just the final yes/no.
///
/// Error handling: if at least a majority of trials succeeded, we
/// return their consensus and silently ignore the errored trial(s).
/// Only when a majority of trials erred do we surface the error —
/// at that point the judge is broken, not just noisy.
async fn judge_consensus(
    judge: &dyn Judge,
    assertion: &str,
    subject: &str,
    n: usize,
) -> Result<Verdict, String> {
    let mut passes = 0usize;
    let mut fails = 0usize;
    let mut errors = 0usize;
    let mut rationale_pass: Option<String> = None;
    let mut rationale_fail: Option<String> = None;
    let mut last_error: Option<String> = None;

    for _ in 0..n {
        match judge.judge(assertion, subject).await {
            Ok(v) => {
                if v.passes {
                    passes += 1;
                    if rationale_pass.is_none() {
                        rationale_pass = Some(v.rationale);
                    }
                } else {
                    fails += 1;
                    if rationale_fail.is_none() {
                        rationale_fail = Some(v.rationale);
                    }
                }
            }
            Err(e) => {
                errors += 1;
                last_error = Some(e);
            }
        }
    }

    let successful = passes + fails;
    if successful == 0 {
        return Err(last_error.unwrap_or_else(|| "all judge trials failed".to_string()));
    }
    let consensus_passes = passes > successful / 2;
    let total = passes + fails + errors;
    let rationale_root = if consensus_passes {
        rationale_pass.unwrap_or_default()
    } else {
        rationale_fail.unwrap_or_default()
    };
    let err_suffix = if errors > 0 {
        format!(" ({errors}/{total} trials errored)")
    } else {
        String::new()
    };
    let verdict_count = if consensus_passes { passes } else { fails };
    Ok(Verdict {
        passes: consensus_passes,
        rationale: format!(
            "{verdict_count}/{total} judges {} — {rationale_root}{err_suffix}",
            if consensus_passes { "passed" } else { "failed" }
        ),
    })
}

/// Aggregate result across N replays of one fixture. Mirrors the
/// code gym's `pass_count / N` shape, plus per-replay records so the
/// `--json` output is self-contained for post-hoc analysis.
#[derive(Debug, Clone, Serialize)]
pub struct AggregatedFixture {
    pub slug: String,
    pub replays: usize,
    pub passes: usize,
    pub rate: f32,
    /// Deduplicated reasons across all replays — useful for binning
    /// failure modes without flooding the report. Per-replay reasons
    /// (with transcripts) live on `replay_records`.
    pub failure_reasons: Vec<String>,
    pub mean_model_ms: u128,
    /// Every scored replay, in order. Each record carries its own
    /// transcript (tool_calls, mock_urls, final_message,
    /// conversation_view) so an operator reading the JSON can see
    /// exactly what the model did on every replay without re-running.
    pub replay_records: Vec<Scored>,
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
        replay_records: scored.to_vec(),
    }
}

/// Render the human-readable summary table. Format mirrors the code
/// gym's output so operators don't context-switch between gyms. When
/// any fixture has failing replays, a per-replay "failure detail"
/// section follows the table — listing each failing replay's reasons,
/// the model's final message (truncated), and any search queries
/// emitted. Operator goal: reading this output is sufficient to
/// root-cause a failure without manual curl probing of the daemon.
pub fn render_table(aggregates: &[AggregatedFixture]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:>9} {:>6}   fail reasons (first 3)\n",
        "fixture", "pass", "rate"
    ));
    out.push_str(&format!(
        "{:-<40} {:->9} {:->6}   {:-<40}\n",
        "", "", "", ""
    ));
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

    let has_failures = aggregates.iter().any(|a| a.passes < a.replays);
    if has_failures {
        out.push_str(&render_failure_detail(aggregates));
    }
    out
}

/// Build the per-replay failure breakdown. One block per fixture with
/// any failing replay; within each block, one entry per failing replay
/// showing its reasons, the first search query (if any), and a
/// 240-char preview of the final message. The full transcript is in
/// the `--json` output for deeper forensics.
fn render_failure_detail(aggregates: &[AggregatedFixture]) -> String {
    let mut out = String::new();
    out.push_str("\n── failure detail ───────────────────────────────────\n");
    for a in aggregates {
        if a.passes == a.replays {
            continue;
        }
        out.push_str(&format!(
            "\n{}  ({}/{} passed, {:.0}%)\n",
            a.slug,
            a.passes,
            a.replays,
            a.rate * 100.0
        ));
        for (i, s) in a.replay_records.iter().enumerate() {
            if s.pass {
                continue;
            }
            out.push_str(&format!("  replay {}:\n", i + 1));
            for r in &s.reasons {
                out.push_str(&format!("    • {r}\n"));
            }
            // First search query the model emitted, if any. Names the
            // judiciousness/shape failure mode at a glance.
            if let Some(first_search) = s
                .transcript
                .tool_calls
                .iter()
                .find(|tc| tc.name == "search" || tc.name == "web_search")
            {
                let q = first_search
                    .arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !q.is_empty() {
                    out.push_str(&format!("    query: {q:?}\n"));
                }
            }
            // Final message preview — most useful signal for
            // synthesis / citation failures.
            let final_msg = &s.transcript.final_message;
            if !final_msg.is_empty() {
                let preview: String = final_msg.chars().take(240).collect();
                let truncated = final_msg.chars().count() > 240;
                out.push_str(&format!(
                    "    final: {preview:?}{}\n",
                    if truncated { " …(truncated)" } else { "" }
                ));
            }
        }
    }
    out
}

/// Count distinct `mock_urls` cited in `final_message`. Two forms
/// count as citation: (1) verbatim URL anywhere in the message,
/// (2) academic-style `[N]` markers (or `[N, M]` lists) where N is a
/// 1-based index into mock_urls. False-positive risk is bounded by
/// `mock_urls.len()` (≤5 in current fixtures) — acceptable trade
/// for not penalising natural citation patterns.
fn count_mock_citations(final_message: &str, mock_urls: &[String]) -> usize {
    use std::collections::HashSet;
    let mut cited: HashSet<usize> = HashSet::new();

    for (i, url) in mock_urls.iter().enumerate() {
        if final_message.contains(url.as_str()) {
            cited.insert(i);
        }
    }

    // Bracketed-marker scan. Tracks bracketed groups, splits on
    // commas/whitespace, validates each integer against mock_urls.len().
    // Also accepts pandoc footnote syntax `[^N]` — same semantic
    // intent as `[N]`, just a different convention. The `^` after
    // `[` is treated as an optional sigil; everything else parses
    // identically.
    let bytes = final_message.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let mut j = i + 1;
            // Optional pandoc-style `^` sigil.
            if j < bytes.len() && bytes[j] == b'^' {
                j += 1;
            }
            let mut nums: Vec<usize> = Vec::new();
            let mut cur: usize = 0;
            let mut have_digit = false;
            while j < bytes.len() && bytes[j] != b']' {
                let c = bytes[j];
                if c.is_ascii_digit() {
                    cur = cur.saturating_mul(10).saturating_add((c - b'0') as usize);
                    have_digit = true;
                } else if c == b',' || c == b' ' {
                    if have_digit {
                        nums.push(cur);
                        cur = 0;
                        have_digit = false;
                    }
                } else {
                    // Non-numeric, non-separator inside the brackets —
                    // this isn't a citation marker (could be e.g.
                    // `[note]` or `[citation needed]`). Abandon.
                    nums.clear();
                    break;
                }
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b']' {
                if have_digit {
                    nums.push(cur);
                }
                for n in &nums {
                    if *n >= 1 && *n <= mock_urls.len() {
                        cited.insert(n - 1);
                    }
                }
                i = j + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    cited.len()
}

fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["http://", "https://"] {
        let mut search_from = 0usize;
        while let Some(idx) = text[search_from..].find(marker) {
            let abs = search_from + idx;
            // URL terminators: whitespace + a broad punctuation set
            // including markdown brackets. The bracket additions
            // (`[`, `]`) catch the `[label](url)` markdown link form
            // where the label is itself a URL — without them the
            // extractor walks past the `]` and concatenates two URLs
            // into one garbage string (observed 2026-05-19 Phase 3c
            // iter9 fixture 08: model emitted
            // `[https://x](https://x)` and the extractor produced
            // `https://x](https://x` which never matches mock_urls).
            let end = text[abs..]
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, ',' | '<' | '>' | ')' | '(' | '[' | ']' | '"' | '\'')
                })
                .map(|n| abs + n)
                .unwrap_or(text.len());
            // Trim trailing common-punctuation that gets picked up by
            // citation patterns: a period at the end of a sentence,
            // a closing parenthesis the URL was wrapped in, etc.
            let mut url = text[abs..end].trim_end_matches(['.', ',', ')', ']']);
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
    use super::super::runner::ObservedToolCall;
    use super::*;

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
        let mut tx = tx_with_calls(
            vec![],
            "Per [1] (https://example.com/a) and [2] (https://example.com/b)…",
        );
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
        assert!(
            s.reasons[0].contains("hallucinated"),
            "reasons={:?}",
            s.reasons
        );
    }

    #[test]
    fn count_mock_citations_inline_url() {
        let urls = vec![
            "https://a.test/x".to_string(),
            "https://b.test/y".to_string(),
        ];
        assert_eq!(
            count_mock_citations("see https://a.test/x for context.", &urls),
            1
        );
        assert_eq!(
            count_mock_citations("see https://a.test/x and https://b.test/y", &urls),
            2
        );
    }

    #[test]
    fn count_mock_citations_numeric_markers() {
        let urls = vec![
            "https://a.test/x".to_string(),
            "https://b.test/y".to_string(),
            "https://c.test/z".to_string(),
        ];
        assert_eq!(count_mock_citations("the price is X [1].", &urls), 1);
        assert_eq!(count_mock_citations("see [1] and [2]", &urls), 2);
        assert_eq!(count_mock_citations("multi-cite [1, 3]", &urls), 2);
        assert_eq!(count_mock_citations("multi-cite [1,2,3]", &urls), 3);
    }

    #[test]
    fn count_mock_citations_combines_forms() {
        let urls = vec![
            "https://a.test/x".to_string(),
            "https://b.test/y".to_string(),
        ];
        // Inline URL covers index 0; [2] covers index 1.
        let s = "https://a.test/x says X, and footnote [2] adds Y.";
        assert_eq!(count_mock_citations(s, &urls), 2);
    }

    #[test]
    fn count_mock_citations_rejects_out_of_range_markers() {
        let urls = vec![
            "https://a.test/x".to_string(),
            "https://b.test/y".to_string(),
        ];
        // [5] and [99] don't map to any mock_url — must NOT count.
        assert_eq!(count_mock_citations("claim [5] and [99]", &urls), 0);
    }

    #[test]
    fn count_mock_citations_ignores_non_numeric_brackets() {
        let urls = vec!["https://a.test/x".to_string()];
        // [note], [citation needed], [stocks] etc. — not citation markers.
        assert_eq!(count_mock_citations("see [note] for context.", &urls), 0);
        assert_eq!(count_mock_citations("[citation needed]", &urls), 0);
    }

    /// Test-only judge that returns verdicts from a predetermined
    /// sequence — one verdict per call, advancing through the slice.
    /// Lets us stage 2-1 / 3-0 / 1-2 vote splits to exercise the
    /// consensus logic.
    struct SequencedJudge {
        verdicts: Vec<Result<Verdict, String>>,
        counter: std::sync::atomic::AtomicUsize,
    }

    impl SequencedJudge {
        fn new(verdicts: Vec<Result<Verdict, String>>) -> Self {
            Self {
                verdicts,
                counter: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Judge for SequencedJudge {
        async fn judge(&self, _assertion: &str, _subject: &str) -> Result<Verdict, String> {
            let i = self
                .counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.verdicts
                .get(i)
                .cloned()
                .unwrap_or_else(|| Err(format!("SequencedJudge: no verdict at index {i}")))
        }
    }

    #[tokio::test]
    async fn judge_consensus_3_pass_returns_pass() {
        let j = SequencedJudge::new(vec![
            Ok(Verdict {
                passes: true,
                rationale: "yes".into(),
            }),
            Ok(Verdict {
                passes: true,
                rationale: "indeed".into(),
            }),
            Ok(Verdict {
                passes: true,
                rationale: "correct".into(),
            }),
        ]);
        let v = judge_consensus(&j, "assertion", "subject", 3)
            .await
            .unwrap();
        assert!(v.passes);
        assert!(
            v.rationale.contains("3/3 judges passed"),
            "got: {}",
            v.rationale
        );
    }

    #[tokio::test]
    async fn judge_consensus_2_pass_1_fail_returns_pass() {
        let j = SequencedJudge::new(vec![
            Ok(Verdict {
                passes: true,
                rationale: "yes".into(),
            }),
            Ok(Verdict {
                passes: false,
                rationale: "no".into(),
            }),
            Ok(Verdict {
                passes: true,
                rationale: "yes again".into(),
            }),
        ]);
        let v = judge_consensus(&j, "a", "s", 3).await.unwrap();
        assert!(v.passes);
        assert!(
            v.rationale.contains("2/3 judges passed"),
            "got: {}",
            v.rationale
        );
    }

    #[tokio::test]
    async fn judge_consensus_1_pass_2_fail_returns_fail() {
        let j = SequencedJudge::new(vec![
            Ok(Verdict {
                passes: false,
                rationale: "no".into(),
            }),
            Ok(Verdict {
                passes: true,
                rationale: "yes".into(),
            }),
            Ok(Verdict {
                passes: false,
                rationale: "still no".into(),
            }),
        ]);
        let v = judge_consensus(&j, "a", "s", 3).await.unwrap();
        assert!(!v.passes);
        assert!(
            v.rationale.contains("2/3 judges failed"),
            "got: {}",
            v.rationale
        );
    }

    #[tokio::test]
    async fn judge_consensus_3_fail_returns_fail() {
        let j = SequencedJudge::new(vec![
            Ok(Verdict {
                passes: false,
                rationale: "no".into(),
            }),
            Ok(Verdict {
                passes: false,
                rationale: "no".into(),
            }),
            Ok(Verdict {
                passes: false,
                rationale: "no".into(),
            }),
        ]);
        let v = judge_consensus(&j, "a", "s", 3).await.unwrap();
        assert!(!v.passes);
        assert!(v.rationale.contains("3/3 judges failed"));
    }

    #[tokio::test]
    async fn judge_consensus_tolerates_one_error_with_majority() {
        let j = SequencedJudge::new(vec![
            Ok(Verdict {
                passes: true,
                rationale: "yes".into(),
            }),
            Err("transient".into()),
            Ok(Verdict {
                passes: true,
                rationale: "yes2".into(),
            }),
        ]);
        let v = judge_consensus(&j, "a", "s", 3).await.unwrap();
        assert!(v.passes);
        assert!(
            v.rationale.contains("2/3 judges passed") && v.rationale.contains("1/3 trials errored"),
            "got: {}",
            v.rationale
        );
    }

    #[tokio::test]
    async fn judge_consensus_all_errors_surfaces_error() {
        let j = SequencedJudge::new(vec![Err("e1".into()), Err("e2".into()), Err("e3".into())]);
        let err = judge_consensus(&j, "a", "s", 3).await.unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn count_mock_citations_accepts_pandoc_footnotes() {
        let urls = vec![
            "https://a.test/x".to_string(),
            "https://b.test/y".to_string(),
        ];
        assert_eq!(count_mock_citations("claim [^1]", &urls), 1);
        assert_eq!(count_mock_citations("two [^1] and [^2]", &urls), 2);
        // `[^N]` and `[N]` for the same index should dedupe.
        assert_eq!(count_mock_citations("[^1] then [1]", &urls), 1);
    }

    #[test]
    fn count_mock_citations_dedupes_same_index_cited_twice() {
        let urls = vec!["https://a.test/x".to_string()];
        // Both forms point to index 0 — count once.
        assert_eq!(
            count_mock_citations("inline https://a.test/x and marker [1]", &urls),
            1
        );
    }

    #[test]
    fn extract_urls_handles_trailing_punctuation() {
        let urls = extract_urls("see https://example.com/a, and https://example.com/b.");
        assert_eq!(urls, vec!["https://example.com/a", "https://example.com/b"]);
    }

    #[test]
    fn extract_urls_splits_markdown_link_form() {
        // [label](url) where the label is itself a URL — without
        // bracket terminators, the extractor concatenated both halves
        // into a garbage string. Phase 3c iter9 regression test.
        let urls = extract_urls("price [[https://a.test/x](https://a.test/x)] today");
        assert_eq!(urls, vec!["https://a.test/x", "https://a.test/x"]);
    }

    #[test]
    fn extract_urls_handles_paren_wrap() {
        let urls = extract_urls("see (https://a.test/x) for details");
        assert_eq!(urls, vec!["https://a.test/x"]);
    }

    #[tokio::test]
    async fn score_with_judge_passes_when_judge_passes() {
        use crate::gym_judge::FixedVerdictJudge;
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
        use crate::gym_judge::FixedVerdictJudge;
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
        use crate::gym_judge::ScriptedJudge;
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
        use crate::gym_judge::FixedVerdictJudge;
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
        use crate::gym_judge::FixedVerdictJudge;
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
    fn scored_carries_transcript_into_json() {
        // Phase 3a propagation: the gym's --json output must include
        // every replay's full transcript so an operator can root-cause
        // a failure without re-running. Pin both shape (transcript
        // present on the serialised Scored) and contents (tool_calls
        // and final_message round-trip).
        let tx = tx_with_calls(
            vec![ObservedToolCall {
                name: "search".into(),
                arguments: serde_json::json!({"query": "NVDA price"}),
                turn: 0,
            }],
            "Per [1] (https://example.com/a), NVDA closed at $X.",
        );
        let mut p = pred_default();
        p.should_call_search = Some(true);
        let s = score("01", &p, &tx);
        let v = serde_json::to_value(&s).unwrap();

        assert!(
            v.pointer("/transcript/tool_calls/0/name")
                .and_then(|n| n.as_str())
                == Some("search"),
            "transcript not in serialised Scored: {v:#}"
        );
        assert!(
            v.pointer("/transcript/final_message")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .contains("NVDA"),
            "final_message lost from transcript: {v:#}"
        );
    }

    #[test]
    fn aggregate_carries_replay_records() {
        // Phase 3a: the per-fixture aggregate must expose each
        // scored replay (with its own transcript) so JSON consumers
        // get the full per-replay payload, not just dedup'd reasons.
        let tx = tx_with_calls(vec![], "hi");
        let p = pred_default();
        let scored = vec![score("01", &p, &tx), score("01", &p, &tx)];
        let agg = aggregate("01_fixture", &scored);
        assert_eq!(agg.replay_records.len(), 2);
        // Round-trip via JSON so we catch any skip_serializing slip.
        let v = serde_json::to_value(&agg).unwrap();
        assert!(
            v.pointer("/replay_records/0/transcript").is_some(),
            "aggregated record dropped transcript: {v:#}"
        );
    }

    #[test]
    fn render_table_includes_failure_detail_for_failing_fixtures() {
        // Phase 3a: human render must surface per-replay reasons +
        // a query/final preview for each failing fixture below the
        // summary table. Operators reading stderr (no --json) should
        // be able to diagnose without re-running.
        let mut p = pred_default();
        p.should_call_search = Some(true);
        let tx_fail = tx_with_calls(vec![], "I don't think I need to search.");
        let tx_pass_query = {
            let mut tx = tx_with_calls(
                vec![ObservedToolCall {
                    name: "search".into(),
                    arguments: serde_json::json!({"query": "bond yields today"}),
                    turn: 0,
                }],
                "ok",
            );
            tx.mock_urls = vec!["https://example.com".into()];
            tx
        };
        let scored = vec![score("x", &p, &tx_fail), score("x", &p, &tx_pass_query)];
        let agg = aggregate("01_temporal_news", &scored);
        let rendered = render_table(&[agg]);

        assert!(rendered.contains("failure detail"), "no detail section");
        assert!(
            rendered.contains("01_temporal_news"),
            "fixture slug not in detail"
        );
        assert!(
            rendered.contains("should_call_search=true"),
            "reason not in detail"
        );
        // The passing replay should NOT show up in the detail block —
        // detail is per-failure, not per-replay.
        assert!(
            !rendered.contains("bond yields today"),
            "passing replay leaked into detail"
        );
    }

    #[test]
    fn render_table_omits_failure_detail_when_all_pass() {
        // Negative: no detail section when there's nothing to detail.
        let tx = tx_with_calls(vec![], "all good");
        let scored = vec![score("x", &pred_default(), &tx)];
        let agg = aggregate("01_clean", &scored);
        let rendered = render_table(&[agg]);
        assert!(!rendered.contains("failure detail"));
    }
}
