// SPDX-License-Identifier: AGPL-3.0-or-later
//! Step 1 of the walk: WHICH ROW, from whose map, on what evidence.
//!
//! Split out of `ground.rs` by order ei-5c. It is the one decision made
//! before any seeding happens, it is the only part of the walk that touches
//! the embedder for anything but the question, and a caller that already
//! knows the question kind skips it entirely ([`WalkSelection::named`]) — so
//! it is separable from the BFS in fact as well as on paper (ARCH §3.1).
//!
//! Re-exported wholesale from [`super`], so `atlas::ground::select_walk` and
//! `atlas::ground::WalkSelection` still resolve.

use crate::atlas_traversal::question_kind::{shared_classifier, KindScore, KindSource};
use crate::types::EmbedFn;
use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind, WalkPolicy};

use super::super::provider::AtlasProvider;
use super::PolicySource;

/// Which navigation map governs this walk.
///
/// The first graph in scope that DECLARED one wins; otherwise the
/// pre-registered table. A mixed-corpus query is the ambiguous case the spec
/// left open, and it is resolved by declaration-beats-default rather than by
/// merging two maps into a third that neither corpus wrote.
///
/// Note `AtlasGraph::ontology()` is `Some` only for a corpus that declared
/// TYPES (`with_ontology` drops the rest), so a built-in pipeline's atlas
/// reaches the pre-registered table here even when its `ontology.json` is on
/// disk — which is correct, because those files carry no `navigation`
/// override either.
pub fn navigation_policy_for(graphs: &[&dyn AtlasProvider]) -> (NavigationPolicy, PolicySource) {
    for g in graphs {
        if let Some(p) = g.ontology() {
            return (
                p.navigation.clone(),
                PolicySource::Declared(g.atlas_corpus_id().to_string()),
            );
        }
    }
    (NavigationPolicy::default(), PolicySource::PreRegistered)
}

/// One decision about HOW to walk, taken once: which row, from whose map,
/// on what evidence.
///
/// Bundled rather than passed as five arguments because they are one
/// decision and a caller must not be able to pair a `Thematic` kind with the
/// `Tension` row, or report a declared policy source for the defaults.
#[derive(Debug, Clone)]
pub struct WalkSelection {
    pub kind: QuestionKind,
    pub walk: WalkPolicy,
    pub kind_source: KindSource,
    /// The classifier's raw scores, when one ran — what makes a borderline
    /// abstain reviewable instead of mysterious.
    pub kind_score: Option<KindScore>,
    pub policy_source: PolicySource,
}

impl WalkSelection {
    /// The row a caller that ALREADY knows the kind wants — an `ask` argument,
    /// a test. Skips the embedder entirely and records that a caller, not a
    /// centroid, decided.
    pub fn named(kind: QuestionKind, policy: &NavigationPolicy, source: PolicySource) -> Self {
        Self {
            kind,
            walk: policy.walk(kind).clone(),
            kind_source: KindSource::Caller,
            kind_score: None,
            policy_source: source,
        }
    }

    /// The unfiltered row under a named reason — the one place
    /// "unclassified" is turned into a walk.
    pub(crate) fn unfiltered(
        kind_source: KindSource,
        kind_score: Option<KindScore>,
        source: PolicySource,
    ) -> Self {
        Self {
            kind: QuestionKind::Thematic,
            walk: WalkPolicy::unfiltered(),
            kind_source,
            kind_score,
            policy_source: source,
        }
    }

    /// One line naming the row and why it was chosen — for `ask`'s result
    /// text and for the log.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} ({}, {}): {} hops, budget {}",
            self.kind.as_str(),
            self.kind_source.as_str(),
            self.policy_source.label(),
            self.walk.hops,
            self.walk.budget
        );
        if let Some(k) = self.kind_score {
            s.push_str(&format!(" [sim {:.3}, margin {:.3}]", k.sim, k.margin));
        }
        s
    }
}

/// Classify the question and read its row.
///
/// Separated from [`ground`] so a caller that already KNOWS the kind uses
/// [`WalkSelection::named`] and skips the embedder, and so the classification
/// decision has one home.
pub async fn select_walk(
    question_embedding: &[f32],
    policy: &NavigationPolicy,
    policy_source: PolicySource,
    embed: Option<&EmbedFn>,
) -> WalkSelection {
    let Some(embed) = embed else {
        return WalkSelection::unfiltered(KindSource::ClassifierUnavailable, None, policy_source);
    };
    let Some(classifier) = shared_classifier(policy, embed).await else {
        // Two reasons, distinguished: the map declared no exemplars at all,
        // or the embedder failed. `classifiable()` answers which.
        let src = if policy.classifiable().is_empty() {
            KindSource::NoClassifier
        } else {
            KindSource::ClassifierUnavailable
        };
        return WalkSelection::unfiltered(src, None, policy_source);
    };
    match classifier.classify(question_embedding) {
        (Some(kind), score) => WalkSelection {
            kind,
            walk: policy.walk(kind).clone(),
            kind_source: KindSource::Classified,
            kind_score: score,
            policy_source,
        },
        (None, score) => WalkSelection::unfiltered(KindSource::Abstained, score, policy_source),
    }
}
