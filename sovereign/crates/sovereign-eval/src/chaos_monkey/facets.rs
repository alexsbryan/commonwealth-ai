// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **mechanical** facets of a chaos row: the ones decided by the
//! deterministic witness kernel, with no model in the loop.
//!
//! These used to be inline in `bench_cmd::chaos_monkey::score_question`, where
//! they were reachable only by an `async fn` that also talks to two model
//! endpoints — so "is the scorer bit-stable?" could not be asked without a
//! daemon, and `NATIVE_GROUNDING.md §7.2`'s determinism floor could not be
//! pinned by a test. Lifting them here is a move, not a copy: `score_question`
//! calls this, so there is exactly one implementation of each rule (ARCH §10.6,
//! one decider one name).
//!
//! Everything in this module is a pure function of
//! `(question, visible answer, retrieved chunks, draft, answered)`. Nothing
//! here allocates a provider, reads the clock, hashes an address, or consults
//! the environment — which is what makes the repeat-stability test below a
//! real gate rather than a hopeful one.

use serde::{Deserialize, Serialize};

use super::question::{ChaosQuestion, QuestionType};
use crate::flywheel::det_checks::{contains_ci, gold_match};

/// The deterministic verdicts for one probe.
///
/// `Option` means *not applicable to this probe* (wrong qtype, no witness,
/// the agent abstained) — never "we could not tell". A check that cannot be
/// run reports absence; it never defaults to a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MechanicalFacets {
    /// Did the visible answer match the bank's gold witness by FORMS
    /// (`gold_match`, which handles `|`-OR groups)? `false` on an answerable
    /// probe is what escalates the caller to the LLM correctness judge — the
    /// one place a model still touches correctness.
    pub gold_forms_hit: bool,
    /// Was the gold answer in the RETRIEVED chunks at all? `Some(false)` on an
    /// abstained answerable probe is a retrieval miss, not a gate fault.
    pub retrieval_present: Option<bool>,
    /// Was the PRE-GATE draft correct? `None` when no draft was recorded.
    pub draft_correct: Option<bool>,
    /// Distractor probes: was the answer led by the wrong passage?
    pub used_distractor: Option<bool>,
    /// ProvenanceTrap probes: did the genuinely-supporting passage reach
    /// retrieval?
    pub citation_faithful: Option<bool>,
    /// SupersededTrap probes: did the answer ground itself in the dead law?
    pub cited_obsolete: Option<bool>,
}

