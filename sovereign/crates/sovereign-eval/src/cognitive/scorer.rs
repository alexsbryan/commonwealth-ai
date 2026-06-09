// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mechanical scoring for cognitive items.
//!
//! Three kinds for v1:
//! - `exact_match`   — substring search on raw response.
//! - `multi_choice`  — extract `choice_field` from JSON, compare.
//! - `calibration`   — extract `confidence_field` (1-5), compare
//!   against expected truth direction.
//!
//! Each scorer returns an [`Outcome`] that flags pass/fail plus a
//! reason string. Parse failures are themselves a fail outcome —
//! the model not producing valid JSON when the prompt asked for
//! JSON is the kind of signal the fast tier is supposed to surface.

use crate::cognitive::item::{Item, Scoring};
use crate::cognitive::runner::ItemResult;
use corpus_engine::enrichment::pipeline::extract_json_block;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub item_id: String,
    pub category: String,
    pub passed: bool,
    pub reason: String,
    /// Echo of the raw response so the per-item JSONL log is
    /// self-contained.
    pub response_raw: String,
    pub elapsed_ms: u64,
    pub model: String,
    /// Throughput inputs — forwarded from the daemon's `usage`
    /// envelope so the aggregator can compute tok/s without re-walking
    /// the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
}

pub fn score(item: &Item, result: &ItemResult) -> Outcome {
    let base = OutcomeBase {
        item_id: item.item.id.clone(),
        category: item.item.category.as_str().to_string(),
        response_raw: result.response_raw.clone(),
        elapsed_ms: result.elapsed_ms,
        model: result.model.clone(),
        prompt_tokens: result.prompt_tokens,
        completion_tokens: result.completion_tokens,
    };
    if !result.transport_ok {
        return finish(
            base,
            false,
            format!(
                "transport failure: {}",
                result.error.as_deref().unwrap_or("(no error)")
            ),
        );
    }
    match &item.scoring {
        Scoring::ExactMatch {
            expected_substring,
            case_sensitive,
        } => score_exact_match(
            base,
            &result.response_raw,
            expected_substring,
            *case_sensitive,
        ),
        Scoring::MultiChoice {
            expected_choice,
            choice_field,
        } => score_multi_choice(base, &result.response_raw, expected_choice, choice_field),
        Scoring::Calibration {
            expected_truth,
            confidence_field,
            pass_high,
            pass_low,
        } => score_calibration(
            base,
            &result.response_raw,
            *expected_truth,
            confidence_field,
            *pass_high,
            *pass_low,
        ),
        Scoring::ToolUse {
            expected_tool,
            expected_args,
            must_contain,
            expected_sequence,
            alternates_ok,
        } => score_tool_use(
            base,
            &result.response_raw,
            expected_tool.as_deref(),
            expected_args.as_ref(),
            must_contain.as_ref(),
            expected_sequence.as_ref(),
            alternates_ok.as_ref(),
        ),
    }
}

