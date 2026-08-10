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
    /// A **compound** question: one half the corpus answers, one half it
    /// verifiably does not. The correct conduct is neither "answer" nor
    /// "abstain" — it is answer the supported half AND disclose the gap.
    ///
    /// WHY THIS IS A TYPE AND NOT JUST `Present`. These probes were typed
    /// `present` when the compound bank was authored, which made every
    /// situated criterion that keys on `present` treat a half-answer as a
    /// failed whole-answer. Measured 2026-08-05: `answers_the_question`
    /// declined BOTH responses that answered correctly and named their gap
    /// — citing the disclosure itself as the reason — and credited only the
    /// one response that fabricated the missing half. A +2 criterion that
    /// pays for fabrication and fines disclosure steers a harness A/B away
    /// from the exact behaviour the bank exists to elicit, so the type that
    /// selects the criteria had to become the type the probe actually is.
    ///
    /// CHAOS TREATS THIS EXACTLY LIKE `Present`, deliberately. A correct
    /// grounded answer does exist (the supported half) and the AND-match
    /// gold covers precisely that half, so `expected_action`, `is_pass` and
    /// the partition are unchanged — retyping moves no chaos number. The
    /// disclosure half is graded by the situated lane, which is the
    /// division of labour the compound bank was built around. That is also
    /// why [`ExpectedAction`] stays a binary: a third variant would have no
    /// consumer that behaves differently from `Answer`.
    PartiallyPresent,
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
            | QuestionType::SupersededTrap
            | QuestionType::PartiallyPresent => ExpectedAction::Answer,
            QuestionType::AbsentAdjacent | QuestionType::AbsentOutOfDomain => {
                ExpectedAction::Abstain
            }
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
            QuestionType::PartiallyPresent => "partially_present",
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
    /// EPISTEMIC_STATE.md third lane (tracked): for ABSENT questions,
    /// the acquisition class that would genuinely satisfy the gap —
    /// what a well-calibrated conjecture should name. `None` = item
    /// unlabeled (lane skips it). Never set on answerable items (the
    /// fairness contract's spirit: labels describe the gap, not hints).
    #[serde(default)]
    pub acquisition_class: Option<AcquisitionClass>,
}

