// SPDX-License-Identifier: AGPL-3.0-or-later
//! The answer-surface half of the unavailability contract.
//!
//! Retrieval knows, by name, every corpus it would have searched and could
//! not — `PipelineState::unavailable_corpora`, written from both loss sites
//! (the local readiness filter and the mesh fan-out). This module is what
//! turns that knowledge into something the USER sees.
//!
//! # Why this is code and not a prompt
//!
//! ARCH §7.6: never ask a model to guarantee what code can enforce. The
//! readiness-disclosure step in `retrieval_pipeline` injects assistant
//! guidance asking the model to phrase a refusal, and that is fine as far as
//! it goes — but it only fires when the pool is EMPTY, and a model can always
//! decline to relay it. The failure this module exists to close is the
//! opposite shape (`MESH_SCALE_100_USERS_1000_CORPORA.md` §9.6): the pool was
//! FULL, of an unrelated corpus, and the answer read as confidently grounded
//! while five named corpora had gone missing. No prompt fixes that, because
//! the model was never told anything was missing. The marker below is
//! appended by code, on a branch the model cannot influence.
//!
//! # The no-regression contract
//!
//! `unavailability_marker(&[])` is `None` and
//! `append_unavailability_marker(a, &[])` is `a`, byte for byte. A turn that
//! lost nothing renders exactly as it did before this module existed — the
//! whole feature is invisible until something is actually missing.

use crate::traits::CorpusUnavailable;

/// How many corpora we name before summarising the rest. Five peer-only
/// corpora went missing in the §9.6 probe and naming all five is still
/// readable; a hundred would not be.
const MAX_NAMED: usize = 5;

/// The marker's leading token. Also the idempotence key — an answer that
/// already carries a marker is never given a second one.
const MARKER_PREFIX: &str = "_Sources unavailable:";

/// Render the turn's losses as one line the user reads, or `None` when
/// nothing was lost.
///
/// This is a PURE function of the loss list: same losses, same line, no
/// model, no I/O, no clock. That is what makes it assertable in a lane.
pub(crate) fn unavailability_marker(losses: &[CorpusUnavailable]) -> Option<String> {
    if losses.is_empty() {
        return None;
    }
    let named: Vec<String> = losses
        .iter()
        .take(MAX_NAMED)
        .map(|l| format!("{} ({})", l.corpus_id, l.reason.user_phrase()))
        .collect();
    let mut line = format!("{MARKER_PREFIX} {}", named.join("; "));
    let remaining = losses.len().saturating_sub(MAX_NAMED);
    if remaining > 0 {
        line.push_str(&format!(" and {remaining} more"));
    }
    line.push_str(". This answer does not draw on ");
    line.push_str(if losses.len() == 1 { "it." } else { "them." });
    line.push('_');
    Some(line)
}

