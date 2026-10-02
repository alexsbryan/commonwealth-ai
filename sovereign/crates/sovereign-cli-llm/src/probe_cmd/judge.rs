// SPDX-License-Identifier: AGPL-3.0-or-later
//! The probe's `assess` and `judge` modes (phase-b-63, -64): the grounding
//! gate's own verdicts and judge registers, run by svrn over text a bench
//! hands it. Bench scores with them and holds none of them, so the gate stays
//! the one decider and no threshold, prompt or parser is copied (ARCH
//! principles 8, 12). Every model call is pinned to one daemon model, and the
//! posture is `LocalOnly`, as the bench's own calls always were.

use std::sync::Arc;

use oicp_client::RemoteApiProvider;
use sovereign_contracts::probe::{
    AssertedValueVerdict, AssessAnswer, AssessEvidence, AssessOp, AssessProbe, JudgeAnswer,
    JudgeEvidence, JudgeOp, JudgeProbe, ProbeEvidence,
};
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::role::{default_profile_for, Role};
use sovereign_core::runtime::{
    assess_asserted_value, chunk_judge_prompt, claim_chunk_support, claim_extraction_prompt,
    extract_claim_list, forced_choice_ab, grounding_gate_threshold, released_pure_decline,
    value_present_in_chunks, AssertedValue, JudgeCall, JudgeRouting, CHUNK_JUDGE_SYSTEM,
    CLAIM_EXTRACTION_SYSTEM,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

/// The model a probe's calls pin: the request's, or svrn's Critic profile's
/// (the role the gate routes its judgement under).
fn model_or_critic(model: Option<&str>) -> String {
    model.map(str::to_string).unwrap_or_else(|| {
        default_profile_for(Role::Critic)
            .preferred_tier
            .model_stem()
            .to_string()
    })
}

/// The provider the bench built for the same calls: the daemon's `/v1`,
/// no bearer, pinned to `model`, with the bench's context window.
fn provider(daemon_base: &str, model: &str, context: u32) -> Arc<dyn InferenceProvider> {
    let v1 = format!("{}/v1", daemon_base.trim_end_matches('/'));
    Arc::new(RemoteApiProvider::new(&v1, None, model, context))
}

/// The mode `assess`.
pub(super) async fn assess(daemon_base: &str, spec: &AssessProbe) -> ProbeEvidence {
    let model = model_or_critic(spec.model.as_deref());
    let inference = provider(daemon_base, &model, spec.context);
    let rows = answer_assess(inference.as_ref(), &spec.ops).await;
    ProbeEvidence::Assess(Box::new(AssessEvidence {
        model,
        gate_threshold: grounding_gate_threshold(),
        rows,
    }))
}

/// The mode `judge`.
pub(super) async fn judge(daemon_base: &str, spec: &JudgeProbe) -> ProbeEvidence {
    let model = model_or_critic(spec.model.as_deref());
    let inference = provider(daemon_base, &model, spec.context);
    let rows = answer_judge(&inference, &model, &spec.ops).await;
    ProbeEvidence::Judge(Box::new(JudgeEvidence { model, rows }))
}

/// Each assess op answered by the gate's own primitive.
async fn answer_assess(inference: &dyn InferenceProvider, ops: &[AssessOp]) -> Vec<AssessAnswer> {
    let mut rows = Vec::with_capacity(ops.len());
    for op in ops {
        let row = match op {
            AssessOp::AssertedValue {
                question,
                answer,
                chunks,
            } => AssessAnswer::AssertedValue {
                verdict: match assess_asserted_value(
                    inference,
                    question,
                    answer,
                    chunks,
                    ShardingPrivacy::LocalOnly,
                )
                .await
                {
                    AssertedValue::Grounded(v) => AssertedValueVerdict::Grounded(v),
                    AssertedValue::Ungrounded(v) => AssertedValueVerdict::Ungrounded(v),
                    AssertedValue::NoValue => AssertedValueVerdict::NoValue,
                },
            },
            AssessOp::PureDecline { answer } => AssessAnswer::PureDecline {
                pure: released_pure_decline(answer),
            },
            AssessOp::ValuePresent { value, chunks } => AssessAnswer::ValuePresent {
                present: value_present_in_chunks(value, chunks),
            },
        };
        tracing::debug!(?row, "probe assess");
        rows.push(row);
    }
    rows
}

/// Each judge op answered by the gate's own register.
async fn answer_judge(
    inference: &Arc<dyn InferenceProvider>,
    model: &str,
    ops: &[JudgeOp],
) -> Vec<JudgeAnswer> {
    let mut rows = Vec::with_capacity(ops.len());
    for op in ops {
        let row = match op {
            JudgeOp::ForcedChoice { register, prompt } => JudgeAnswer::ForcedChoice {
                a_b: pinned_forced_choice(inference.as_ref(), model, register, prompt).await,
            },
            JudgeOp::ChunkSupport {
                register,
                passage,
                claim,
            } => JudgeAnswer::ChunkSupport {
                a_b: pinned_forced_choice(
                    inference.as_ref(),
                    model,
                    register,
                    &chunk_judge_prompt(passage, claim),
                )
                .await,
            },
            JudgeOp::CentralClaim { question, answer } => {
                let req = CompletionRequest {
                    prompt: claim_extraction_prompt(question, answer, false),
                    system_message: Some(CLAIM_EXTRACTION_SYSTEM.into()),
                    preferred_speed: Speed::Slow,
                    max_tokens: Some(64),
                    temperature: Some(0.0),
                    think_budget: Some(0),
                    enable_thinking: Some(false),
                    model_id: Some(model.to_string()),
                    ..Default::default()
                };
                JudgeAnswer::CentralClaim {
                    text: inference
                        .complete(&req)
                        .await
                        .map(|resp| resp.text.trim().to_string())
                        .map_err(|e| e.to_string()),
                }
            }
            JudgeOp::ClaimList {
                question,
                answer,
                max_claims,
            } => JudgeAnswer::ClaimList {
                claims: extract_claim_list(
                    inference,
                    question,
                    answer,
                    *max_claims,
                    ShardingPrivacy::LocalOnly,
                )
                .await,
            },
            JudgeOp::ClaimChunkSupport { passage, claim } => JudgeAnswer::ClaimChunkSupport {
                support: claim_chunk_support(inference, passage, claim, ShardingPrivacy::LocalOnly)
                    .await,
            },
        };
        tracing::debug!(?row, "probe judge");
        rows.push(row);
    }
    rows
}

/// The forced-choice pass in the gate's register, pinned to `model`, labelled
/// with the bench scorer's `register` in the `grounding_gate` trace.
///
/// `JudgeCall::Harness` carries a `&'static str`; the label arrives over the
/// wire, so it is leaked. A probe process answers one request and exits, so
/// the leak is bounded by the request's ops.
async fn pinned_forced_choice(
    inference: &dyn InferenceProvider,
    model: &str,
    register: &str,
    prompt: &str,
) -> Option<(f64, f64)> {
    let label: &'static str = Box::leak(register.to_string().into_boxed_str());
    forced_choice_ab(
        inference,
        CHUNK_JUDGE_SYSTEM,
        prompt,
        None,
        JudgeRouting::PinnedSlot(model),
        JudgeCall::Harness(label),
    )
    .await
}

#[cfg(test)]
#[path = "judge_tests.rs"]
mod tests;
