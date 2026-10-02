// SPDX-License-Identifier: AGPL-3.0-or-later
//! The assess and judge probes ([`super::ProbeMode::Assess`],
//! [`super::ProbeMode::Judge`]; phase-b-63, -64): svrn's own grounding
//! primitives, run by svrn over text a bench hands it. A bench scores with
//! the gate's verdicts and registers but holds none of them, so svrn's gate
//! stays the one decider and no threshold or prompt is copied.
//!
//! Each request is a list of ops answered in order, every model call pinned
//! to one daemon model (`None` = svrn's Critic profile's) under the
//! `LocalOnly` posture a bench always ran.

use serde::{Deserialize, Serialize};

/// An assess probe: the gate's verdicts over bench-supplied text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessProbe {
    /// The daemon model the value assessment runs on; `None` = svrn's
    /// Critic profile's.
    pub model: Option<String>,
    /// The provider's context window, as the bench's own provider had it.
    pub context: u32,
    /// The questions to answer, in order.
    pub ops: Vec<AssessOp>,
}

/// One assess question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AssessOp {
    /// `assess_asserted_value`: is the value the answer offers present in
    /// the chunks.
    AssertedValue {
        /// The question the answer replies to.
        question: String,
        /// The answer text.
        answer: String,
        /// The evidence it is checked against.
        chunks: Vec<String>,
    },
    /// `released_pure_decline`: is the text nothing but a decline.
    PureDecline {
        /// The answer text.
        answer: String,
    },
    /// `value_present_in_chunks`: does the value appear in the chunks.
    ValuePresent {
        /// The value, checked whole.
        value: String,
        /// The evidence.
        chunks: Vec<String>,
    },
}

/// What an assess probe observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessEvidence {
    /// The model the value assessments ran on (the request's, or the Critic
    /// profile's).
    pub model: String,
    /// `grounding_gate_threshold()` as this svrn resolves it.
    pub gate_threshold: f64,
    /// One answer per op, in order.
    pub rows: Vec<AssessAnswer>,
}

/// The answer to one [`AssessOp`], same variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AssessAnswer {
    /// [`AssessOp::AssertedValue`].
    AssertedValue {
        /// The gate's verdict.
        verdict: AssertedValueVerdict,
    },
    /// [`AssessOp::PureDecline`].
    PureDecline {
        /// True iff the text is a pure decline.
        pure: bool,
    },
    /// [`AssessOp::ValuePresent`].
    ValuePresent {
        /// True iff the value is present.
        present: bool,
    },
}

/// `AssertedValue`, on the wire: the groundedness of the one value an
/// answer asserts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertedValueVerdict {
    /// The value is present in the evidence.
    Grounded(String),
    /// The value is absent from the evidence.
    Ungrounded(String),
    /// Nothing was decided: no checkable value, or no verdict. The
    /// could-not-judge bucket, not a pass.
    NoValue,
}

/// A judge probe: the gate's model registers over bench-supplied text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeProbe {
    /// The daemon model every call is pinned to; `None` = svrn's Critic
    /// profile's.
    pub model: Option<String>,
    /// The provider's context window, as the bench's own provider had it.
    pub context: u32,
    /// The calls to make, in order.
    pub ops: Vec<JudgeOp>,
}

/// One judge call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JudgeOp {
    /// `forced_choice_ab` in the gate's register (`CHUNK_JUDGE_SYSTEM`,
    /// pinned slot) over a prompt the bench rendered.
    ForcedChoice {
        /// The bench scorer asking, carried into the `grounding_gate` trace.
        register: String,
        /// The prompt.
        prompt: String,
    },
    /// The chunk-support register: `chunk_judge_prompt(passage, claim)`,
    /// then the forced-choice pass.
    ChunkSupport {
        /// The bench scorer asking.
        register: String,
        /// The passage.
        passage: String,
        /// The claim.
        claim: String,
    },
    /// The gate's central-claim extraction, unanchored:
    /// `claim_extraction_prompt` under `CLAIM_EXTRACTION_SYSTEM`, 64 tokens,
    /// temperature 0, no thinking.
    CentralClaim {
        /// The question.
        question: String,
        /// The answer.
        answer: String,
    },
    /// `extract_claim_list`.
    ClaimList {
        /// The question.
        question: String,
        /// The answer.
        answer: String,
        /// The cap on claims.
        max_claims: usize,
    },
    /// `claim_chunk_support`.
    ClaimChunkSupport {
        /// The passage.
        passage: String,
        /// The claim.
        claim: String,
    },
}

/// What a judge probe observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeEvidence {
    /// The model the calls ran on (the request's, or the Critic profile's).
    pub model: String,
    /// One answer per op, in order.
    pub rows: Vec<JudgeAnswer>,
}

/// The answer to one [`JudgeOp`], same variant. `None` / `Err` is the
/// primitive's own failure, carried, never defaulted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JudgeAnswer {
    /// [`JudgeOp::ForcedChoice`]: `(p_A, p_B)`.
    ForcedChoice {
        /// The distribution, or `None` when the pass failed.
        a_b: Option<(f64, f64)>,
    },
    /// [`JudgeOp::ChunkSupport`]: `(p_A, p_B)`.
    ChunkSupport {
        /// The distribution, or `None` when the pass failed.
        a_b: Option<(f64, f64)>,
    },
    /// [`JudgeOp::CentralClaim`]: the completion's text, trimmed, or the
    /// provider's error.
    CentralClaim {
        /// `Ok(text)` or `Err(message)`.
        text: Result<String, String>,
    },
    /// [`JudgeOp::ClaimList`].
    ClaimList {
        /// The claims, or `None` on an inference failure.
        claims: Option<Vec<String>>,
    },
    /// [`JudgeOp::ClaimChunkSupport`].
    ClaimChunkSupport {
        /// Support in [0,1], or `None` on a judge failure.
        support: Option<f64>,
    },
}