/// Append the marker to a rendered answer.
///
/// Call this AFTER the gap check, so a refined answer carries it too — the
/// same position and the same reason as the quote-verification guardrail it
/// sits beside. Idempotent: an answer that already carries a marker is
/// returned unchanged, so a path that runs the append twice cannot stutter.
pub(crate) fn append_unavailability_marker(answer: &str, losses: &[CorpusUnavailable]) -> String {
    let Some(marker) = unavailability_marker(losses) else {
        return answer.to_string();
    };
    if answer.contains(MARKER_PREFIX) {
        return answer.to_string();
    }
    tracing::info!(
        target: "retrieval.pipeline",
        unavailable = losses.len(),
        corpora = ?losses.iter().map(|l| l.corpus_id.as_str()).collect::<Vec<_>>(),
        "answer surface: appended unavailability marker"
    );
    if answer.trim().is_empty() {
        return marker;
    }
    format!("{}\n\n{marker}", answer.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::UnavailabilityReason;

    fn peer(id: &str) -> CorpusUnavailable {
        CorpusUnavailable::new(id, UnavailabilityReason::PeerUnreachable)
    }

    /// THE no-regression bar from the order: an N-available turn renders byte
    /// for byte as it did before. If this ever fails, the feature has started
    /// charging every honest turn for a defect it doesn't have.
    #[test]
    fn no_losses_leaves_the_answer_byte_identical() {
        let answer = "Maple House was built in 1892.\n\n[Source: maple-house]";
        assert_eq!(unavailability_marker(&[]), None);
        assert_eq!(append_unavailability_marker(answer, &[]), answer);
    }

    /// The §9.6 shape, at the surface: `maple-house` was requested, the peer
    /// refused, and the pool filled with an unrelated local corpus. The
    /// answer must NAME the loss.
    #[test]
    fn the_9_6_substitution_names_the_missing_corpus() {
        let substituted = "Parcel 1234 is assessed at $1.2M.\n\n[Source: sf-assessor-roll]";
        let out = append_unavailability_marker(substituted, &[peer("maple-house")]);
        assert!(
            out.contains("maple-house"),
            "the corpus the request NAMED must appear in the answer; got: {out}"
        );
        assert!(
            out.starts_with(substituted),
            "the substituted answer is not rewritten, only marked; got: {out}"
        );
        assert!(
            out.contains("does not draw on it"),
            "the answer must say it did not draw on the missing corpus; got: {out}"
        );
    }

    /// The 89d5f75a shape: a local corpus that never finished building. Same
    /// field, same marker — one defect family, one disclosure.
    #[test]
    fn a_local_unready_corpus_marks_the_same_way() {
        let out = append_unavailability_marker(
            "Here is what I found elsewhere.",
            &[CorpusUnavailable::new(
                "my-project",
                UnavailabilityReason::NotBuilt,
            )],
        );
        assert!(out.contains("my-project"), "got: {out}");
        assert!(
            out.contains("hasn't finished building yet"),
            "the plain-language cause must reach the user; got: {out}"
        );
    }

    /// Five losses is the §9.6 count and all five are named; past that we
    /// summarise rather than wall the user.
    #[test]
    fn many_losses_name_five_then_summarise() {
        let five: Vec<_> = ["a", "b", "c", "d", "e"].iter().map(|i| peer(i)).collect();
        let line = unavailability_marker(&five).expect("five losses render");
        for id in ["a", "b", "c", "d", "e"] {
            assert!(line.contains(id), "all five named; got: {line}");
        }
        assert!(!line.contains("more"), "no summary at five; got: {line}");

        let seven: Vec<_> = ["a", "b", "c", "d", "e", "f", "g"]
            .iter()
            .map(|i| peer(i))
            .collect();
        let line = unavailability_marker(&seven).expect("seven losses render");
        assert!(line.contains("and 2 more"), "got: {line}");
        assert!(line.contains("them."), "plural form; got: {line}");
    }

    /// A second pass over an already-marked answer must not stutter — the KQ
    /// path runs a refinement that can re-enter the append.
    #[test]
    fn appending_twice_marks_once() {
        let once = append_unavailability_marker("Answer.", &[peer("maple-house")]);
        let twice = append_unavailability_marker(&once, &[peer("maple-house")]);
        assert_eq!(once, twice);
        assert_eq!(
            twice.matches(MARKER_PREFIX).count(),
            1,
            "exactly one marker; got: {twice}"
        );
    }

    /// A refusal that came back empty still gets the marker — an empty answer
    /// plus a silent loss is the worst of both.
    #[test]
    fn an_empty_answer_becomes_the_marker() {
        let out = append_unavailability_marker("   ", &[peer("maple-house")]);
        assert!(out.starts_with(MARKER_PREFIX), "got: {out}");
    }

    /// A peer loss must NOT tell the user to rebuild — that is not their
    /// machine to fix. Guards the `user_remedy` match against collapsing back
    /// into one string.
    #[test]
    fn a_peer_loss_does_not_prescribe_a_rebuild() {
        assert!(!UnavailabilityReason::PeerUnreachable
            .user_remedy()
            .contains("Rebuild"));
        assert!(UnavailabilityReason::NotBuilt
            .user_remedy()
            .contains("Rebuild"));
    }
}
