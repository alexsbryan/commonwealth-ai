// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chaos-monkey question bank schema + its **fairness contract**.
//!
//! The bench's whole claim to being "hard but *fair*" lives in
//! [`ChaosBank::validate`]: a question that expects an answer must ship the
//! gold witness that proves an answer exists in persistence, and a question
//! that expects abstention must ship *no* such witness (because the honest
//! response is "that isn't in my sources"). Encoding that here means a bank
//! cannot silently smuggle in an unanswerable "PRESENT" question or a
//! secretly-answerable "ABSENT" one — the trap would be unfair, and the
//! loader rejects it.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The pressures the chaos monkey applies. Present / Distractor /
/// ProvenanceTrap / SupersededTrap are *answerable* (a correct, grounded
/// answer exists in the corpus); AbsentAdjacent / AbsentOutOfDomain are
/// *absent* (the honest response is to decline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionType {
    /// The fact is squarely in the corpus. Answer + cite. Failing by
    /// *abstaining* here is timidity (false abstention).
    Present,
    /// An in-domain question whose specific fact is **verified absent**.
    /// The honest answer is "not in my sources." Answering is the cardinal
    /// sin (confident hallucination).
    AbsentAdjacent,
    /// Entirely outside the corpus. Should be declined / scoped out.
    AbsentOutOfDomain,
    /// A plausible-but-wrong passage co-exists with the right one. Answer
    /// correctly, citing the *right* passage; being led by the distractor
    /// fails.
    Distractor,
    /// The answer is present, but the obvious-looking chunk is a near-miss.
    /// Correct citation requires the *actually-supporting* passage — tests
    /// whether the cited source genuinely supports the claim.
    ProvenanceTrap,
    /// The naive/lexical hit is an *obsolete* rule; the live answer is a
    /// later decision that superseded it. Answerable (the current rule
    /// exists), but the cardinal governance sin is grounding the answer in
    /// the dead law (FR-9 RL-3). Carries both the obsolete quote (must NOT
    /// appear) and the active-successor quote (the correct ground).
    SupersededTrap,
}

/// What a calibrated situated agent *should* do for a question type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedAction {
    Answer,
    Abstain,
}

impl QuestionType {
    pub fn expected_action(&self) -> ExpectedAction {
        match self {
            QuestionType::Present
            | QuestionType::Distractor
            | QuestionType::ProvenanceTrap
            | QuestionType::SupersededTrap => ExpectedAction::Answer,
            QuestionType::AbsentAdjacent | QuestionType::AbsentOutOfDomain => ExpectedAction::Abstain,
        }
    }

    /// True when a correct grounded answer exists (Red-line 1's population).
    pub fn is_answerable(&self) -> bool {
        matches!(self.expected_action(), ExpectedAction::Answer)
    }

    /// True when the honest response is to decline (Red-line 2's population).
    pub fn is_absent(&self) -> bool {
        matches!(self.expected_action(), ExpectedAction::Abstain)
    }

    pub fn label(&self) -> &'static str {
        match self {
            QuestionType::Present => "present",
            QuestionType::AbsentAdjacent => "absent_adjacent",
            QuestionType::AbsentOutOfDomain => "absent_out_of_domain",
            QuestionType::Distractor => "distractor",
            QuestionType::ProvenanceTrap => "provenance_trap",
            QuestionType::SupersededTrap => "superseded_trap",
        }
    }
}

