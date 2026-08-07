// SPDX-License-Identifier: AGPL-3.0-or-later
//! The embedding instruction the router's classifier stack encodes under.
//!
//! ## Why this module exists (2026-08-04)
//!
//! The intent axis owned 3 of 40 calibration cases — 7.5% coverage —
//! and `router fit --objective max-coverage --min-precision 0.85` found
//! NOTHING better across 1640 candidate gates. It was never an operating-
//! point problem. The geometry said why: leave-one-out 1-NN **on the
//! exemplar bank itself** was 60.6%, margin carried NEGATIVE information
//! (mean margin when correct was 0.0029 *below* mean margin when wrong),
//! and between-class scatter was 12.3% of total variance. The scorer
//! could not classify its own hand-authored training data.
//!
//! The cause was the instruction, not the gate and not the scorer.
//! Exemplars and queries were embedded through
//! [`InferenceProvider::embed_query`], which applies Qwen3-Embedding's
//! shipped retrieval instruction (`model_family.rs`, "Given a search
//! query, retrieve relevant passages that answer the query"). That asks
//! an instruction-FOLLOWING model to encode *what passages would answer
//! this* — i.e. TOPIC. We then used that vector to classify SPEECH ACT.
//! Topic and speech act are near-orthogonal; the model was doing exactly
//! what it was told.
//!
//! ## Why THIS instruction
//!
//! Chosen from 8 candidates by `intent_instruction_probe` on the PRODUCT
//! metric — coverage at a precision floor, with the gate fitted on one
//! calibration bank and evaluated on the other. Cross-bank, at a 90%
//! precision floor:
//!
//! | instruction         | fit axes → eval holdout | fit holdout → eval axes |
//! |---------------------|-------------------------|-------------------------|
//! | shipped retrieval   | cov  0% · prec 100%     | cov  9% · prec  50%     |
//! | none (unprefixed)   | cov  6% · prec 100%     | cov 37% · prec  65%     |
//! | **speech-act**      | **cov 41% · prec 88%**  | **cov 49% · prec 88%**  |
//!
//! Ranking accuracy moves with it: 67% → 91% on `axes_v1`, 53% → 75% on
//! the holdout.
//!
//! **Do not re-select this instruction on a proxy.** LOO accuracy,
//! scatter ratio, margin separation and ranking accuracy ALL mis-rank the
//! candidates: `act-enumerated` won LOO (76.5%), scatter (0.531) and
//! holdout ranking (42/55) while delivering only 16-24% coverage — the
//! thing the axis is actually for. Re-run the probe and read the
//! cross-bank coverage row.
//!
//! ## Why `embed`, not `embed_query`
//!
//! [`InferenceProvider::embed_query`] means "apply the model's own QUERY
//! instruction" — that is retrieval's, it is calibrated on it, and
//! `model_family.rs` must not be changed to suit the router. We want the
//! un-instructed surface so we can supply our own instruction, which is
//! exactly what [`InferenceProvider::embed`] is on the one embed family
//! this workspace ships (`document_instruction` is empty). That
//! assumption is load-bearing — a family that sets a document
//! instruction would silently DOUBLE-prefix and put every classifier in
//! a fourth vector space — so it is pinned by
//! `embed_is_the_uninstructed_surface` below rather than left as a
//! comment.
//!
//! This also reproduces the probe's own call shape exactly: it embedded
//! `format!("{instruction}{text}")` through `embed_batch`. The numbers
//! above describe this code path and no other.
//!
//! ## Scope — which axes encode here
//!
//! The intent, locator, scope and archive axes share ONE query embedding
//! per turn (`router.rs`), so they move together or not at all. The
//! effort and current-info classifiers each pay their own embed call and
//! are therefore free to differ; see `DEFAULT_*` notes on each. Anything
//! embedding under this instruction must be calibrated under it —
//! thresholds fitted in retrieval space are meaningless here.

use crate::traits::InferenceProvider;
use crate::Result;

/// Which embedding space an axis is calibrated in.
///
/// A closed set, so it is an enum rather than a string passed between
/// surfaces (ARCH_PRINCIPLES §2.1). Thresholds are only meaningful
/// within one space: a gate fitted in [`Self::RetrievalQuery`] and
/// applied to [`Self::Classifier`] vectors is not conservative or
/// aggressive, it is nonsense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedSpace {
    /// [`CLASSIFIER_INSTRUCTION`] — the router's own instruction.
    Classifier,
    /// The embed model's query instruction, via `embed_query`. This is
    /// retrieval's space; corpus search is calibrated on it.
    RetrievalQuery,
    /// Bare `embed`, no instruction.
    Unprefixed,
}

