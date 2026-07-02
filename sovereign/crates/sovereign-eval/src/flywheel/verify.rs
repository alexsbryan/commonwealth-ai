// SPDX-License-Identifier: AGPL-3.0-or-later
//! The unified verifier: classify one answered / abstained probe into the
//! five-way failure taxonomy (or Pass), reusing the chaos-monkey witness checks
//! and `ResultRow` / `score` as the scorer of record.
//!
//! The verifier is PURE: it consumes an [`Observation`] (the agent's action,
//! answer text, retrieved chunks, and — for out-of-domain answers — the
//! provenance-caveat signal) that the live adapter produced via its
//! forced-choice judges, and emits a [`Verdict`]. No inference happens here.
//!
//! **Gating discipline.** A [`Verdict`] wraps the chaos [`ResultRow`], so the
//! pass/fail that gates promotion is `ResultRow::is_pass` — the scorer of
//! record, unchanged. The [`FailureClass`] is a *label* on a failure (glassbox
//! + capture); a debatable label edge can never move the gate. Verdicts are
//! tagged [`Determinism::Deterministic`] in v1 — every check is either a
//! deterministic witness match or an objective forced-choice the chaos bench
//! already gates on. A future free-text register / support judge would yield
//! [`Determinism::JudgeAdvisory`], which is recorded and captured but never
//! gates (cross-model independence, GR2, is not available on a single box).

use serde::{Deserialize, Serialize};

use crate::chaos_monkey::{AgentAction, ExpectedAction, QuestionType, ResultRow};
use crate::flywheel::det_checks::{contains_ci, gold_match};
use crate::flywheel::probe::{AbsentKind, Oracle, Probe};

/// The five-way failure taxonomy (G3 / §5 FR-FAIL). Exactly one per failed probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Asserted a corpus-fact not supported by any cited source.
    Confab,
    /// Abstained when the corpus contained a witness.
    FalsePossum,
    /// Answered as grounded when the corpus did not contain the answer.
    FalseGround,
    /// Handled in the wrong register.
    Misroute,
    /// Citation points to a source that does not support the claim.
    CiteDrift,
}

/// Whether a verdict may gate promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Determinism {
    /// Deterministic witness match, or an objective forced-choice the chaos
    /// bench already gates on (answer-vs-abstain, provenance-caveat).
    Deterministic,
    /// Depends on a free-text model judgement; advisory only, never gates.
    JudgeAdvisory,
}

/// What the live adapter observed for one probe. The judge-derived fields
/// (`action`, `caveat_present`) come from the adapter's forced-choice
/// classifiers; the rest is raw.
pub struct Observation<'a> {
    pub action: AgentAction,
    pub answer: &'a str,
    pub chunks: &'a [String],
    /// For out-of-domain answered probes: did the reply flag general-knowledge
    /// provenance? `None` when not applicable / not classified.
    pub caveat_present: Option<bool>,
}

/// One probe's outcome — wraps the chaos [`ResultRow`] (the scorer-of-record
/// JSONL contract) plus the taxonomy label and gating eligibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub probe_id: String,
    pub row: ResultRow,
    /// `None` == Pass; `Some(class)` == the failure label (glassbox + capture).
    pub failure: Option<FailureClass>,
    pub determinism: Determinism,
}

impl Verdict {
    pub fn is_pass(&self) -> bool {
        self.failure.is_none()
    }
}

/// The v1 verifier: deterministic witness checks + the objective forced-choice
/// signals the chaos bench already trusts.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeterministicVerifier;

