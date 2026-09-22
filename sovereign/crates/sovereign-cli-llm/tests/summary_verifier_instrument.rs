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
use sovereign_tools::summary_verify::{JudgeSummaryVerifier, SummaryVerifier};

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
    let mut control_failures = 0usize; // MUST equal rows.len()-1 pairs... see below
    let mut control_pairs = 0usize;

    for (i, row) in rows.iter().enumerate() {
        let summary = match &row.summary {
            Some(s) => s.clone(),
            None => synthesize(&synth_provider, &row.member_texts).await,
        };
        let v1 = verifier.verify(&summary, &row.member_texts).await;
        let v2 = verifier.verify(&summary, &row.member_texts).await;
        let (p1, p2, claims1, claims2) = match (v1, v2) {
            (Some(a), Some(b)) => {
                let (ca, cb) = (
                    (a.claims_total, a.claims_unsupported),
                    (b.claims_total, b.claims_unsupported),
                );
                (a.passed(), b.passed(), Some(ca), Some(cb))
            }
            _ => {
                eprintln!(
                    "[{}] verifier returned None (judge unreachable) — row not measured",
                    row.cluster_key
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
        // Negative control: this summary against the NEXT cluster's
        // members (wrap). A pass here is a control FAILURE — the gate
        // cannot tell its own cluster from a neighbour's.
        let other = &rows[(i + 1) % rows.len()];
        let ctrl = verifier.verify(&summary, &other.member_texts).await;
        let ctrl_pass = matches!(ctrl, Some(v) if v.passed());
        control_pairs += 1;
        if !ctrl_pass {
            control_failures += 1; // named confusingly below as "correctly failed"
        }
        table.push(serde_json::json!({
            "cluster_key": row.cluster_key,
            "summary_chars": summary.chars().count(),
            "pass_run1": p1,
            "pass_run2": p2,
            "claims1": claims1,
            "claims2": claims2,
            "control_against": other.cluster_key,
            "control_passed": ctrl_pass,
        }));
        eprintln!(
            "[{:<24}] pass1={} pass2={} {} | control({})={} [want false]",
            row.cluster_key,
            p1,
            p2,
            if p1 != p2 { "<— FLIP" } else { "" },
            other.cluster_key,
            ctrl_pass,
        );
    }

    let measured = table.len();
    eprintln!(
        "\n== verifier instrument: {} rows measured · flips {} · pass-any {} · controls correctly failed {}/{}",
        measured, flips, pass_any, control_failures, control_pairs
    );
    let out = serde_json::json!({
        "rows_measured": measured,
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