impl EmbedSpace {
    /// The router-embed cache key discriminator for this space.
    pub const fn cache_method(self) -> &'static str {
        match self {
            Self::Classifier => "c",
            Self::RetrievalQuery => "q",
            Self::Unprefixed => "d",
        }
    }
}

/// THE axis → embedding-space map. Every surface that embeds on behalf
/// of a router classifier resolves the space here: the classifiers
/// themselves, the cache freshness gate's exemplar specs, and
/// `router fit`. One decider, so a calibration run cannot measure a
/// different space than the runtime serves (ARCH_PRINCIPLES §10.6).
///
/// `None` for an unrecognised axis — callers must skip and say so, never
/// fall back to a default space. Silently picking one produces numbers
/// from the wrong vector space, which is the single easiest way to make
/// a calibration run lie (§18.3).
///
/// ## The dividing line: what the axis discriminates BY
///
/// The split below is not per-axis taste. A speech-act instruction tells
/// the model to encode what the speaker is DOING and to discard what they
/// are talking ABOUT. So an axis whose classes differ by speech act
/// belongs here, and an axis whose classes differ by SUBJECT MATTER
/// cannot live here at all — the instruction deletes the very signal it
/// needs.
///
/// That was learned the expensive way on 2026-08-04, by moving all four
/// shared-vector axes at once and measuring. `archive` separates "my past
/// conversations" from "this thread" from "the world" — three subjects,
/// one speech act — and in the classifier space its negatives land INSIDE
/// its positive band on both terms: "What did Kant say about duty?"
/// scores (0.926, +0.024) against "Have I mentioned kayaking in any of
/// our past chats?" at (0.929, +0.031). No floor/margin pair separates
/// them; the axis is not merely mis-tuned there, it is inexpressible.
/// Full table: `tests/archive_axis_live.rs --ignored`.
///
/// The assignments, and the measurement behind each:
///
/// * `intent` — [`EmbedSpace::Classifier`]. The speech-act axis proper.
///   Cross-bank at a 90% precision floor, gate fitted on one calibration
///   bank and evaluated on the other: 0-9% coverage → 41-49% at 88%
///   precision.
/// * `locator` — [`EmbedSpace::Classifier`]. Scored one-vs-rest over the
///   SAME exemplar bank as `intent`, so it has no independent choice; it
///   also validates on a real negative set (5/8 positives, **0/14 false
///   positives**, correctly abstaining on the adversarial archive-recall
///   negatives — `tests/locator_axis_live.rs --ignored`).
/// * `archive` — [`EmbedSpace::RetrievalQuery`]. Measured above: cannot
///   be gated in the classifier space. Stays where its 5/6-positive,
///   0/20-false-positive calibration was done.
/// * `scope` — [`EmbedSpace::RetrievalQuery`]. Personal-vs-external is a
///   subject-matter distinction, the same shape as `archive`, and unlike
///   `intent` it has no hold-out bank and no live negative set — 10
///   calibration cases in total. Its LOO 1-NN rose under the classifier
///   instruction (97.5% → 100.0%), but LOO is the proxy this same probe
///   showed mis-ranks candidates against the product metric. Not moved on
///   that evidence.
/// * `current_info` — [`EmbedSpace::RetrievalQuery`], unchanged.
///   Time-sensitivity is likewise topical, and it measured WORSE under
///   the classifier instruction (93.5% → 89.1%).
/// * `effort` — [`EmbedSpace::Unprefixed`], unchanged. It measured 87.5%
///   → 93.8% under the classifier instruction, but again that is LOO.
///   Moving it needs a coverage-at-precision number from
///   `router fit --axis effort`, not a proxy.
pub fn axis_space(axis: &str) -> Option<EmbedSpace> {
    Some(match axis {
        "intent" | "locator" => EmbedSpace::Classifier,
        "scope" | "archive" | "current_info" => EmbedSpace::RetrievalQuery,
        "effort" => EmbedSpace::Unprefixed,
        _ => return None,
    })
}