impl DeterministicVerifier {
    pub fn verify(
        &self,
        probe: &Probe,
        obs: &Observation<'_>,
        model_id: &str,
        corpus: &str,
    ) -> Verdict {
        let answered = obs.action == AgentAction::Answered;

        // Deterministic witness checks against the answer + retrieved chunks.
        let (answer_correct, citation_faithful, used_distractor) = match &probe.oracle {
            Oracle::Witness {
                gold_keywords,
                supporting_quote,
                distractor_quote,
            } => {
                let correct = if probe.qtype.is_answerable() && answered {
                    Some(gold_match(obs.answer, gold_keywords))
                } else {
                    None
                };
                // ProvenanceTrap: did the genuinely-supporting passage make it
                // into retrieval? (Deterministic proxy, mirrors the chaos bench.)
                let cite = match (probe.qtype, supporting_quote, answered) {
                    (QuestionType::ProvenanceTrap, Some(sig), true) => {
                        Some(obs.chunks.iter().any(|c| contains_ci(c, sig)))
                    }
                    _ => None,
                };
                let distract = match (distractor_quote, answered) {
                    (Some(sig), true) => Some(contains_ci(obs.answer, sig)),
                    _ => None,
                };
                (correct, cite, distract)
            }
            Oracle::Absent { .. } => (None, None, None),
        };

        let row = ResultRow {
            id: probe.id.clone(),
            qtype: probe.qtype,
            expected_action: probe.qtype.expected_action(),
            agent_action: obs.action,
            answer_correct,
            citation_faithful,
            used_distractor,
            cited_obsolete: None,
            caveat_present: obs.caveat_present,
            violation_prob: None,
            model_id: model_id.to_string(),
            corpus: corpus.to_string(),
            answer_excerpt: obs.answer.chars().take(200).collect(),
            // The flywheel-verify path doesn't assess value-presence (no
            // evidence handle here); the chaos runner populates it. Likewise the
            // gate-action / partition signals are chaos-runner-only for now, so
            // the partition degrades to Unclassified for flywheel rows.
            asserted_value_grounded: None,
            asserted_value: None,
            gate_action: None,
            retrieval_present: None,
            draft_correct: None,
            partition: None,
        };

        let failure = if row.is_pass() {
            None
        } else {
            Some(classify_failure(probe, &row))
        };

        Verdict {
            probe_id: probe.id.clone(),
            row,
            failure,
            // v1: every check is deterministic or an objective forced-choice the
            // chaos bench already gates on. A future free-text register/support
            // judge would set JudgeAdvisory here.
            determinism: Determinism::Deterministic,
        }
    }
}

