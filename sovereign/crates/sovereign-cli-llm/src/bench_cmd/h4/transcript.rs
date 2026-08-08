// SPDX-License-Identifier: AGPL-3.0-or-later
//! The H4 replay's input: one chaos `*.transcripts.jsonl` row, typed.
//!
//! **One reader, two consumers.** The sentence sweep (deliverable 2) and the
//! H4 gate (deliverable 4) read the same three things out of a transcript — the
//! turn's evidence, its released answer, and the incumbent's per-claim verdicts
//! — so they read them through one module rather than two `serde_json::Value`
//! key-picks that can drift apart (principle 8).
//!
//! **Why this is not `situated::transcripts::Transcript`.** That type exists and
//! is loaded the same way, but its doc comment says what it is: *"Deliberately a
//! SUBSET of the row's fields — the lane reads the response and the probe's
//! identity and nothing else, so adding fields chaos-side never breaks it."* It
//! carries no `retrieved_chunks`, no `epistemic_state` and no `violation_prob`,
//! which are exactly the three H4 needs. Widening it would change an instrument
//! the situated lane is calibrated against, to serve a measurement that lane
//! does not run. So: a second view of the same rows, and no edit to the first.
//!
//! **Where the per-claim verdicts actually live.** The incumbent's longform
//! ladder retains `GateOutcome.claims` (`grounding/mod.rs:578`) — one
//! `GateClaim { text, supported, failed_once, violation_prob }` per audited
//! claim — and the ledger renders them into the turn's `epistemic_state` as
//! `holdings[]`, each carrying `claim` and `verification`. That rendering is
//! what a frozen transcript preserves, and it is what H4's agreement
//! measurement replays against. Nothing here re-invokes a judge.

use std::path::Path;

use serde::Deserialize;

/// One audited claim, as the incumbent's ledger froze it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Holding {
    /// The claim text the ladder judged.
    #[serde(default)]
    pub claim: String,
    /// The incumbent's final verdict on it. Observed vocabulary across the
    /// three committed transcripts: `verified`, `failed_once`. Kept as a
    /// string rather than an enum precisely because it is an OPEN set written
    /// by another subsystem — §2/§4: closed sets are enums, and this one is not
    /// ours to close. [`Holding::supported`] is the one place it is
    /// interpreted.
    #[serde(default)]
    pub verification: String,
}