/// The classifier stack's embedding instruction.
///
/// Changing this text invalidates every calibrated threshold on every
/// axis that encodes under it, AND every entry in the `c:` space of the
/// router-embed cache. The cache invalidation is structural — the key
/// hash folds this constant in (`router_embed_cache::key`) — so a change
/// here fails the freshness gate rather than silently serving vectors
/// from the old space. The thresholds are NOT structural: re-run
/// `sovereign router fit` and the `intent_instruction_probe` before
/// trusting any number measured across a change to this string.
pub const CLASSIFIER_INSTRUCTION: &str = "Instruct: Classify the speech act of the user's message — what the speaker is DOING with these words, not what they are about\nMessage: ";

/// The exact string handed to the embed model for `text`. One decider
/// for the concatenation: the cache key, the freshness gate, the
/// classifiers and the calibration harness all build their input here,
/// so none of them can drift into a different vector space.
pub fn classifier_input(text: &str) -> String {
    format!("{CLASSIFIER_INSTRUCTION}{text}")
}

/// Embed `text` in the classifier space. Every router classifier that
/// encodes under [`CLASSIFIER_INSTRUCTION`] calls this — never
/// `embed_query` (retrieval's instruction) and never bare `embed`
/// (unprefixed, a third space).
///
/// Not normalised: callers normalise with `router_axis::normalize`,
/// matching the pre-existing contract of every classifier's embed path.
pub async fn embed_classifier(inference: &dyn InferenceProvider, text: &str) -> Result<Vec<f32>> {
    inference.embed(&classifier_input(text)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_family::ModelFamily;

    #[test]
    fn instruction_is_applied_as_a_prefix() {
        let input = classifier_input("what did I decide about the router?");
        assert!(input.starts_with(CLASSIFIER_INSTRUCTION));
        assert!(input.ends_with("what did I decide about the router?"));
    }

    /// `locator` is scored one-vs-rest over the SAME exemplar bank as
    /// `intent`, so it cannot sit in a different space — a split would
    /// score one of them against vectors it was never calibrated on,
    /// with no error anywhere. Pinned so a future edit fails loudly.
    #[test]
    fn intent_and_locator_share_one_space() {
        assert_eq!(axis_space("intent"), Some(EmbedSpace::Classifier));
        assert_eq!(
            axis_space("locator"),
            axis_space("intent"),
            "locator scores against the intent exemplar bank; they share a space"
        );
    }

    /// The subject-matter axes must NOT be in the classifier space: a
    /// speech-act instruction discards exactly the signal they classify
    /// by. `archive` was measured there and its world negatives landed
    /// inside its positive band — see `axis_space` docs. This is the
    /// tripwire against re-unifying them for tidiness.
    #[test]
    fn subject_matter_axes_stay_out_of_the_classifier_space() {
        for axis in ["scope", "archive", "current_info"] {
            assert_eq!(
                axis_space(axis),
                Some(EmbedSpace::RetrievalQuery),
                "{axis} discriminates by subject matter, which the classifier \
                 instruction is designed to discard"
            );
        }
        assert_eq!(axis_space("effort"), Some(EmbedSpace::Unprefixed));
    }

    #[test]
    fn unknown_axis_has_no_space_rather_than_a_default() {
        assert_eq!(
            axis_space("intnet"),
            None,
            "a typo must not resolve to a space"
        );
        assert_eq!(axis_space(""), None);
    }

    #[test]
    fn cache_methods_are_distinct_per_space() {
        let methods = [
            EmbedSpace::Classifier.cache_method(),
            EmbedSpace::RetrievalQuery.cache_method(),
            EmbedSpace::Unprefixed.cache_method(),
        ];
        let unique: std::collections::HashSet<_> = methods.iter().collect();
        assert_eq!(
            unique.len(),
            3,
            "each space needs its own cache key namespace"
        );
    }

    /// `embed_classifier` supplies the instruction itself, so `embed`
    /// MUST be the un-instructed surface. If a future embed family sets
    /// a `document_instruction`, every classifier silently moves to a
    /// double-prefixed vector space while every calibrated threshold
    /// stays put — a green build over a broken router. Fail here
    /// instead, at the assumption, with the repair named.
    #[test]
    fn embed_is_the_uninstructed_surface() {
        let quirks = ModelFamily::Qwen3Embedding.default_quirks();
        let eq = quirks.embed.expect("Qwen3Embedding must have EmbedQuirks");
        assert_eq!(
            eq.document_instruction, "",
            "the classifier stack prefixes CLASSIFIER_INSTRUCTION itself and embeds via \
             `embed`; a non-empty document_instruction would double-prefix it. Either \
             clear it, or give `embed_classifier` a provider surface that bypasses it — \
             and recalibrate every axis, because the vector space changed."
        );
    }
}
