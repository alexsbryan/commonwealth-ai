// SPDX-License-Identifier: AGPL-3.0-or-later
//! When a retrieval bench must refuse to produce a number at all.
//!
//! A COULD-NOT-JUDGE, not a zero and not a pass. The retrieval pipeline
//! reports every corpus it would have searched and could not: a local index
//! that never finished building, one built at another embedding dimension, or
//! a mesh peer that refused, timed out, or was never reached. Any of those
//! means the evidence pool the question is about to be scored against was
//! assembled WITHOUT a corpus that was in scope for it, so the number would
//! measure the loss rather than the retrieval.
//!
//! THE MESH CASE IS WHY THIS EXISTS. `commonwealth-api`'s
//! `routes_knowledge.rs` deliberately swallows per-peer fan-out failures into
//! `corpora_unavailable` so one sleepy peer cannot take an interactive query
//! down. That is correct for a human and fatal for a measurement: the run
//! COMPLETES and reports a score computed on partial retrieval — success-
//! shaped and wrong (ARCH §18.3). The fix belongs on the measurement side and
//! NOT in the fan-out, because the fan-out's swallowing is right for the
//! surface it serves.
//!
//! It reuses the refusal path that already exists rather than minting a
//! second one (§10.6): the caller turns a `Some(_)` into `EvalResult::error`
//! via `with_error`, `bench_cmd::all::drop_unmeasured` then excludes errored
//! rows from the baseline comparison instead of scoring them 0.0, and a bank
//! where every row errored classifies as `BenchStatus::Stale` — "unmeasured,
//! not regressed" — which already exits non-zero.
//!
//! A run WITHOUT `--isolate` on a host carrying unready corpora will now go
//! Stale where it used to print a number. That is the finding, not a
//! regression: those runs were already scoring a pool a corpus was missing
//! from. `--isolate` narrows the in-scope set to the bank\'s own corpus, so
//! the CI lane is unaffected unless the corpus it targets is genuinely lost.

use sovereign_core::traits::CorpusUnavailable;

/// Whether this question may be scored at all, given what retrieval lost.
///
/// ONE decider (§10.6), and the only place the bench turns a lost corpus into
/// a verdict. `None` = nothing was lost, score it. `Some(why)` = a
/// COULD-NOT-JUDGE, and `why` names every corpus and its reason so the
/// operator does not have to re-run to find out which.
///
/// Why refuse on ANY entry: the pipeline records a corpus here only if it was
/// IN SCOPE for the turn — corpora the conversation disabled, or ones outside
/// the principal ceiling, are filtered out before the record is written
/// (`retrieval/corpus_search.rs`, Filters 3-5). So a non-empty set means the
/// evidence pool about to be scored was assembled without something that was
/// supposed to be in it, and the resulting number measures the loss rather
/// than the retrieval.
pub(crate) fn refusal_for_lost_corpora(lost: &[CorpusUnavailable]) -> Option<String> {
    if lost.is_empty() {
        return None;
    }
    let named: Vec<String> = lost
        .iter()
        .map(|u| format!("{} ({})", u.corpus_id, u.reason.log_tag()))
        .collect();
    Some(format!(
        "retrieval lost {} corpus/corpora that were in scope for this question — \
         refusing to score a partial pool: {}",
        named.len(),
        named.join(", ")
    ))
}

#[cfg(test)]
mod lost_corpora_refusal_tests {
    use super::refusal_for_lost_corpora;
    use sovereign_core::traits::{CorpusUnavailable, UnavailabilityReason};

    /// The no-regression bar. A turn that lost nothing must still be SCORED —
    /// a refusal that fires on the happy path would turn every green lane into
    /// a could-not-judge, which is the opposite failure.
    #[test]
    fn a_turn_that_lost_nothing_is_scored() {
        assert_eq!(refusal_for_lost_corpora(&[]), None);
    }

    /// The mesh case this exists for: a peer that could not serve costs the
    /// pool a corpus, and the run must say could-not-judge instead of
    /// reporting a number computed without it.
    #[test]
    fn a_peer_that_could_not_serve_refuses_the_score() {
        let why = refusal_for_lost_corpora(&[CorpusUnavailable::new(
            "sep",
            UnavailabilityReason::PeerUnreachable,
        )])
        .expect("a lost corpus must refuse the score");
        assert!(why.contains("sep"), "must name the corpus: {why}");
        assert!(
            why.contains("peer_unreachable"),
            "must name the reason so the operator need not re-run: {why}"
        );
    }

    /// Local readiness losses refuse on the same terms. Scoring retrieval
    /// against a corpus that never finished building measures the build, not
    /// the retrieval — and `--prod-pipeline` is the lane where that shows up.
    #[test]
    fn a_local_readiness_loss_refuses_too_and_names_every_corpus() {
        let why = refusal_for_lost_corpora(&[
            CorpusUnavailable::new("sep", UnavailabilityReason::NotBuilt),
            CorpusUnavailable::new(
                "wikipedia",
                UnavailabilityReason::DimMismatch { built: 768 },
            ),
        ])
        .expect("lost corpora must refuse the score");
        assert!(
            why.contains("sep") && why.contains("index_not_built"),
            "{why}"
        );
        assert!(
            why.contains("wikipedia") && why.contains("dim_mismatch"),
            "{why}"
        );
        assert!(why.contains('2'), "must say how many were lost: {why}");
    }
}