impl Holding {
    /// Did the incumbent hold this claim supported at release?
    ///
    /// `Some(true)` / `Some(false)` for verdicts we recognise; **`None` for a
    /// verdict string we do not** — could-not-judge, not "unsupported". A new
    /// verdict word appearing chaos-side must show up as an unreadable claim in
    /// the report, not silently as a disagreement (§18.3).
    ///
    /// `failed_once` reads as SUPPORTED because that is what it means in
    /// `GateClaim` (`grounding/mod.rs:589`): *"the first check failed and the
    /// claim went through a retry / rewrite / annotation before release"* — the
    /// released text's version of the claim did verify. Reading it as a failure
    /// would score the incumbent against a verdict it did not reach.
    pub fn supported(&self) -> Option<bool> {
        match self.verification.as_str() {
            "verified" | "failed_once" => Some(true),
            "unsupported" | "failed" | "unverified" => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct EpistemicState {
    #[serde(default)]
    holdings: Vec<Holding>,
}

/// One frozen turn, as H4 needs to read it.
#[derive(Debug, Clone, Deserialize)]
pub struct ReplayTurn {
    /// The probe id.
    pub id: String,
    #[serde(default)]
    pub question: String,
    /// The visible, post-gate answer — the released artifact.
    #[serde(default)]
    pub answer: String,
    /// The sealed evidence for the turn, verbatim chunk text.
    #[serde(default)]
    pub retrieved_chunks: Vec<String>,
    /// The gate's own persisted action. Read, never re-derived.
    #[serde(default)]
    pub gate_action: Option<String>,
    /// The Critic's violation probability for the turn, when a `--gv-shadow` or
    /// `--grounding-verify` run recorded one. `None` means the run did not ask
    /// the Critic — **not** that the turn was clean. Twelve of the fifteen
    /// committed chaos artifacts carry this key as `null` on every row for
    /// exactly that reason (§4's 2026-08-07 correction).
    #[serde(default)]
    pub violation_prob: Option<f64>,
    #[serde(default)]
    epistemic_state: Option<EpistemicState>,
}

impl ReplayTurn {
    /// The incumbent's per-claim verdicts for this turn. Empty when the ladder
    /// audited no claim (a decline, a NO_CLAIM release, a judge fail-open).
    pub fn holdings(&self) -> &[Holding] {
        self.epistemic_state
            .as_ref()
            .map_or(&[], |e| e.holdings.as_slice())
    }

    /// A turn is replayable when it has both a released answer and evidence to
    /// resolve it against. Anything else is could-not-judge and is reported as
    /// such rather than scored.
    pub fn is_replayable(&self) -> bool {
        !self.answer.trim().is_empty()
            && self.retrieved_chunks.iter().any(|c| !c.trim().is_empty())
    }
}

/// Load a chaos `*.transcripts.jsonl`.
///
/// A malformed line is reported and skipped, and the skip count is RETURNED —
/// never swallowed, because a replay over half its bank is not the replay you
/// think you are reading. (The same contract, and the same reasoning, as
/// `situated::transcripts::load`.) Rows are sorted by id so two runs of the
/// same file produce byte-identical output.
pub fn load(path: &Path) -> Result<(Vec<ReplayTurn>, usize), String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut rows = Vec::new();
    let mut skipped = 0usize;
    for (n, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ReplayTurn>(line) {
            Ok(t) => rows.push(t),
            Err(e) => {
                skipped += 1;
                eprintln!("bench h4: {}:{} unreadable — {e}", path.display(), n + 1);
            }
        }
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((rows, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frozen two-row fixture in the exact shape chaos writes
    /// (`chaos_monkey.rs:772`): the same keys, the same nesting, including the
    /// `violation_prob: null` that twelve of the fifteen committed artifacts
    /// carry on every row.
    const FIXTURE: &str = r#"{"id":"b-turn","qtype":"present","question":"Who?","answer":"Karl Yundt giggled.","retrieved_chunks":["Karl Yundt giggled grimly."],"gate_action":"citation_grounded","violation_prob":0.25,"epistemic_state":{"version":1,"holdings":[{"claim":"Yundt giggled","provenance":{"corpus":{"corpus_id":"c","chunk_id":null}},"verification":"verified"},{"claim":"a second one","provenance":{},"verification":"failed_once"}],"gaps":[],"verdict":"grounded"}}
{"id":"a-turn","qtype":"absent","question":"Where?","answer":"","retrieved_chunks":[],"gate_action":"abstained","violation_prob":null,"epistemic_state":{"version":1,"holdings":[],"gaps":[],"verdict":"cannot_know_from_here"}}

not json at all
"#;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.transcripts.jsonl");
        std::fs::write(&p, FIXTURE).unwrap();
        (dir, p)
    }

    #[test]
    fn the_frozen_fixture_parses_to_exactly_what_it_says() {
        let (_d, p) = fixture();
        let (rows, skipped) = load(&p).unwrap();
        assert_eq!(skipped, 1, "the malformed line is counted, not swallowed");
        assert_eq!(rows.len(), 2);
        // Sorted by id — "a-turn" before "b-turn" despite the file order.
        assert_eq!(rows[0].id, "a-turn");
        assert_eq!(rows[1].id, "b-turn");

        let b = &rows[1];
        assert_eq!(b.answer, "Karl Yundt giggled.");
        assert_eq!(b.retrieved_chunks.len(), 1);
        assert_eq!(b.gate_action.as_deref(), Some("citation_grounded"));
        assert_eq!(b.violation_prob, Some(0.25));
        assert_eq!(b.holdings().len(), 2);
        assert_eq!(b.holdings()[0].verification, "verified");
        assert!(b.is_replayable());
    }

    #[test]
    fn a_null_violation_prob_is_absent_not_zero() {
        let (_d, p) = fixture();
        let (rows, _) = load(&p).unwrap();
        assert_eq!(
            rows[0].violation_prob, None,
            "a run that never asked the Critic must not read as a clean turn"
        );
    }

    #[test]
    fn an_abstained_turn_with_no_evidence_is_not_replayable() {
        let (_d, p) = fixture();
        let (rows, _) = load(&p).unwrap();
        assert!(
            !rows[0].is_replayable(),
            "no answer and no evidence is could-not-judge, not a failure"
        );
        assert!(rows[0].holdings().is_empty());
    }

    #[test]
    fn failed_once_means_supported_at_release() {
        // GateClaim (grounding/mod.rs:589): failed_once == the claim was fixed
        // BEFORE release. Reading it as a failure would score the incumbent
        // against a verdict it never reached.
        assert_eq!(
            Holding {
                claim: "x".into(),
                verification: "failed_once".into()
            }
            .supported(),
            Some(true)
        );
        assert_eq!(
            Holding {
                claim: "x".into(),
                verification: "verified".into()
            }
            .supported(),
            Some(true)
        );
        assert_eq!(
            Holding {
                claim: "x".into(),
                verification: "unsupported".into()
            }
            .supported(),
            Some(false)
        );
    }

    #[test]
    fn an_unknown_verdict_word_is_could_not_judge_not_a_disagreement() {
        assert_eq!(
            Holding {
                claim: "x".into(),
                verification: "some_new_word".into()
            }
            .supported(),
            None,
            "an open set written by another subsystem must degrade to \
             could-not-judge, never to a silent verdict"
        );
    }

    #[test]
    fn loading_is_stable_across_repeats() {
        let (_d, p) = fixture();
        let (a, _) = load(&p).unwrap();
        let (b, _) = load(&p).unwrap();
        let ids: Vec<&str> = a.iter().map(|r| r.id.as_str()).collect();
        let ids2: Vec<&str> = b.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ids2);
    }
}
