// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn mixed_day_and_instant_report_times_are_refused_as_a_partial_order() {
    let close = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05"),
        "closed_by_pr",
        "transition-close",
    );
    let reopen = transition(
        "claim-reopen",
        "doc-reopen",
        Some("2026-10-05T12:00:00Z"),
        "reopen",
        "transition-reopen",
    );
    let lines = replay(DECLARATION, &[close, reopen]).await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "pending");
    assert_eq!(line["values"], json!(["open", "resolved"]));
    assert!(line["protocol"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("mixed precision")));
}

#[tokio::test]
async fn recency_alone_cannot_change_a_transition_identity() {
    let close = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "same-transition",
    );
    let reopen = transition(
        "claim-reopen",
        "doc-reopen",
        Some("2026-10-06T12:00:00Z"),
        "reopen",
        "same-transition",
    );
    let lines = replay(DECLARATION, &[close, reopen]).await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "conflict");
    assert_eq!(line["values"], json!(["open", "resolved"]));
    assert!(line["protocol"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason
            .as_str()
            .unwrap()
            .contains("without an explicit correction")));
}

#[tokio::test]
async fn flattened_values_in_a_legacy_cache_do_not_supply_qualification() {
    let mut legacy = transition(
        "claim-legacy",
        "doc-legacy",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-legacy",
    );
    legacy.field_provenance = false;
    let lines = replay(DECLARATION, &[legacy]).await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "pending");
    assert!(line["protocol"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason
            .as_str()
            .unwrap()
            .contains("provenance is unavailable")));
}
