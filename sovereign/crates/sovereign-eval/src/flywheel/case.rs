// SPDX-License-Identifier: AGPL-3.0-or-later
//! The regression-case store (G3): every detected failure becomes a durable,
//! replayable case. Append-only JSONL so capture is a cheap append — never a
//! whole-bank rewrite — and the fairness contract is re-checked at capture AND
//! at load, so the suite can never accrue an unfair case.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::chaos_monkey::ExpectedAction;
use crate::flywheel::probe::{Oracle, Probe};
use crate::flywheel::verify::{Determinism, FailureClass};

/// The §7 fairness invariant at the probe level: an answerable probe MUST carry
/// a non-empty witness; an absent probe MUST carry no in-corpus witness. This
/// is the generalization of `ChaosBank::validate`'s per-question rule, enforced
/// at generate, capture, AND load — three gates, one contract.
///
/// (The held-out-slice marker on `Oracle::Absent` is metadata about a *withheld*
/// document, not an in-corpus witness, so it does not make an absent probe unfair.)
pub fn validate_fairness(probe: &Probe) -> Result<(), String> {
    match (probe.expected_action(), &probe.oracle) {
        (ExpectedAction::Answer, Oracle::Witness { gold_keywords, .. }) => {
            if gold_keywords.is_empty() {
                return Err(format!(
                    "answerable probe `{}` must carry gold_keywords (the witness that an answer exists)",
                    probe.id
                ));
            }
            Ok(())
        }
        (ExpectedAction::Answer, _) => {
            Err(format!("answerable probe `{}` must have a Witness oracle", probe.id))
        }
        (ExpectedAction::Abstain, Oracle::Absent { .. }) => Ok(()),
        (ExpectedAction::Abstain, _) => Err(format!(
            "absent probe `{}` must have an Absent oracle (no in-corpus witness)",
            probe.id
        )),
    }
}

/// One durable, replayable failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionCase {
    pub id: String,
    pub probe: Probe,
    pub failure: FailureClass,
    pub determinism: Determinism,
    #[serde(default)]
    pub captured_answer_excerpt: String,
    #[serde(default)]
    pub captured_chunks: Vec<String>,
    pub corpus: String,
    pub model_id: String,
    pub captured_at: String,
    #[serde(default)]
    pub source_run: String,
}

impl RegressionCase {
    /// Stable dedup key — re-failing the same probe on the same corpus must not
    /// append a duplicate (idempotent capture).
    pub fn dedup_key(&self) -> String {
        format!("{}|{}", self.corpus, self.probe.query)
    }
}

/// An append-only JSONL bank of regression cases.
#[derive(Debug, Default, Clone)]
pub struct RegressionBank {
    pub cases: Vec<RegressionCase>,
}

impl RegressionBank {
    /// Load a bank, re-validating every case's fairness. A case whose probe
    /// violates the contract is rejected (a hand-edited bank can't smuggle one
    /// in). Missing file = empty bank (first run).
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("could not read regression bank {path:?}: {e}")),
        };
        let mut cases = Vec::new();
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let case: RegressionCase = serde_json::from_str(line)
                .map_err(|e| format!("{path:?}:{}: malformed regression case: {e}", lineno + 1))?;
            validate_fairness(&case.probe)
                .map_err(|e| format!("{path:?}:{}: unfair regression case: {e}", lineno + 1))?;
            cases.push(case);
        }
        Ok(Self { cases })
    }

    fn contains(&self, key: &str) -> bool {
        self.cases.iter().any(|c| c.dedup_key() == key)
    }

    /// Capture a case: refuse if unfair, skip if already present, else append.
    /// Returns `true` when newly captured. Idempotent on the dedup key — the
    /// optimizer re-failing the same probe never grows the bank.
    pub fn capture(path: &Path, case: &RegressionCase) -> Result<bool, String> {
        validate_fairness(&case.probe)
            .map_err(|e| format!("refusing to capture unfair case `{}`: {e}", case.id))?;
        let existing = Self::load(path)?;
        if existing.contains(&case.dedup_key()) {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        let line = serde_json::to_string(case).map_err(|e| format!("serialize case: {e}"))?;
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("open {path:?} for append: {e}"))?;
        writeln!(f, "{line}").map_err(|e| format!("append {path:?}: {e}"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chaos_monkey::QuestionType;
    use crate::flywheel::probe::{AbsentKind, Oracle, Probe, ProbeSource};

    fn present_probe(gold: Vec<String>) -> Probe {
        Probe {
            id: "p1".into(),
            query: "who runs the shop?".into(),
            qtype: QuestionType::Present,
            oracle: Oracle::Witness { gold_keywords: gold, supporting_quote: None, distractor_quote: None },
            source: ProbeSource::I1Corpus,
            note: String::new(),
        }
    }

    fn absent_probe() -> Probe {
        Probe {
            id: "a1".into(),
            query: "capital of Australia?".into(),
            qtype: QuestionType::AbsentAdjacent,
            oracle: Oracle::Absent { held_out_witness: None, kind: AbsentKind::Adjacent },
            source: ProbeSource::I1Corpus,
            note: String::new(),
        }
    }

    #[test]
    fn fairness_accepts_witnessed_answerable_and_witnessless_absent() {
        assert!(validate_fairness(&present_probe(vec!["verloc".into()])).is_ok());
        assert!(validate_fairness(&absent_probe()).is_ok());
    }

    #[test]
    fn fairness_rejects_witnessless_answerable() {
        assert!(validate_fairness(&present_probe(vec![])).is_err());
    }

    #[test]
    fn fairness_rejects_answerable_with_absent_oracle() {
        let mut p = present_probe(vec!["x".into()]);
        p.oracle = Oracle::Absent { held_out_witness: None, kind: AbsentKind::Adjacent };
        assert!(validate_fairness(&p).is_err(), "an answerable probe with no Witness oracle is unfair");
    }

    #[test]
    fn capture_round_trips_dedups_and_rejects_unfair() {
        let dir = std::env::temp_dir().join("flywheel_case_unit");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("regressions.jsonl");

        let case = RegressionCase {
            id: "r1".into(),
            probe: present_probe(vec!["verloc".into()]),
            failure: FailureClass::FalsePossum,
            determinism: Determinism::Deterministic,
            captured_answer_excerpt: "I don't know".into(),
            captured_chunks: vec![],
            corpus: "secret-agent".into(),
            model_id: "primary".into(),
            captured_at: "2026-06-08T00:00:00Z".into(),
            source_run: "unit".into(),
        };

        assert!(RegressionBank::capture(&path, &case).unwrap(), "first capture appends");
        assert!(!RegressionBank::capture(&path, &case).unwrap(), "duplicate is skipped");

        let bank = RegressionBank::load(&path).unwrap();
        assert_eq!(bank.cases.len(), 1);
        assert_eq!(bank.cases[0].probe.query, "who runs the shop?", "probe replays verbatim");

        // An unfair case is refused at capture.
        let mut unfair = case.clone();
        unfair.id = "r2".into();
        unfair.probe = present_probe(vec![]); // witnessless answerable
        unfair.corpus = "other".into();
        assert!(RegressionBank::capture(&path, &unfair).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