struct OutcomeBase {
    item_id: String,
    category: String,
    response_raw: String,
    elapsed_ms: u64,
    model: String,
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

fn finish(base: OutcomeBase, passed: bool, reason: String) -> Outcome {
    Outcome {
        item_id: base.item_id,
        category: base.category,
        passed,
        reason,
        response_raw: base.response_raw,
        elapsed_ms: base.elapsed_ms,
        model: base.model,
        prompt_tokens: base.prompt_tokens,
        completion_tokens: base.completion_tokens,
    }
}

fn score_exact_match(
    base: OutcomeBase,
    response: &str,
    expected: &str,
    case_sensitive: bool,
) -> Outcome {
    let hit = if case_sensitive {
        response.contains(expected)
    } else {
        response.to_lowercase().contains(&expected.to_lowercase())
    };
    let reason = if hit {
        format!("response contains `{expected}`")
    } else {
        format!("response missing `{expected}`")
    };
    finish(base, hit, reason)
}

fn score_multi_choice(base: OutcomeBase, response: &str, expected: &str, field: &str) -> Outcome {
    let parsed = match parse_json(response) {
        Ok(v) => v,
        Err(e) => return finish(base, false, format!("JSON parse failed: {e}")),
    };
    let got = match parsed.get(field).and_then(serde_json::Value::as_str) {
        Some(s) => s.trim().to_string(),
        None => {
            return finish(
                base,
                false,
                format!("field `{field}` not a string in response"),
            )
        }
    };
    let hit = got.eq_ignore_ascii_case(expected.trim());
    let reason = format!("expected `{expected}`, got `{got}`");
    finish(base, hit, reason)
}

fn score_calibration(
    base: OutcomeBase,
    response: &str,
    expected_truth: bool,
    field: &str,
    pass_high: u32,
    pass_low: u32,
) -> Outcome {
    let parsed = match parse_json(response) {
        Ok(v) => v,
        Err(e) => return finish(base, false, format!("JSON parse failed: {e}")),
    };
    // Boolean-judgment scoring (post-2026-05-20). The runner's
    // response_format constrains Calibration emissions to carry an
    // explicit boolean ahead of `confidence`. The boolean is the
    // judgment carrier; confidence is auxiliary calibration signal.
    //
    // Two field-name eras kept side by side:
    // - `claim_is_true` — current name (2026-05-20+). The
    //   self-describing label avoids the conflation observed on
    //   Qwen3.6, which interpreted `verdict: true` as "I assert my
    //   rationale" rather than "the user's claim is true".
    // - `verdict` — earlier name; kept as a fallback so prior
    //   reports re-score correctly.
    //
    // The legacy Likert-threshold path below is the third fallback
    // for reports that predate the boolean entirely.
    let boolean_judgment = parsed
        .get("claim_is_true")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| parsed.get("verdict").and_then(serde_json::Value::as_bool));
    if let Some(v) = boolean_judgment {
        let hit = v == expected_truth;
        let conf_str = match parsed.get(field) {
            Some(serde_json::Value::Number(n)) => format!(" (confidence={n})"),
            _ => String::new(),
        };
        let reason = format!("claim_is_true={v} expected={expected_truth}{conf_str}");
        return finish(base, hit, reason);
    }
    let conf = match parsed.get(field) {
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(x) => x as u32,
            None => match n.as_f64() {
                Some(f) if f >= 0.0 => f.round() as u32,
                _ => {
                    return finish(
                        base,
                        false,
                        format!("field `{field}` is a non-positive number"),
                    )
                }
            },
        },
        Some(other) => {
            return finish(
                base,
                false,
                format!("field `{field}` is not numeric: {other}"),
            )
        }
        None => return finish(base, false, format!("field `{field}` missing")),
    };
    if !(1..=5).contains(&conf) {
        return finish(
            base,
            false,
            format!("confidence `{conf}` outside 1-5 range"),
        );
    }
    let (hit, reason) = if expected_truth {
        let pass = conf >= pass_high;
        (
            pass,
            format!(
                "claim true; confidence {conf} {} pass_high={pass_high}",
                if pass { ">=" } else { "<" }
            ),
        )
    } else {
        let pass = conf <= pass_low;
        (
            pass,
            format!(
                "claim false; confidence {conf} {} pass_low={pass_low}",
                if pass { "<=" } else { ">" }
            ),
        )
    };
    finish(base, hit, reason)
}

