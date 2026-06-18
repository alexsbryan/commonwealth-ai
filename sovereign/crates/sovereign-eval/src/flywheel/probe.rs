// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shared probe schema — the "generate" output every Fidelity-Flywheel
//! signal source (I1 corpus, I2 adversarial, I3 cross-model, I4 delta, I5
//! human) emits and the verifier consumes.
//!
//! A [`Probe`] reuses the chaos-monkey [`QuestionType`] as its register/shape
//! vocabulary — the flywheel is scoped to the grounding / abstention registers
//! (the moat), so those five pressures ARE the register set — and pairs it with
//! an [`Oracle`] carrying the grounding ground-truth. Reusing `QuestionType`
//! (rather than a parallel "register" enum) lets a [`crate::flywheel::verify::Verdict`]
//! lower into a chaos [`crate::chaos_monkey::ResultRow`] exactly, so
//! `chaos_monkey::score` stays the single scorer of record.
//!
//! The flywheel deliberately does NOT model the reasoning / open-world /
//! creative registers (R-risk): corpus self-supervision can't supply ground
//! truth for them, so they're out of scope.

use serde::{Deserialize, Serialize};

use crate::chaos_monkey::{ChaosQuestion, ExpectedAction, QuestionType};

/// Which signal source emitted a probe — provenance that lets the verifier,
/// capture, and gate stay generator-agnostic while still attributing findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeSource {
    /// I1 — autonomous corpus self-supervision (claim mining + held-out slice).
    I1Corpus,
    /// I2 — adversarial generation near the corpus boundary.
    I2Adversarial,
    /// I3 — cross-model disagreement.
    I3Disagreement,
    /// I4 — delta natural-experiment.
    I4Delta,
    /// I5 — human-authored anchor (the curated bank is this source's degenerate case).
    I5Human,
}

/// In-domain-unknowable vs fully out-of-corpus, for absent probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbsentKind {
    /// In-domain but verified absent → the honest move is to abstain.
    Adjacent,
    /// Entirely outside the corpus → the honest move is a caveated general answer.
    OutOfDomain,
}

/// The grounding ground-truth, resolving the requirements doc's
/// `grounding_oracle | witness` alternation.
///
/// (`Structural` — the probability oracle used by mechanism-fidelity — is
/// intentionally NOT a variant: it has no [`QuestionType`] and is scored by a
/// different metamorphic scorer. It joins when the I3 cross-model generator
/// reuses the attribution miner.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Oracle {
    /// A correct grounded answer exists. `gold_keywords` AND-match it;
    /// `supporting_quote` signs the genuinely-supporting passage;
    /// `distractor_quote` signs a plausible-wrong passage that must NOT ground it.
    Witness {
        gold_keywords: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supporting_quote: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        distractor_quote: Option<String>,
    },
    /// No answer exists in the *indexed* corpus. `held_out_witness`, when
    /// present, is the answer mined from a withheld document slice — proof the
    /// fact is real (so abstention is verifiably correct) AND a leak detector
    /// (answering it = grounding a fact not in the indexed corpus → F-FALSE-GROUND).
    Absent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        held_out_witness: Option<Vec<String>>,
        kind: AbsentKind,
    },
}

/// The unified "generate" output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub id: String,
    pub query: String,
    /// The register / shape — reused chaos vocabulary. `qtype.expected_action()`
    /// gives the answer-vs-abstain register; `AbsentOutOfDomain` is the
    /// caveated-general register.
    pub qtype: QuestionType,
    pub oracle: Oracle,
    pub source: ProbeSource,
    #[serde(default)]
    pub note: String,
}

impl Probe {
    pub fn expected_action(&self) -> ExpectedAction {
        self.qtype.expected_action()
    }
}

/// Lift a curated chaos-monkey question into a `Probe`. Used by the I5/human
/// (curated-bank) generator and to reuse the existing absent bank while the
/// held-out slice is being built.
pub fn chaos_to_probe(q: &ChaosQuestion) -> Probe {
    let oracle = match q.qtype.expected_action() {
        ExpectedAction::Answer => Oracle::Witness {
            gold_keywords: q.gold_keywords.clone(),
            supporting_quote: q.supporting_quote.clone(),
            distractor_quote: q.distractor_quote.clone(),
        },
        ExpectedAction::Abstain => Oracle::Absent {
            held_out_witness: None,
            kind: match q.qtype {
                QuestionType::AbsentOutOfDomain => AbsentKind::OutOfDomain,
                _ => AbsentKind::Adjacent,
            },
        },
    };
    Probe {
        id: format!("chaos:{}", q.id),
        query: q.question.clone(),
        qtype: q.qtype,
        oracle,
        source: ProbeSource::I5Human,
        note: q.rationale.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifts_answerable_to_witness() {
        let q = ChaosQuestion {
            id: "p1".into(),
            qtype: QuestionType::Present,
            question: "who?".into(),
            gold_keywords: vec!["verloc".into()],
            supporting_quote: Some("the shop".into()),
            distractor_quote: None,
            rationale: "in ch.1".into(),
            obsolete_quote: None,
            active_successor_quote: None,
        };
        let p = chaos_to_probe(&q);
        assert_eq!(p.id, "chaos:p1");
        assert_eq!(p.qtype, QuestionType::Present);
        assert!(matches!(p.oracle, Oracle::Witness { .. }));
        assert_eq!(p.source, ProbeSource::I5Human);
    }

    #[test]
    fn lifts_absent_to_absent_oracle_dropping_any_witness() {
        let q = ChaosQuestion {
            id: "a1".into(),
            qtype: QuestionType::AbsentOutOfDomain,
            question: "capital of Australia?".into(),
            gold_keywords: vec![],
            supporting_quote: None,
            distractor_quote: None,
            rationale: "out of corpus".into(),
            obsolete_quote: None,
            active_successor_quote: None,
        };
        let p = chaos_to_probe(&q);
        assert!(matches!(
            p.oracle,
            Oracle::Absent { held_out_witness: None, kind: AbsentKind::OutOfDomain }
        ));
        assert_eq!(p.expected_action(), ExpectedAction::Abstain);
    }
}
