// SPDX-License-Identifier: AGPL-3.0-or-later
//! `verify` (v0.6 §2.6, `cmnwlth/docs/oicp-v0.6.md`): a unit of text judged,
//! claim by claim, against the stored texts a caller may read, statelessly.
//!
//! A host splits the unit into claims, classes each, and gives each a verdict
//! with the passages it judged. A verdict says which rung decided it and the
//! rung's operating point, `uncalibrated` until a bench has measured that
//! cell. Ranges into the unit are half-open code-point counts, each with the
//! exact string it names, as everywhere in the evidence extension
//! ([`crate::evidence`]).

use serde::{Deserialize, Serialize};

use crate::evidence::{CorpusTexts, Span, Unavailable};

/// The named refusals and reasons of the verify route. Each travels as
/// `{"error": "<reason>"}` on a refusal, or as a `could_not_judge` verdict's
/// or a passage's `reason`. A client matches these strings exactly and treats
/// any other as opaque.
pub mod reasons {
    /// The unit is empty or whitespace.
    pub const EMPTY_UNIT: &str = "empty unit";
    /// The requested tier is one this host does not serve; the manifest's
    /// `verify_tiers` says which it does.
    pub const TIER_NOT_OFFERED: &str = "tier not offered";
    /// The tier needs a model and this listener holds none to judge with.
    pub const NO_CHECKER: &str = "no checker";
    /// The checker was asked and gave no verdict on any passage.
    pub const CHECKER_DID_NOT_ANSWER: &str = "checker did not answer";
    /// No requested corpus could be read, so nothing was judged against.
    pub const NO_CORPUS_READ: &str = "no corpus read";
    /// A passage's span absences, shared with search's hits (v0.6 §2.8): a
    /// passage whose text or record cannot be read gives v0.5's reason
    /// instead (`texts not stored`, `text not stored`, `document unreadable`).
    pub use crate::evidence::reasons::{NOT_AN_EXACT_SLICE, PASSAGE_NAMES_NO_TEXT};
    /// A correction's `episode_id` is not one this host issues (a UUID).
    pub const NOT_AN_EPISODE: &str = "not an episode id";
    /// This listener keeps no journal, so a correction has nowhere to go.
    pub const NO_JOURNAL: &str = "no journal";
}

/// A verdict's operating point before any bench has calibrated its cell.
pub const UNCALIBRATED: &str = "uncalibrated";

/// How hard a host looks (v0.6 §2.6). A host advertises the tiers it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyTier {
    /// No model: each claim's specifics checked for presence in the passages.
    Fast,
    /// A model judges each claim against the passages.
    Checker,
    /// A stronger judge, quoting before it decides. No host serves it yet.
    Judge,
}

impl VerifyTier {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            VerifyTier::Fast => "fast",
            VerifyTier::Checker => "checker",
            VerifyTier::Judge => "judge",
        }
    }
}

/// `POST {verify_endpoint}` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRequest {
    /// The text to judge: a sentence or a few.
    pub unit: String,
    /// Text around the unit the host may read to understand it (the
    /// paragraph it sits in). Never judged itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Corpora to judge against. Empty means every corpus the caller may
    /// read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora: Vec<String>,
    /// The tier asked for.
    pub tier: VerifyTier,
    /// Code points of `prefix`/`suffix` on each passage's span;
    /// [`crate::evidence::DEFAULT_CONTEXT`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_context: Option<u32>,
}

impl VerifyRequest {
    /// `span_context`, or its default.
    pub fn effective_span_context(&self) -> u32 {
        self.span_context
            .unwrap_or(crate::evidence::DEFAULT_CONTEXT)
    }
}

/// `POST {verify_endpoint}` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyResponse {
    /// Every claim of the unit, in unit order.
    pub claims: Vec<VerifiedClaim>,
    /// The verifier's identity. It changes whenever the same request could
    /// be judged differently (a rung, a threshold, the checker's model), so a
    /// client keys cached verdicts on it.
    pub verifier: String,
    /// Each corpus that was read, with the digest of its texts.
    pub corpora: Vec<CorpusTexts>,
    /// Each requested corpus that could not be read, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpora_unavailable: Vec<Unavailable>,
}

/// One claim of the unit and what the host found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedClaim {
    /// First code point of the claim in the unit.
    pub start: u64,
    /// One past the claim's last code point in the unit.
    pub end: u64,
    /// The unit's `[start, end)`, exactly.
    pub exact: String,
    /// Whether the claim is one this host checks.
    pub class: Checkability,
    /// The verdict.
    pub verdict: VerifyVerdict,
    /// The passages judged, each with its own verdict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<JudgedPassage>,
    /// The claim's specifics that no passage carries, as written in the
    /// claim. Set by the fast tier; empty otherwise. Under v0.6 §2.15 a
    /// hint the checker reads, never a verdict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specifics_absent: Vec<String>,
    /// v0.6 §2.15 (`evidence:verify_sentences`): the specifics the claim
    /// asserts, each by kind and as a range of the unit inside the claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specifics: Vec<ClaimSpecific>,
    /// The rung that decided, the host's name for it.
    pub rung: String,
    /// The deciding rung's score, in `[0, 1]`, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// The deciding cell's operating point, or [`UNCALIBRATED`].
    pub operating_point: String,
    /// The decision's id in the host's journal, to correct it by (v0.6
    /// §2.10). Absent when the host keeps no journal of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
}

