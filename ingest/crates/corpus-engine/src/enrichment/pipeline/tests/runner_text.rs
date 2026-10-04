// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runner tests over pure text helpers (failure classification, response
//! heads, query clamping), split from `tests/runner.rs` (ARCH §3.2).

use super::*;

#[test]
fn classify_phase1_parse_failure_detects_think_truncation() {
    // Unclosed <think> — no answer past the reasoning trace.
    // Even if the serde error looks like generic parse drift,
    // the structured classifier calls this ThinkTruncated.
    let response = "<think>the model ran out of budget";
    let err = Error::Serialization("no JSON".into());
    assert_eq!(
        classify_phase1_parse_failure(response, &err),
        PhaseFailureKind::ThinkTruncated
    );
}

#[test]
fn classify_phase1_parse_failure_detects_empty_extraction() {
    // Well-formed envelope but no atoms — the pipeline parser
    // signals this with a distinctive "did not extract" error
    // string. The classifier maps it to EmptyExtraction so the
    // CLI can route to prompt-fix work, not a plain retry.
    let response = "{\"section_id\":\"sec_0001\"}";
    let err = Error::Serialization("phase 1 (atlas) did not extract anything usable".into());
    assert_eq!(
        classify_phase1_parse_failure(response, &err),
        PhaseFailureKind::EmptyExtraction
    );
}

#[test]
fn classify_phase1_parse_failure_defaults_to_parse_drift() {
    // Anything else — malformed JSON, schema validation fail —
    // is generic parse drift. A plain retry is the right first
    // move for these.
    let response = "<think>done</think>\n{malformed json";
    let err = Error::Serialization("missing field `foo` at line 3".into());
    assert_eq!(
        classify_phase1_parse_failure(response, &err),
        PhaseFailureKind::ParseDrift
    );
}

#[test]
fn truncate_response_head_returns_none_on_whitespace() {
    assert_eq!(truncate_response_head("   "), None);
    assert_eq!(truncate_response_head(""), None);
}

#[test]
fn truncate_response_head_prefers_post_think_content() {
    // Simulates the common failure: thinking preamble + malformed
    // JSON. We want to see the JSON, not the reasoning.
    let raw = format!(
        "<think>{}</think>\n{{\"questions\": [\"?\"], \"oops: missing_brace\"",
        "reasoning text ".repeat(500) // ~7.5 KB of think
    );
    let head = truncate_response_head(&raw).expect("has content");
    assert!(
        head.contains("oops: missing_brace"),
        "expected post-think JSON candidate, got: {head}"
    );
    assert!(
        !head.contains("reasoning text"),
        "reasoning preamble leaked into head: {head}"
    );
}

#[test]
fn truncate_response_head_flags_truncated_thinking() {
    // <think> opened but never closed — no answer was emitted.
    let raw = format!("<think>{}", "half a thought ".repeat(200));
    let head = truncate_response_head(&raw).expect("has content");
    assert!(
        head.starts_with("<think block truncated"),
        "expected truncation marker, got: {head}"
    );
}

#[test]
fn truncate_response_head_caps_post_think_content() {
    let raw = format!(
        "<think>short</think>{}",
        "J".repeat(FAILURE_HEAD_CHAR_CAP * 2)
    );
    let head = truncate_response_head(&raw).expect("has content");
    // Head is capped and carries the "+N chars" marker.
    assert!(head.contains("+"), "expected drop marker, got: {head}");
    assert!(head.chars().count() <= FAILURE_HEAD_CHAR_CAP + 64);
}

#[test]
fn truncate_response_head_passes_short_response_through_unchanged() {
    let raw = "not valid json";
    assert_eq!(truncate_response_head(raw).unwrap(), "not valid json");
}

#[test]
fn phase1_query_text_clamps_to_budget() {
    let body = "x".repeat(5000);
    let ch = chapter("ch", "Title", &body);
    let q = phase1_query_text(&ch);
    // Title (5) + "\n\n" (2) + 800 chars of body = 807. Allow some
    // slack for char vs byte counting.
    assert!(q.chars().count() <= 810);
    assert!(q.starts_with("Title"));
}