/// The acquisition-route class that would satisfy an absent question's
/// gap. Mirrors the shape (not the payload) of the runtime's
/// `AcquisitionRoute` so the tracked lane can compare a resolved
/// route's class against the label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionClass {
    /// A catalog recipe corpus would cover it (broad public knowledge).
    InstallRecipe,
    /// The user's own material would cover it (folder / vault / imports).
    ConnectSource,
    /// Current/web information would cover it.
    WebSearch,
    /// Only a specific document the user holds could cover it.
    ProvideDocument,
    /// Nothing reasonably acquirable covers it (fictional adjacent
    /// detail that simply does not exist) — the honest conjecture is
    /// none at all.
    Unknowable,
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
                    // A compound probe makes TWO claims about the corpus —
                    // one half present, one half absent — and only the
                    // present half has a machine-checkable witness
                    // (`gold_keywords`). The absent half's certification
                    // lives in `rationale`, which names the probes it
                    // inherits the absence from. Requiring it here is what
                    // keeps the fairness contract structural instead of a
                    // convention the next bank author may not read: an
                    // uncertified compound probe would assert a gap nobody
                    // ever verified, and the situated lane would then grade
                    // disclosure of a gap that might not exist.
                    if q.qtype == QuestionType::PartiallyPresent && q.rationale.trim().is_empty() {
                        return Err(format!(
                            "partially_present question `{}` must carry a rationale certifying \
                             the ABSENT half — gold_keywords only witness the half the corpus \
                             answers, so nothing else records why the other half is a real gap",
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
        self.questions
            .iter()
            .filter(|q| q.qtype.is_answerable())
            .count()
    }
    pub fn absent_count(&self) -> usize {
        self.questions
            .iter()
            .filter(|q| q.qtype.is_absent())
            .count()
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
            acquisition_class: None,
        }
    }

    #[test]
    fn expected_action_maps_types() {
        assert_eq!(
            QuestionType::Present.expected_action(),
            ExpectedAction::Answer
        );
        assert_eq!(
            QuestionType::Distractor.expected_action(),
            ExpectedAction::Answer
        );
        assert_eq!(
            QuestionType::ProvenanceTrap.expected_action(),
            ExpectedAction::Answer
        );
        assert_eq!(
            QuestionType::AbsentAdjacent.expected_action(),
            ExpectedAction::Abstain
        );
        assert_eq!(
            QuestionType::AbsentOutOfDomain.expected_action(),
            ExpectedAction::Abstain
        );
    }

    /// A compound probe is answerable (its supported half has gold) and
    /// carries its own label, which is what the situated vocabulary selects
    /// on. Pinning the label string matters more than it looks: the criteria
    /// file names it verbatim in `applies_to`, and a silent rename there is a
    /// criterion that never applies to anything.
    #[test]
    fn partially_present_is_answerable_and_labeled() {
        let t = QuestionType::PartiallyPresent;
        assert_eq!(t.expected_action(), ExpectedAction::Answer);
        assert!(t.is_answerable());
        assert!(!t.is_absent());
        assert_eq!(t.label(), "partially_present");
        // The label is also the wire form a bank file writes.
        let parsed: QuestionType = toml::from_str("v = \"partially_present\"\n")
            .map(|w: std::collections::HashMap<String, QuestionType>| w["v"])
            .expect("partially_present must deserialize from its label");
        assert_eq!(parsed, QuestionType::PartiallyPresent);
    }

    /// The compound fairness contract, made structural. `gold_keywords`
    /// witness only the half the corpus ANSWERS; nothing else in the schema
    /// records why the other half is a genuine gap, so the rationale is the
    /// only certification the absent half has and the loader must insist on
    /// it. Without this, a probe could assert a gap nobody verified and the
    /// situated lane would grade disclosure of it.
    #[test]
    fn partially_present_requires_a_rationale() {
        let mut bad = q("c1", QuestionType::PartiallyPresent);
        bad.rationale = "   ".into();
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![bad],
        };
        let err = bank
            .validate()
            .expect_err("a compound probe with no rationale certifies no gap");
        assert!(err.contains("ABSENT half"), "unhelpful error: {err}");

        // The same probe WITH a rationale is fine — the rule is about the
        // certification being present, not about compound probes being hard.
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![q("c1", QuestionType::PartiallyPresent)],
        };
        assert!(bank.validate().is_ok());
        assert_eq!(bank.answerable_count(), 1);
    }

    /// The shipped compound bank must load under the promoted type — this is
    /// the end-to-end check that the retype, the label and the validator rule
    /// agree with the file on disk rather than only with each other.
    #[test]
    fn shipped_compound_bank_loads_as_partially_present() {
        let bench =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/chaos_monkey");
        // Skip only when the bench tree is genuinely absent (filtered
        // checkout). If the tree IS here and the bank is not, that is a
        // missing file or a path that has rotted — a failure, not a skip.
        // A guard that cannot tell those apart turns into a test that has
        // silently never run (§18.1).
        if !bench.is_dir() {
            return;
        }
        let path = bench.join("saltgrass_compound.toml");
        assert!(
            path.is_file(),
            "bench tree is present but {} is not — path rotted or bank removed",
            path.display()
        );
        let bank = ChaosBank::load(&path).expect("shipped compound bank must load");
        assert!(!bank.questions.is_empty());
        for q in &bank.questions {
            assert_eq!(
                q.qtype,
                QuestionType::PartiallyPresent,
                "probe `{}` is not a compound type — every probe in this bank asks for one \
                 fact the corpus holds and one it lacks",
                q.id
            );
        }
        // Every probe answerable means the chaos denominators are unmoved by
        // the retype, which is the claim the bank header makes.
        assert_eq!(bank.answerable_count(), bank.questions.len());
        assert_eq!(bank.absent_count(), 0);
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
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![bad],
        };
        assert!(
            bank.validate().is_err(),
            "present question with no witness is unfair"
        );
    }

    #[test]
    fn absent_with_gold_is_rejected() {
        let mut sneaky = q("a1", QuestionType::AbsentAdjacent);
        sneaky.gold_keywords = vec!["actually answerable".into()];
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![sneaky],
        };
        assert!(
            bank.validate().is_err(),
            "absent question that is secretly answerable is unfair"
        );
    }

    #[test]
    fn duplicate_ids_rejected() {
        let bank = ChaosBank {
            meta: BankMeta::default(),
            questions: vec![
                q("dup", QuestionType::Present),
                q("dup", QuestionType::Distractor),
            ],
        };
        assert!(bank.validate().is_err());
    }

    /// Every bank checked into `bench/chaos_monkey/` must load and pass the
    /// fairness contract. Until 2026-08-04 nothing exercised the shipped
    /// banks, so a malformed one — a new probe missing its witness, a typo'd
    /// qtype — surfaced only when someone armed a multi-hour run against it.
    /// Walks up from CWD because tests run from the crate dir, and skips
    /// silently on a filtered checkout rather than failing for absence.
    #[test]
    fn every_checked_in_bank_loads_and_is_fair() {
        let mut here = std::env::current_dir().expect("cwd");
        let dir = loop {
            let candidates = [
                here.join("bench/chaos_monkey"),
                here.join("sovereign/bench/chaos_monkey"),
            ];
            if let Some(d) = candidates.into_iter().find(|c| c.is_dir()) {
                break d;
            }
            if !here.pop() {
                return; // filtered checkout — absence is not a failure
            }
        };

        let mut checked = 0usize;
        for entry in std::fs::read_dir(&dir).expect("read bank dir").flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            // The directory also holds the bench manifest, which is a
            // different schema. Identify banks by their content rather than
            // by name, so a new non-bank file never breaks this and a new
            // bank is picked up without editing an allowlist.
            let text = std::fs::read_to_string(&p).expect("read toml");
            if !text.contains("[[questions]]") {
                continue;
            }
            let bank = ChaosBank::load(&p)
                .unwrap_or_else(|e| panic!("checked-in bank {} is not fair: {e}", p.display()));
            assert!(
                !bank.questions.is_empty(),
                "bank {} has no questions",
                p.display()
            );
            checked += 1;
        }
        // A floor, not an exact count: this must fail loudly if bank
        // discovery silently stops finding anything.
        assert!(
            checked >= 3,
            "expected at least 3 banks in {}, found {checked}",
            dir.display()
        );
    }
}
