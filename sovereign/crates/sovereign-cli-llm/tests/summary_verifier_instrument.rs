// SPDX-License-Identifier: AGPL-3.0-or-later
//! The summary verifier's offline instrument: TEST-RETEST + NEGATIVE
//! CONTROL over the PRODUCTION registers (ei7 / RAPTOR, order
//! ei7-prove-raptor; frame 2026-09-22).
//!
//! NOT part of the normal test suite: it drives a live daemon (`#[ignore]`).
//! Run explicitly:
//!
//! ```text
//! SUMMARY_VERIFIER_INPUT=<jsonl> \
//! SUMMARY_VERIFIER_BASE=http://127.0.0.1:9741/v1 \
//! SUMMARY_VERIFIER_MODEL=<judge model id> \
//! cargo test -p sovereign-cli-llm --test summary_verifier_instrument -- --ignored --nocapture
//! ```
//!
//! INPUT — one JSON object per line:
//!   {"cluster_key": "...", "member_texts": ["...", ...]}
//!   (optional "summary": when absent the instrument SYNTHESIZES one from
//!   the members first, so the probe text is abstractive — the shape the
//!   gate exists to judge; floor quote-glue is what the build falls back TO)
//!
//! WHAT IT MEASURES (pre-registered, frame 2026-09-22):
//!   a. TEST-RETEST — verify(summary, own_members) twice; a pass/fail
//!      flip between the two runs is judge noise the conjunction turns
//!      into a lottery (ARCH §7: one run is not a measurement).
//!   b. NEGATIVE CONTROL — verify(summaryA, membersB) for adjacent
//!      disjoint clusters; anything but FAIL means the gate cannot
//!      discriminate its own cluster from a neighbour.
//! Both numbers print and ride the output JSON. The structural change
//! that follows is judged AGAINST these, not against intuition.

use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_tools::summary_verify::{JudgeSummaryVerifier, SummaryVerdict, SummaryVerifier};