/// Decide every model-free facet for one probe.
///
/// `answered` is the caller's already-decided answer-vs-abstain verdict (the
/// gate action, the typed ledger verdict, or the forced-choice judge). It is an
/// input rather than something re-derived here precisely because it is the one
/// facet that is NOT mechanical.
pub fn mechanical_facets(
    q: &ChaosQuestion,
    visible: &str,
    chunk_texts: &[String],
    draft: Option<&str>,
    answered: bool,
) -> MechanicalFacets {
    let answerable = q.qtype.is_answerable();
    MechanicalFacets {
        gold_forms_hit: gold_match(visible, &q.gold_keywords),
        retrieval_present: answerable
            .then(|| gold_match(&chunk_texts.join(" \n "), &q.gold_keywords)),
        draft_correct: match (answerable, draft) {
            (true, Some(d)) => Some(gold_match(d, &q.gold_keywords)),
            _ => None,
        },
        used_distractor: match (&q.distractor_quote, answered) {
            (Some(sig), true) => Some(contains_ci(visible, sig)),
            _ => None,
        },
        citation_faithful: match (q.qtype, &q.supporting_quote, answered) {
            (QuestionType::ProvenanceTrap, Some(sig), true) => {
                Some(chunk_texts.iter().any(|c| contains_ci(c, sig)))
            }
            _ => None,
        },
        cited_obsolete: match (q.qtype, &q.obsolete_quote, answered) {
            (QuestionType::SupersededTrap, Some(sig), true) => Some(contains_ci(visible, sig)),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chaos_monkey::ChaosBank;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    fn repo_bench() -> PathBuf {
        // sovereign/crates/sovereign-eval → sovereign/bench
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/chaos_monkey")
    }

    /// One canonical digest over every mechanical facet of a whole transcript.
    fn digest(rows: &[(String, MechanicalFacets)]) -> String {
        let mut h = Sha256::new();
        for (id, f) in rows {
            h.update(id.as_bytes());
            h.update(serde_json::to_vec(f).expect("facets serialize").as_slice());
        }
        hex::encode(h.finalize())
    }

    fn replay(bank: &ChaosBank, transcript: &str) -> Vec<(String, MechanicalFacets)> {
        let by_id: std::collections::HashMap<&str, &ChaosQuestion> =
            bank.questions.iter().map(|q| (q.id.as_str(), q)).collect();
        let mut out = Vec::new();
        for line in transcript.lines().filter(|l| !l.trim().is_empty()) {
            let rec: serde_json::Value = serde_json::from_str(line).expect("transcript line");
            let id = rec.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let Some(q) = by_id.get(id) else { continue };
            let visible = rec.get("answer").and_then(|v| v.as_str()).unwrap_or("");
            let chunks: Vec<String> = rec
                .get("retrieved_chunks")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let draft = rec.get("draft").and_then(|v| v.as_str());
            let answered = rec.get("agent_action").and_then(|v| v.as_str()) == Some("Answered");
            out.push((
                id.to_string(),
                mechanical_facets(q, visible, &chunks, draft, answered),
            ));
        }
        out
    }

    /// `NATIVE_GROUNDING.md §7.2`, scorer-determinism floor: the mechanical
    /// facets of a FROZEN transcript are bit-stable across repeats. This is the
    /// requirement every new native facet inherits — a statistic that moves
    /// between two replays of the same bytes cannot be a HARD-lane metric.
    #[test]
    fn mechanical_facets_are_bit_stable_across_three_repeats() {
        let bank_path = repo_bench().join("secret_agent.toml");
        let transcript_path = repo_bench().join("results/secret_agent_before.transcripts.jsonl");
        let bank =
            ChaosBank::load(&bank_path).unwrap_or_else(|e| panic!("load bank {bank_path:?}: {e}"));
        let text = std::fs::read_to_string(&transcript_path)
            .unwrap_or_else(|e| panic!("read transcript {transcript_path:?}: {e}"));

        let runs: Vec<Vec<(String, MechanicalFacets)>> =
            (0..3).map(|_| replay(&bank, &text)).collect();

        // A replay that matched nothing proves nothing (house rule: a zero-row
        // run is never green).
        assert!(
            runs[0].len() >= 40,
            "expected the 43-probe transcript to replay; got {} rows — the fixture moved",
            runs[0].len()
        );
        let d0 = digest(&runs[0]);
        for (i, r) in runs.iter().enumerate().skip(1) {
            assert_eq!(runs[0], *r, "repeat {i} disagreed with repeat 0 field-wise");
            assert_eq!(
                digest(r),
                d0,
                "repeat {i} disagreed with repeat 0 by digest"
            );
        }
    }

    /// The instrument's own falsifiability check: the digest MUST move when a
    /// facet moves. A stability test that would pass on a constant is not a
    /// test (ARCH §18.1 — a check with no failing input you can name).
    #[test]
    fn the_digest_notices_a_flipped_facet() {
        let a = vec![(
            "probe".to_string(),
            MechanicalFacets {
                gold_forms_hit: true,
                retrieval_present: Some(true),
                draft_correct: None,
                used_distractor: None,
                citation_faithful: None,
                cited_obsolete: None,
            },
        )];
        let mut b = a.clone();
        b[0].1.retrieval_present = Some(false);
        assert_ne!(digest(&a), digest(&b));
    }

    #[test]
    fn facets_that_need_an_answer_stay_absent_on_an_abstention() {
        let bank_path = repo_bench().join("secret_agent.toml");
        let bank = ChaosBank::load(&bank_path).expect("bank loads");
        let q = bank
            .questions
            .iter()
            .find(|q| q.distractor_quote.is_some())
            .cloned()
            .unwrap_or_else(|| bank.questions[0].clone());
        let f = mechanical_facets(&q, "", &[], None, false);
        assert_eq!(f.used_distractor, None);
        assert_eq!(f.citation_faithful, None);
        assert_eq!(f.cited_obsolete, None);
    }
}