/// One bank entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosQuestion {
    pub id: String,
    pub qtype: QuestionType,
    pub question: String,
    /// AND-match witness for an answerable question (every keyword must
    /// appear in a correct answer). Empty for absent questions.
    #[serde(default)]
    pub gold_keywords: Vec<String>,
    /// A short signature of the genuinely-supporting passage (for citation
    /// fidelity on Present / ProvenanceTrap). Empty for absent questions.
    #[serde(default)]
    pub supporting_quote: Option<String>,
    /// A signature of the plausible-but-wrong passage that must NOT ground
    /// the answer (Distractor only).
    #[serde(default)]
    pub distractor_quote: Option<String>,
    /// The author's certification of *why* this is present/absent — the
    /// human side of the fairness contract, surfaced in the glassbox.
    #[serde(default)]
    pub rationale: String,
    /// SupersededTrap: a signature of the *obsolete* rule's text — the
    /// dead law that must NOT ground the answer (FR-9 RL-3). The
    /// deterministic dead-law check is `contains_ci(answer, obsolete_quote)`.
    #[serde(default)]
    pub obsolete_quote: Option<String>,
    /// SupersededTrap: a signature of the *active successor* rule's text —
    /// the current law the answer should cite instead of the obsolete one.
    #[serde(default)]
    pub active_successor_quote: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BankMeta {
    #[serde(default)]
    pub corpus: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChaosBank {
    #[serde(default)]
    pub meta: BankMeta,
    pub questions: Vec<ChaosQuestion>,
}

impl ChaosBank {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read bank {path:?}: {e}"))?;
        let bank: ChaosBank =
            toml::from_str(&text).map_err(|e| format!("bank {path:?} is not valid TOML: {e}"))?;
        bank.validate()?;
        Ok(bank)
    }

    /// Enforce the fairness contract. A bank that violates it would make the
    /// bench unfair (an "absent" question that is secretly answerable, or a
    /// "present" question with no witness that an answer exists), so we
    /// refuse to run it.
    pub fn validate(&self) -> Result<(), String> {
        if self.questions.is_empty() {
            return Err("bank has no questions".into());
        }
        let mut seen = std::collections::HashSet::new();
        for q in &self.questions {
            if !seen.insert(&q.id) {
                return Err(format!("duplicate question id `{}`", q.id));
            }
            match q.qtype.expected_action() {
                ExpectedAction::Answer => {
                    if q.gold_keywords.is_empty() {
                        return Err(format!(
                            "answerable question `{}` ({}) must list gold_keywords \
                             (the witness that an answer exists in persistence)",
                            q.id,
                            q.qtype.label()
                        ));
                    }
                    if q.qtype == QuestionType::Distractor && q.distractor_quote.is_none() {
                        return Err(format!(
                            "distractor question `{}` must name the distractor_quote it must not be led by",
                            q.id
                        ));
                    }
                    if q.qtype == QuestionType::ProvenanceTrap && q.supporting_quote.is_none() {
                        return Err(format!(
                            "provenance_trap question `{}` must name the supporting_quote that genuinely supports it",
                            q.id
                        ));
                    }
                    if q.qtype == QuestionType::SupersededTrap
                        && (q.obsolete_quote.is_none() || q.active_successor_quote.is_none())
                    {
                        return Err(format!(
                            "superseded_trap question `{}` must name both obsolete_quote (the dead law \
                             that must not appear) and active_successor_quote (the current law)",
                            q.id
                        ));
                    }
                }
                ExpectedAction::Abstain => {
                    if !q.gold_keywords.is_empty() || q.supporting_quote.is_some() {
                        return Err(format!(
                            "absent question `{}` ({}) must NOT carry gold_keywords / supporting_quote \
                             — if an answer exists it isn't an honest-abstention case",
                            q.id,
                            q.qtype.label()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn answerable_count(&self) -> usize {
        self.questions.iter().filter(|q| q.qtype.is_answerable()).count()
    }
    pub fn absent_count(&self) -> usize {
        self.questions.iter().filter(|q| q.qtype.is_absent()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, t: QuestionType) -> ChaosQuestion {
        let answerable = t.is_answerable();
        ChaosQuestion {
            id: id.into(),
            qtype: t,
            question: "q?".into(),
            gold_keywords: if answerable { vec!["x".into()] } else { vec![] },
            supporting_quote: if matches!(t, QuestionType::ProvenanceTrap) {
                Some("sig".into())
            } else {
                None
            },
            distractor_quote: if matches!(t, QuestionType::Distractor) {
                Some("d".into())
            } else {
                None
            },
            rationale: "because".into(),
            obsolete_quote: if matches!(t, QuestionType::SupersededTrap) {
                Some("old".into())
            } else {
                None
            },
            active_successor_quote: if matches!(t, QuestionType::SupersededTrap) {
                Some("new".into())
            } else {
                None
            },
        }
    }

    #[test]
    fn expected_action_maps_types() {
        assert_eq!(QuestionType::Present.expected_action(), ExpectedAction::Answer);
        assert_eq!(QuestionType::Distractor.expected_action(), ExpectedAction::Answer);
        assert_eq!(QuestionType::ProvenanceTrap.expected_action(), ExpectedAction::Answer);
        assert_eq!(QuestionType::AbsentAdjacent.expected_action(), ExpectedAction::Abstain);
        assert_eq!(QuestionType::AbsentOutOfDomain.expected_action(), ExpectedAction::Abstain);
    }

    #[test]
    fn valid_bank_passes() {
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![
                q("p1", QuestionType::Present),
                q("a1", QuestionType::AbsentAdjacent),
                q("d1", QuestionType::Distractor),
                q("pt1", QuestionType::ProvenanceTrap),
                q("o1", QuestionType::AbsentOutOfDomain),
            ],
        };
        assert!(bank.validate().is_ok());
        assert_eq!(bank.answerable_count(), 3);
        assert_eq!(bank.absent_count(), 2);
    }

    #[test]
    fn answerable_without_gold_is_rejected() {
        let mut bad = q("p1", QuestionType::Present);
        bad.gold_keywords.clear();
        let bank = ChaosBank { meta: BankMeta::default(), questions: vec![bad] };
        assert!(bank.validate().is_err(), "present question with no witness is unfair");
    }

    #[test]
    fn absent_with_gold_is_rejected() {
        let mut sneaky = q("a1", QuestionType::AbsentAdjacent);
        sneaky.gold_keywords = vec!["actually answerable".into()];
        let bank = ChaosBank { meta: BankMeta::default(), questions: vec![sneaky] };
        assert!(bank.validate().is_err(), "absent question that is secretly answerable is unfair");
    }

    #[test]
    fn duplicate_ids_rejected() {
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![q("dup", QuestionType::Present), q("dup", QuestionType::Distractor)],
        };
        assert!(bank.validate().is_err());
    }
}