#[tokio::test]
#[ignore = "drives a live daemon (SUMMARY_VERIFIER_BASE, default localhost:9741)"]
async fn test_retest_and_negative_control_over_production_registers() {
    let input_path = std::env::var("SUMMARY_VERIFIER_INPUT").unwrap_or_else(|_| {
        panic!("SUMMARY_VERIFIER_INPUT must name the clusters JSONL (see the module doc)")
    });
    let base = std::env::var("SUMMARY_VERIFIER_BASE")
        .unwrap_or_else(|_| "http://127.0.0.1:9741/v1".into());
    let model = std::env::var("SUMMARY_VERIFIER_MODEL").unwrap_or_else(|_| "fast".into());
    // The synth probe text generator — NOT the builder's register; see the
    // module doc. Any faithful abstractive summary of the right members is
    // a valid probe for the VERIFIER's behavior.
    let synth_model =
        std::env::var("SUMMARY_VERIFIER_SYNTH_MODEL").unwrap_or_else(|_| "primary".into());

    #[derive(serde::Deserialize)]
    struct Row {
        cluster_key: String,
        #[serde(default)]
        summary: Option<String>,
        member_texts: Vec<String>,
    }
    let rows: Vec<Row> = std::fs::read_to_string(&input_path)
        .unwrap_or_else(|e| panic!("read {input_path}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("clusters JSONL row"))
        .collect();
    assert!(rows.len() >= 2, "need ≥2 clusters for the negative control");

    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&base, None, &model, 8192));
    let verifier = JudgeSummaryVerifier::new(provider.clone());
    let synth_provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&base, None, &synth_model, 8192));

    // Synthesize the abstractive summary the row did not carry, through the
    // same daemon: one chat completion over the member texts. This is NOT
    // the builder's grammar-constrained register — the instrument measures
    // the VERIFIER, and any faithful abstractive summary of the right
    // members is a valid probe; the negative control pins discrimination.
    async fn synthesize(p: &Arc<dyn InferenceProvider>, members: &[String]) -> String {
        use sovereign_core::types::CompletionRequest;
        let body = members
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        let req = CompletionRequest {
            prompt: format!("Summarize the passages.\n\n{body}"),
            system_message: Some(
                "You write faithful abstractive summaries: paraphrase, name the \
                 people and events, do not quote the passages verbatim."
                    .to_string(),
            ),
            preferred_speed: sovereign_core::types::Speed::Slow,
            max_tokens: Some(384),
            temperature: Some(0.0),
            ..Default::default()
        };
        p.complete(&req).await.expect("synthesis call").text
    }

    let mut table: Vec<serde_json::Value> = Vec::new();
    let mut flips = 0usize;
    let mut pass_any = 0usize;
    let mut could_not_judge = 0usize;
    let mut control_failures = 0usize; // controls that correctly FAILED (both kinds)
    let mut control_pairs = 0usize;

    // Replace the summary's first capitalized word (length > 3) with a
    // foreign name — the smallest corruption that changes WHO the
    // summary is about while leaving its shape intact.
    fn swap_first_proper_noun(summary: &str, foreign: &str) -> String {
        // Pick the MOST FREQUENT capitalized word except the summary's
        // own first token: the misattribution shape is a character
        // consistently called by the wrong name, and the veto's
        // repetition rule is what this control exercises.
        let first = summary
            .split(|c: char| !c.is_alphabetic())
            .find(|w| !w.is_empty())
            .unwrap_or("");
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for w in summary.split(|c: char| !c.is_alphabetic()) {
            if w.chars().count() > 3
                && w.chars().next().is_some_and(char::is_uppercase)
                && w != first
            {
                *counts.entry(w).or_insert(0) += 1;
            }
        }
        let target = counts
            .iter()
            .filter(|(_, n)| **n >= 2)
            .max_by_key(|(_, n)| **n)
            .map(|(w, _)| *w)
            .unwrap_or("");
        assert!(
            !target.is_empty(),
            "no repeated capitalized name found to swap — summary: {summary}"
        );
        let mut out = String::with_capacity(summary.len());
        let mut rest = summary;
        while let Some(idx) = rest.find(target) {
            out.push_str(&rest[..idx]);
            out.push_str(foreign);
            rest = &rest[idx + target.len()..];
        }
        out.push_str(rest);
        out
    }

    for (i, row) in rows.iter().enumerate() {
        let summary = match &row.summary {
            Some(s) => s.clone(),
            None => synthesize(&synth_provider, &row.member_texts).await,
        };
        let v1 = verifier.verify(&summary, &row.member_texts).await;
        let v2 = verifier.verify(&summary, &row.member_texts).await;
        // A None verdict is could-not-judge, not a dropped row (the
        // reviewer's 2026-09-22 finding: the shrinking denominators
        // 6 → 6 → 2 hid the judge's instability). The row rides the table
        // flagged, and the denominator says it.
        let (p1, w1, c1) = match &v1 {
            Some(v) => (
                v.passed(),
                v.whole_summary_violation,
                (v.claims_total, v.claims_unsupported),
            ),
            None => {
                could_not_judge += 1;
                table.push(serde_json::json!({
                    "cluster_key": row.cluster_key,
                    "could_not_judge": "run1: verifier returned None",
                }));
                eprintln!(
                    "[{}] could-not-judge: verifier returned None on run 1",
                    row.cluster_key
                );
                continue;
            }
        };
        let (p2, w2, c2) = match &v2 {
            Some(v) => (
                v.passed(),
                v.whole_summary_violation,
                (v.claims_total, v.claims_unsupported),
            ),
            None => {
                could_not_judge += 1;
                table.push(serde_json::json!({
                    "cluster_key": row.cluster_key,
                    "could_not_judge": "run2: verifier returned None (flip-worthy if run1 answered)",
                    "pass_run1": p1,
                }));
                eprintln!(
                    "[{}] could-not-judge: verifier returned None on run 2 (run 1 answered: {})",
                    row.cluster_key, p1
                );
                continue;
            }
        };
        if p1 != p2 {
            flips += 1;
        }
        if p1 || p2 {
            pass_any += 1;
        }
        // Negative control 1 — CROSS-CLUSTER: this summary against the
        // NEXT cluster's members (wrap). A pass here is a control
        // failure — the gate cannot tell its own cluster from a
        // neighbour's.
        let other = &rows[(i + 1) % rows.len()];
        let ctrl = verifier.verify(&summary, &other.member_texts).await;
        let ctrl_pass = ctrl.as_ref().is_some_and(SummaryVerdict::passed);
        let ctrl_viol = ctrl.as_ref().and_then(|v| v.whole_summary_violation);
        control_pairs += 1;
        if !ctrl_pass {
            control_failures += 1;
        }
        // Negative control 2 — NAME-SWAP corruption: the summary with its
        // first capitalized token replaced by a name from ANOTHER cluster.
        // A probe that cannot catch this has no discrimination at the
        // unit it claims to judge.
        let foreign = rows[(i + 3) % rows.len()]
            .member_texts
            .iter()
            .find_map(|t| {
                t.split(|c: char| !c.is_alphabetic())
                    .find(|w| {
                        w.chars().count() > 3 && w.chars().next().is_some_and(char::is_uppercase)
                    })
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Zarkon".into());
        let swapped = swap_first_proper_noun(&summary, &foreign);
        let corrupt = verifier.verify(&swapped, &row.member_texts).await;
        let corrupt_pass = corrupt.as_ref().is_some_and(SummaryVerdict::passed);
        let corrupt_viol = corrupt.as_ref().and_then(|v| v.whole_summary_violation);
        control_pairs += 1;
        if !corrupt_pass {
            control_failures += 1;
        }
        table.push(serde_json::json!({
            "cluster_key": row.cluster_key,
            "summary_chars": summary.chars().count(),
            "pass_run1": p1,
            "pass_run2": p2,
            "claims1": c1,
            "claims2": c2,
            "whole1": w1,
            "whole2": w2,
            "control_cross_cluster": ctrl_pass,
            "control_cross_cluster_against": other.cluster_key,
            "control_name_swap": corrupt_pass,
            "control_cross_cluster_violation": ctrl_viol,
            "control_name_swap_violation": corrupt_viol,
            "name_swap_token": foreign,
        }));
        eprintln!(
            "[{:<24}] pass1={} pass2={} {} | cross({})={} [want false] | name-swap({})={} [want false]",
            row.cluster_key,
            p1,
            p2,
            if p1 != p2 { "<— FLIP" } else { "" },
            other.cluster_key,
            ctrl_pass,
            foreign,
            corrupt_pass,
        );
    }

    let measured = table.len();
    eprintln!(
        "\n== verifier instrument: {} rows measured · could-not-judge {} · flips {} · pass-any {} · controls correctly failed {}/{}",
        measured, could_not_judge, flips, pass_any, control_failures, control_pairs
    );
    let out = serde_json::json!({
        "rows_measured": measured,
        "could_not_judge": could_not_judge,
        "test_retest_flips": flips,
        "pass_any": pass_any,
        "negative_control_failed_correctly": control_failures,
        "negative_control_pairs": control_pairs,
        "rows": table,
    });
    let out_path = std::env::var("SUMMARY_VERIFIER_OUTPUT")
        .unwrap_or_else(|_| "target/summary-verifier-instrument.json".into());
    std::fs::write(&out_path, serde_json::to_string_pretty(&out).unwrap())
        .unwrap_or_else(|e| panic!("write {out_path}: {e}"));
    eprintln!("wrote {out_path}");
}