/// Whether a claim is one the host checks. A claim it does not check is
/// `abstained`, never `supported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkability {
    /// The claim asserts something the passages can bear on.
    Checkable,
    /// Opinion, argument, or a sentence with nothing to look for.
    NotCheckable,
}

/// The closed set of verdicts (v0.6 §2.6), on a claim or on one passage.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerifyVerdict {
    /// A passage supports the claim.
    Supported,
    /// A passage contradicts the claim.
    Contradicted,
    /// Nothing judged supports the claim.
    Unsupported,
    /// The host could not judge, and says why: one of [`reasons`] where one
    /// applies.
    CouldNotJudge {
        /// Why.
        reason: String,
    },
    /// The host does not judge this claim at this tier.
    Abstained,
}

/// One passage a verdict rests on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgedPassage {
    /// The corpus the passage came from.
    pub corpus_id: String,
    /// The passage as a range of its stored text. Absent when the passage is
    /// not an exact slice of it, with the reason in `span_absent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// Why `span` is absent: one of [`reasons`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_absent: Option<String>,
    /// This passage's verdict on the claim.
    pub verdict: VerifyVerdict,
    /// The rung's score for this passage, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// v0.6 §2.15: on a `contradicted` passage, and only there, each
    /// specific of the claim the passage states otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differing: Vec<DifferingSpecific>,
}

/// v0.6 §2.15: what a specific is. A closed set; a kind outside it is a
/// decode error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSpecificKind {
    /// A number with or without a unit, a percentage, a currency amount.
    Figure,
    /// A year or a written date.
    Date,
    /// A number with a counted noun.
    Quantity,
    /// A run of name-shaped words: a person, a place, a work.
    Name,
}

/// v0.6 §2.15: one specific, as a range of the text it is read from (the
/// unit for a claim's, the stored text for a passage's), in code points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSpecific {
    /// What it is.
    pub kind: ClaimSpecificKind,
    /// First code point.
    pub start: u64,
    /// One past the last code point.
    pub end: u64,
    /// The text's `[start, end)`, exactly.
    pub exact: String,
}

/// v0.6 §2.15: a specific of the claim and the passage's of the same kind
/// that states it otherwise: what makes the passage `contradicted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DifferingSpecific {
    /// The claim's, in the unit.
    pub claim: ClaimSpecific,
    /// The passage's, in its stored text.
    pub source: ClaimSpecific,
}

/// What a client says a decision should have been. Closed, and wider than
/// the incumbent judges' verdicts by `contradicted`: a correction may say the
/// sources say the opposite, which no incumbent judge can yet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CorrectedVerdict {
    /// The sources support the claim.
    Supported,
    /// The sources say otherwise.
    Contradicted,
    /// Nothing in the sources bears on it.
    Unsupported,
}

/// A range of a stored text a correction points at as its witness: by
/// name and code points, never by its words.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WitnessRef {
    /// The corpus the witness was read from.
    pub corpus: String,
    /// The stored text's name (64 lowercase hex).
    pub text_sha256: String,
    /// First code point.
    pub start: u64,
    /// One past the last code point.
    pub end: u64,
}

/// What the host found when it read the witness back, mechanically.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WitnessCheck {
    /// The text is held and the range lies inside it.
    Verified,
    /// No corpus the caller may read holds a text of that name.
    NotHeld,
    /// The text is held; the range is not inside it.
    OutOfRange,
    /// The correction named no witness.
    NotGiven,
}

/// `POST {verify_correction_endpoint}` (v0.6 §2.10): one decision a client
/// says was wrong, by the `episode_id` its claim came back with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCorrection {
    /// The claim's `episode_id`, from a [`VerifiedClaim`].
    pub episode_id: String,
    /// What it should have been.
    pub expected: CorrectedVerdict,
    /// Where in the sources the client says the answer stands; the host
    /// reads it back before it records anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<WitnessRef>,
}

/// The reply to a correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCorrectionReply {
    /// Whether the host kept it: `false` when its journal is switched off
    /// or full for the day, a posture, never an error.
    pub recorded: bool,
    /// What reading the witness back found.
    pub witness: WitnessCheck,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_and_tiers_have_their_wire_spellings() {
        let v = serde_json::to_value(VerifyVerdict::CouldNotJudge {
            reason: reasons::NO_CHECKER.into(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "could_not_judge", "reason": "no checker"})
        );
        assert_eq!(
            serde_json::to_value(VerifyVerdict::Unsupported).unwrap(),
            serde_json::json!({"kind": "unsupported"})
        );
        for t in [VerifyTier::Fast, VerifyTier::Checker, VerifyTier::Judge] {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
        }
        let req: VerifyRequest =
            serde_json::from_value(serde_json::json!({"unit": "x", "tier": "fast"})).unwrap();
        assert!(req.corpora.is_empty() && req.context.is_none());
        assert_eq!(
            req.effective_span_context(),
            crate::evidence::DEFAULT_CONTEXT
        );
    }
}