fn score_tool_use(
    base: OutcomeBase,
    response: &str,
    expected_tool: Option<&str>,
    expected_args: Option<&std::collections::BTreeMap<String, String>>,
    must_contain: Option<&Vec<String>>,
    expected_sequence: Option<&Vec<String>>,
    alternates_ok: Option<&Vec<String>>,
) -> Outcome {
    let parsed = match parse_json(response) {
        Ok(v) => v,
        Err(e) => return finish(base, false, format!("JSON parse failed: {e}")),
    };

    if let Some(expected) = expected_sequence {
        let actual = match parsed.get("tools").and_then(serde_json::Value::as_array) {
            Some(arr) => arr
                .iter()
                .filter_map(|v| {
                    v.as_str().map(str::to_string).or_else(|| {
                        v.get("tool")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                })
                .collect::<Vec<_>>(),
            None => {
                return finish(
                    base,
                    false,
                    "expected `tools` array for sequence item".into(),
                )
            }
        };
        let hit = actual == *expected;
        let reason = format!("expected sequence {expected:?}, got {actual:?}");
        return finish(base, hit, reason);
    }

    let chosen = match parsed.get("tool").and_then(serde_json::Value::as_str) {
        Some(s) => s.trim().to_string(),
        None => return finish(base, false, "missing `tool` field in response".into()),
    };

    let Some(expected) = expected_tool else {
        return finish(
            base,
            false,
            "item has neither expected_tool nor expected_sequence".into(),
        );
    };

    let mut acceptable: Vec<String> = vec![expected.to_string()];
    if let Some(alts) = alternates_ok {
        acceptable.extend(alts.iter().cloned());
    }
    let tool_hit = acceptable.iter().any(|t| chosen.eq_ignore_ascii_case(t));
    if !tool_hit {
        return finish(
            base,
            false,
            format!("expected tool one of {acceptable:?} (or `none`), got `{chosen}`"),
        );
    }

    // "none" — no args to check.
    if chosen.eq_ignore_ascii_case("none") {
        return finish(base, true, "correctly chose no-tool".into());
    }

    let actual_args = parsed
        .get("args")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Fuzzy path takes precedence when set: every listed substring must
    // appear (case-insensitive) anywhere in the JSON-encoded args. The
    // catch-all that lets shell-command items pass when flag order or
    // error-handling deltas don't matter, but a load-bearing token is
    // still required.
    if let Some(required) = must_contain {
        let haystack = serde_json::to_string(&actual_args)
            .unwrap_or_default()
            .to_lowercase();
        let missing: Vec<String> = required
            .iter()
            .filter(|needle| !haystack.contains(&needle.to_lowercase()))
            .cloned()
            .collect();
        if missing.is_empty() {
            return finish(
                base,
                true,
                format!("correct tool `{chosen}` — all required tokens present"),
            );
        }
        return finish(
            base,
            false,
            format!("missing required tokens: {}", missing.join(", ")),
        );
    }

    let Some(expected_args) = expected_args else {
        return finish(
            base,
            true,
            format!("correct tool `{chosen}` (no args required)"),
        );
    };

    let mut missing: Vec<String> = Vec::new();
    let mut wrong: Vec<String> = Vec::new();
    for (key, expected_val) in expected_args {
        let actual_val = actual_args.get(key).and_then(serde_json::Value::as_str);
        match actual_val {
            None => missing.push(key.clone()),
            Some(v) if !v.trim().eq_ignore_ascii_case(expected_val.trim()) => {
                wrong.push(format!("{key}: expected `{expected_val}`, got `{v}`"));
            }
            _ => {}
        }
    }
    if missing.is_empty() && wrong.is_empty() {
        finish(
            base,
            true,
            format!("correct tool `{chosen}` with all expected args"),
        )
    } else {
        let mut parts: Vec<String> = Vec::new();
        if !missing.is_empty() {
            parts.push(format!("missing args: {}", missing.join(", ")));
        }
        if !wrong.is_empty() {
            parts.push(format!("wrong args: {}", wrong.join("; ")));
        }
        finish(base, false, parts.join("; "))
    }
}

/// Parse a model response that's supposed to be JSON but may carry
/// reasoning tags, fences, or prose around it.
///
/// Delegates to `corpus_engine::enrichment::pipeline::extract_json_block`
/// — the canonical hardened extractor used across the v2 enrichment
/// pipeline (≈40 call sites). Handles `<think>...</think>`, ` ```json `
/// fences, balanced-brace recovery from prose.
fn parse_json(raw: &str) -> Result<serde_json::Value, String> {
    let block = extract_json_block(raw).ok_or_else(|| {
        format!(
            "no JSON block found (response head: {:?})",
            raw.chars().take(120).collect::<String>()
        )
    })?;
    serde_json::from_str(block).map_err(|e| format!("JSON parse: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> OutcomeBase {
        OutcomeBase {
            item_id: "test".into(),
            category: "decision_quality".into(),
            response_raw: String::new(),
            elapsed_ms: 0,
            model: "m".into(),
            prompt_tokens: None,
            completion_tokens: None,
        }
    }

    #[test]
    fn multi_choice_matches_field() {
        let r = score_multi_choice(base(), r#"{"choice": "A"}"#, "A", "choice");
        assert!(r.passed);
    }

    #[test]
    fn multi_choice_case_insensitive() {
        let r = score_multi_choice(base(), r#"{"choice": "a"}"#, "A", "choice");
        assert!(r.passed);
    }

    #[test]
    fn multi_choice_handles_fenced_json() {
        let r = score_multi_choice(base(), "```json\n{\"choice\": \"B\"}\n```", "B", "choice");
        assert!(r.passed);
    }

    #[test]
    fn multi_choice_handles_prose_wrapped_json() {
        let r = score_multi_choice(
            base(),
            "Sure, here it is: {\"choice\": \"C\"} that's my pick.",
            "C",
            "choice",
        );
        assert!(r.passed);
    }

    #[test]
    fn calibration_true_claim_high_confidence_passes() {
        let r = score_calibration(base(), r#"{"confidence": 5}"#, true, "confidence", 4, 2);
        assert!(r.passed);
    }

    #[test]
    fn calibration_false_claim_low_confidence_passes() {
        let r = score_calibration(base(), r#"{"confidence": 1}"#, false, "confidence", 4, 2);
        assert!(r.passed);
    }

    #[test]
    fn calibration_false_claim_high_confidence_fails() {
        let r = score_calibration(base(), r#"{"confidence": 5}"#, false, "confidence", 4, 2);
        assert!(!r.passed);
    }

    #[test]
    fn calibration_out_of_range_fails() {
        let r = score_calibration(base(), r#"{"confidence": 7}"#, true, "confidence", 4, 2);
        assert!(!r.passed);
    }

    #[test]
    fn calibration_claim_is_true_overrides_confidence_for_judgment() {
        // The 2026-05-20 Likert-conflation failure: rationale identifies
        // the claim as false, but confidence=5 (model treats 5 as
        // strength-of-conviction not likelihood-of-truth). With a
        // boolean judgment field present, that field carries the
        // judgment; confidence becomes auxiliary metadata.
        let response = r#"{
            "rationale": "claim is false; RSA cipher suites lack forward secrecy",
            "claim_is_true": false,
            "confidence": 5
        }"#;
        let r = score_calibration(base(), response, false, "confidence", 4, 2);
        assert!(
            r.passed,
            "claim_is_true=false matches expected_truth=false, must pass despite high confidence"
        );
    }

    #[test]
    fn calibration_claim_is_true_mismatch_fails() {
        let response = r#"{"rationale": "x", "claim_is_true": true, "confidence": 5}"#;
        let r = score_calibration(base(), response, false, "confidence", 4, 2);
        assert!(!r.passed);
    }

    #[test]
    fn calibration_legacy_verdict_field_still_works() {
        // Backward-compat: reports written before the rename
        // (2026-05-20) used `verdict`. Scoring must still respect
        // that label so old reports re-score correctly.
        let response = r#"{"rationale": "x", "verdict": false, "confidence": 5}"#;
        let r = score_calibration(base(), response, false, "confidence", 4, 2);
        assert!(r.passed);
    }

    #[test]
    fn exact_match_case_insensitive_default() {
        let r = score_exact_match(base(), "Hello WORLD", "world", false);
        assert!(r.passed);
    }

    #[test]
    fn multi_choice_through_think_block() {
        let raw = "<think>The answer should be B because BFS is shortest-first.</think>\n{\"choice\": \"B\"}";
        let r = score_multi_choice(base(), raw, "B", "choice");
        assert!(r.passed);
    }

    #[test]
    fn parse_json_returns_err_when_extractor_finds_nothing() {
        assert!(parse_json("just prose, no braces here").is_err());
    }

    fn args_map(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn tool_use_correct_single_call() {
        let r = score_tool_use(
            base(),
            r#"{"tool": "callers", "args": {"name": "reindex_file"}}"#,
            Some("callers"),
            Some(&args_map(&[("name", "reindex_file")])),
            None,
            None,
            None,
        );
        assert!(r.passed, "{}", r.reason);
    }

    #[test]
    fn tool_use_wrong_tool_fails() {
        let r = score_tool_use(
            base(),
            r#"{"tool": "code_search", "args": {"query": "reindex"}}"#,
            Some("callers"),
            Some(&args_map(&[("name", "reindex_file")])),
            None,
            None,
            None,
        );
        assert!(!r.passed);
    }

    #[test]
    fn tool_use_alternate_ok() {
        let r = score_tool_use(
            base(),
            r#"{"tool": "code_search", "args": {"query": "foo"}}"#,
            Some("symbols"),
            None,
            None,
            None,
            Some(&vec!["code_search".to_string()]),
        );
        assert!(r.passed);
    }

    #[test]
    fn tool_use_none_correct() {
        let r = score_tool_use(
            base(),
            r#"{"tool": "none", "rationale": "answer is in prompt"}"#,
            Some("none"),
            None,
            None,
            None,
            None,
        );
        assert!(r.passed);
    }

    #[test]
    fn tool_use_args_missing_key_fails() {
        let r = score_tool_use(
            base(),
            r#"{"tool": "callers", "args": {}}"#,
            Some("callers"),
            Some(&args_map(&[("name", "reindex_file")])),
            None,
            None,
            None,
        );
        assert!(!r.passed);
        assert!(r.reason.contains("missing"));
    }

    #[test]
    fn tool_use_sequence_matches() {
        let r = score_tool_use(
            base(),
            r#"{"tools": ["callers", "symbols"]}"#,
            None,
            None,
            None,
            Some(&vec!["callers".to_string(), "symbols".to_string()]),
            None,
        );
        assert!(r.passed);
    }

    #[test]
    fn tool_use_sequence_wrong_order_fails() {
        let r = score_tool_use(
            base(),
            r#"{"tools": ["symbols", "callers"]}"#,
            None,
            None,
            None,
            Some(&vec!["callers".to_string(), "symbols".to_string()]),
            None,
        );
        assert!(!r.passed);
    }

    #[test]
    fn tool_use_must_contain_passes_loose_shell() {
        // Model's command differs in flag order and adds 2>/dev/null,
        // but the load-bearing tokens are present.
        let r = score_tool_use(
            base(),
            r#"{"tool": "Bash", "args": {"command": "du -sh ~/.sovereign/*/ 2>/dev/null | sort -rh"}}"#,
            Some("Bash"),
            None,
            Some(&vec!["du".into(), "~/.sovereign".into(), "sort".into()]),
            None,
            None,
        );
        assert!(r.passed, "{}", r.reason);
    }

    #[test]
    fn tool_use_must_contain_fails_on_missing_token() {
        // Model invented an unrelated path — required token absent.
        let r = score_tool_use(
            base(),
            r#"{"tool": "Grep", "args": {"path": "/home/user/sovereign-core"}}"#,
            Some("Grep"),
            None,
            Some(&vec!["sovereign/crates".into()]),
            None,
            None,
        );
        assert!(!r.passed);
        assert!(r.reason.contains("missing required"));
    }

    #[test]
    fn tool_use_must_contain_takes_precedence_over_expected_args() {
        // Both are set — must_contain wins; expected_args would otherwise
        // fail this one exactly.
        let r = score_tool_use(
            base(),
            r#"{"tool": "code_search", "args": {"query": "checkpoint resume logic implementation"}}"#,
            Some("code_search"),
            Some(&args_map(&[("query", "checkpoint resume")])),
            Some(&vec!["checkpoint".into(), "resume".into()]),
            None,
            None,
        );
        assert!(r.passed);
    }
}
