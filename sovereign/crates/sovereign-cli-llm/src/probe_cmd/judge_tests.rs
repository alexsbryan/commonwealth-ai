// SPDX-License-Identifier: AGPL-3.0-or-later
//! The same-verdict proof for the assess and judge probes (phase-b-63,
//! pre-registered in the commit that adds the modes, before any run): on one
//! fixture set covering every op kind, against one deterministic daemon, the
//! probe's answers after the JSON round trip equal what the in-process
//! primitives answer to the calls the bench made before. Bar: 0 mismatches.

use axum::routing::post;
use axum::Router;
use serde_json::Value;
use sovereign_core::runtime::{
    assess_asserted_value, chunk_judge_prompt, claim_chunk_support, claim_extraction_prompt,
    extract_claim_list, forced_choice_ab, grounding_gate_threshold, released_pure_decline,
    value_present_in_chunks, AssertedValue, JudgeCall, JudgeRouting, CHUNK_JUDGE_SYSTEM,
    CLAIM_EXTRACTION_SYSTEM,
};

use super::*;

/// A daemon whose every answer is a function of the request: a forced-choice
/// pass gets a distribution keyed by the request body's length, anything
/// else a claim sentence keyed the same way.
async fn deterministic_daemon() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let text = body.to_string();
            let content = if text.contains("x_forced_choice") {
                let a = (text.len() % 7 + 1) as f64 / 10.0;
                serde_json::json!({ "A": a, "B": 1.0 - a }).to_string()
            } else {
                // Keyed by the body too, so a changed prompt or parameter
                // changes the answer and cannot pass unseen.
                format!("The capital of France is Paris, {}.", text.len() % 97)
            };
            axum::Json(serde_json::json!({
                "id": "x", "object": "chat.completion", "created": 0, "model": "m",
                "choices": [{ "index": 0, "finish_reason": "stop",
                              "message": { "role": "assistant", "content": content } }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    serde_json::from_str(&serde_json::to_string(v).unwrap()).unwrap()
}

#[tokio::test]
async fn the_probe_answers_what_the_primitives_answer() {
    let base = deterministic_daemon().await;
    let (model, context) = ("critic-under-test", 8192);
    let inference = provider(&base, model, context);
    let chunks = vec![
        "Paris is the capital and largest city of France.".to_string(),
        "Lyon is a city in France.".to_string(),
    ];
    let (q, a) = ("What is the capital of France?", "It is Paris.");

    // ── judge ──────────────────────────────────────────────────────
    let ops = vec![
        JudgeOp::ForcedChoice {
            register: "bench_abstain".into(),
            prompt: "Did the reply decline?".into(),
        },
        JudgeOp::ChunkSupport {
            register: "bench_chunk_support".into(),
            passage: chunks[0].clone(),
            claim: "Paris is the capital of France.".into(),
        },
        JudgeOp::CentralClaim {
            question: q.into(),
            answer: a.into(),
        },
        JudgeOp::ClaimList {
            question: q.into(),
            answer: a.into(),
            max_claims: 4,
        },
        JudgeOp::ClaimChunkSupport {
            passage: chunks[1].clone(),
            claim: "Lyon is the capital of France.".into(),
        },
    ];
    let ProbeEvidence::Judge(via_probe) = judge(
        &base,
        &JudgeProbe {
            model: Some(model.into()),
            context,
            ops,
        },
    )
    .await
    else {
        panic!("a judge probe answers with judge evidence");
    };
    let via_probe = round_trip(&*via_probe);
    let fc = |prompt: String, register: &'static str| {
        let inference = inference.clone();
        async move {
            forced_choice_ab(
                inference.as_ref(),
                CHUNK_JUDGE_SYSTEM,
                &prompt,
                None,
                JudgeRouting::PinnedSlot(model),
                JudgeCall::Harness(register),
            )
            .await
        }
    };
    let central = inference
        .complete(&CompletionRequest {
            prompt: claim_extraction_prompt(q, a, false),
            system_message: Some(CLAIM_EXTRACTION_SYSTEM.into()),
            preferred_speed: Speed::Slow,
            max_tokens: Some(64),
            temperature: Some(0.0),
            think_budget: Some(0),
            enable_thinking: Some(false),
            model_id: Some(model.to_string()),
            ..Default::default()
        })
        .await
        .map(|r| r.text.trim().to_string())
        .map_err(|e| e.to_string());
    let direct = vec![
        JudgeAnswer::ForcedChoice {
            a_b: fc("Did the reply decline?".into(), "bench_abstain").await,
        },
        JudgeAnswer::ChunkSupport {
            a_b: fc(
                chunk_judge_prompt(&chunks[0], "Paris is the capital of France."),
                "bench_chunk_support",
            )
            .await,
        },
        JudgeAnswer::CentralClaim { text: central },
        JudgeAnswer::ClaimList {
            claims: extract_claim_list(&inference, q, a, 4, ShardingPrivacy::LocalOnly).await,
        },
        JudgeAnswer::ClaimChunkSupport {
            support: claim_chunk_support(
                &inference,
                &chunks[1],
                "Lyon is the capital of France.",
                ShardingPrivacy::LocalOnly,
            )
            .await,
        },
    ];
    assert_eq!(via_probe.model, model);
    assert_eq!(via_probe.rows, direct);
    assert!(
        matches!(
            via_probe.rows[0],
            JudgeAnswer::ForcedChoice { a_b: Some(_) }
        ),
        "the fixture reaches a verdict, so equality is not two failures agreeing"
    );

    // ── assess ─────────────────────────────────────────────────────
    let ops = vec![
        AssessOp::AssertedValue {
            question: q.into(),
            answer: a.into(),
            chunks: chunks.clone(),
        },
        AssessOp::PureDecline {
            answer: "I don't know.".into(),
        },
        AssessOp::PureDecline { answer: a.into() },
        AssessOp::ValuePresent {
            value: "Paris".into(),
            chunks: chunks.clone(),
        },
        AssessOp::ValuePresent {
            value: "Marseille".into(),
            chunks: chunks.clone(),
        },
    ];
    let ProbeEvidence::Assess(via_probe) = assess(
        &base,
        &AssessProbe {
            model: Some(model.into()),
            context,
            ops,
        },
    )
    .await
    else {
        panic!("an assess probe answers with assess evidence");
    };
    let via_probe = round_trip(&*via_probe);
    let asserted = match assess_asserted_value(
        inference.as_ref(),
        q,
        a,
        &chunks,
        ShardingPrivacy::LocalOnly,
    )
    .await
    {
        AssertedValue::Grounded(v) => AssertedValueVerdict::Grounded(v),
        AssertedValue::Ungrounded(v) => AssertedValueVerdict::Ungrounded(v),
        AssertedValue::NoValue => AssertedValueVerdict::NoValue,
    };
    let direct = vec![
        AssessAnswer::AssertedValue { verdict: asserted },
        AssessAnswer::PureDecline {
            pure: released_pure_decline("I don't know."),
        },
        AssessAnswer::PureDecline {
            pure: released_pure_decline(a),
        },
        AssessAnswer::ValuePresent {
            present: value_present_in_chunks("Paris", &chunks),
        },
        AssessAnswer::ValuePresent {
            present: value_present_in_chunks("Marseille", &chunks),
        },
    ];
    assert_eq!(via_probe.rows, direct);
    assert_eq!(via_probe.gate_threshold, grounding_gate_threshold());
    assert_ne!(
        direct[3], direct[4],
        "the fixture separates present from absent, so the rows are not all one answer"
    );
}

/// No model named: the calls pin svrn's Critic profile's model, and the
/// evidence names it, so a bench that asked for the default reports what ran.
#[tokio::test]
async fn no_model_is_the_critic_profiles() {
    let base = deterministic_daemon().await;
    let ProbeEvidence::Assess(ev) = assess(
        &base,
        &AssessProbe {
            model: None,
            context: 8192,
            ops: Vec::new(),
        },
    )
    .await
    else {
        panic!("assess evidence");
    };
    assert_eq!(
        ev.model,
        default_profile_for(Role::Critic)
            .preferred_tier
            .model_stem()
    );
}