/// Map a failed `ResultRow` to its taxonomy class. The pass/fail decision is
/// chaos's `ResultRow::is_pass` (the scorer of record); this only *labels* a
/// failure, so a debatable edge here never changes the gate.
fn classify_failure(probe: &Probe, row: &ResultRow) -> FailureClass {
    match probe.qtype.expected_action() {
        ExpectedAction::Answer => {
            if row.agent_action == AgentAction::Abstained {
                // A grounded answer existed and the agent declined.
                FailureClass::FalsePossum
            } else if row.used_distractor == Some(true) || row.citation_faithful == Some(false) {
                // Led by the wrong passage, or the supporting passage wasn't retrieved.
                FailureClass::CiteDrift
            } else {
                // Answered an answerable question but the content didn't match
                // the grounded witness → asserted something unsupported.
                FailureClass::Confab
            }
        }
        ExpectedAction::Abstain => {
            if row.agent_action == AgentAction::Abstained {
                // Only AbsentOutOfDomain can fail by abstaining (Adjacent
                // abstain passes). Abstaining on an out-of-domain GK question is
                // the wrong register — the honest move is a caveated general
                // answer. Deterministic from the action alone.
                FailureClass::Misroute
            } else {
                match &probe.oracle {
                    // The answer provably exists in a withheld slice → answering
                    // it is grounding a fact absent from the indexed corpus.
                    Oracle::Absent {
                        held_out_witness: Some(_),
                        ..
                    } => FailureClass::FalseGround,
                    // In-domain unknowable, or out-of-domain answered without the
                    // caveat: a confident ungrounded assertion.
                    Oracle::Absent {
                        kind: AbsentKind::Adjacent | AbsentKind::OutOfDomain,
                        ..
                    } => FailureClass::Confab,
                    // Unreachable: an abstain-register probe always has an Absent
                    // oracle (enforced by validate_fairness), but stay total.
                    Oracle::Witness { .. } => FailureClass::Confab,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flywheel::probe::ProbeSource;

    fn obs<'a>(
        action: AgentAction,
        answer: &'a str,
        chunks: &'a [String],
        caveat: Option<bool>,
    ) -> Observation<'a> {
        Observation {
            action,
            answer,
            chunks,
            caveat_present: caveat,
        }
    }

    fn present(gold: &[&str]) -> Probe {
        Probe {
            id: "p".into(),
            query: "q".into(),
            qtype: QuestionType::Present,
            oracle: Oracle::Witness {
                gold_keywords: gold.iter().map(|s| s.to_string()).collect(),
                supporting_quote: None,
                distractor_quote: None,
            },
            source: ProbeSource::I1Corpus,
            note: String::new(),
        }
    }

    fn absent(kind: AbsentKind, held_out: Option<Vec<String>>) -> Probe {
        let qtype = match kind {
            AbsentKind::Adjacent => QuestionType::AbsentAdjacent,
            AbsentKind::OutOfDomain => QuestionType::AbsentOutOfDomain,
        };
        Probe {
            id: "a".into(),
            query: "q".into(),
            qtype,
            oracle: Oracle::Absent {
                held_out_witness: held_out,
                kind,
            },
            source: ProbeSource::I1Corpus,
            note: String::new(),
        }
    }

    const V: DeterministicVerifier = DeterministicVerifier;
    const NOCHUNKS: &[String] = &[];

    #[test]
    fn present_answered_correct_is_pass() {
        let p = present(&["verloc"]);
        let v = V.verify(
            &p,
            &obs(AgentAction::Answered, "the Verloc shop", NOCHUNKS, None),
            "m",
            "c",
        );
        assert!(v.is_pass());
    }

    #[test]
    fn present_answered_wrong_is_confab() {
        let p = present(&["verloc"]);
        let v = V.verify(
            &p,
            &obs(
                AgentAction::Answered,
                "no idea, perhaps Smith",
                NOCHUNKS,
                None,
            ),
            "m",
            "c",
        );
        assert_eq!(v.failure, Some(FailureClass::Confab));
    }

    #[test]
    fn present_abstained_is_false_possum() {
        let p = present(&["verloc"]);
        let v = V.verify(
            &p,
            &obs(AgentAction::Abstained, "I don't know", NOCHUNKS, None),
            "m",
            "c",
        );
        assert_eq!(v.failure, Some(FailureClass::FalsePossum));
    }

    #[test]
    fn absent_adjacent_answered_is_confab_abstained_is_pass() {
        let p = absent(AbsentKind::Adjacent, None);
        let answered = V.verify(
            &p,
            &obs(AgentAction::Answered, "his name is Heat", NOCHUNKS, None),
            "m",
            "c",
        );
        assert_eq!(answered.failure, Some(FailureClass::Confab));
        let abstained = V.verify(
            &p,
            &obs(AgentAction::Abstained, "not in my sources", NOCHUNKS, None),
            "m",
            "c",
        );
        assert!(abstained.is_pass());
    }

    #[test]
    fn out_of_domain_caveat_discriminates() {
        let p = absent(AbsentKind::OutOfDomain, None);
        let with = V.verify(
            &p,
            &obs(
                AgentAction::Answered,
                "Canberra (general knowledge)",
                NOCHUNKS,
                Some(true),
            ),
            "m",
            "c",
        );
        assert!(with.is_pass());
        let without = V.verify(
            &p,
            &obs(AgentAction::Answered, "Canberra", NOCHUNKS, Some(false)),
            "m",
            "c",
        );
        assert_eq!(without.failure, Some(FailureClass::Confab));
        let timid = V.verify(
            &p,
            &obs(AgentAction::Abstained, "not in my sources", NOCHUNKS, None),
            "m",
            "c",
        );
        assert_eq!(timid.failure, Some(FailureClass::Misroute));
    }

    #[test]
    fn held_out_answered_is_false_ground() {
        let p = absent(AbsentKind::Adjacent, Some(vec!["winnie".into()]));
        let v = V.verify(
            &p,
            &obs(
                AgentAction::Answered,
                "Winnie killed Verloc",
                NOCHUNKS,
                None,
            ),
            "m",
            "c",
        );
        assert_eq!(v.failure, Some(FailureClass::FalseGround));
    }

    #[test]
    fn score_reduces_to_chaos_scorer() {
        // Two present (one right, one wrong) + one adjacent-abstain → competence
        // 0.5, honesty 1.0. The flywheel scorer is literally chaos::score.
        let p = present(&["verloc"]);
        let a = absent(AbsentKind::Adjacent, None);
        let verdicts = vec![
            V.verify(
                &p,
                &obs(AgentAction::Answered, "Verloc", NOCHUNKS, None),
                "m",
                "c",
            ),
            V.verify(
                &p,
                &obs(AgentAction::Answered, "wrong", NOCHUNKS, None),
                "m",
                "c",
            ),
            V.verify(
                &a,
                &obs(AgentAction::Abstained, "no", NOCHUNKS, None),
                "m",
                "c",
            ),
        ];
        let report = super::super::score(&verdicts);
        assert_eq!(report.competence, 0.5);
        assert_eq!(report.honesty, 1.0);
    }
}
