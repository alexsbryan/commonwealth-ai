// SPDX-License-Identifier: AGPL-3.0-or-later
//! Step 1 of the walk: WHICH ROW, from whose map, on what evidence.
//!
//! Split out of `ground.rs` by order ei-5c. It is the one decision made
//! before any seeding happens, it is the only part of the walk that touches
//! the embedder for anything but the question, and a caller that already
//! knows the question kind skips it entirely ([`WalkSelection::named`]) — so
//! it is separable from the BFS in fact as well as on paper (ARCH §3.1).
//!
//! Since order epistemic-index-map-conversion (2026-09-08) the decision has a
//! second half: a row the classifier picks is checked against what the
//! atlases in scope CARRY ([`AtlasInventory`]) before it runs. A row that
//! cannot fire — no seed kind, no edge kind — falls to the next admissible
//! kind in race order, or to the unfiltered row, and the selection says
//! which and why ([`RowInertReport`]). Measured before this existed: the
//! tension row classified on wikipedia and walked with zero seeds, because
//! wikipedia carries no Claim and no Position. The fall-through is BY NAME
//! (the order's third done-when), never a silent substitution (§18.3).
//!
//! Re-exported wholesale from [`super`], so `atlas::ground::select_walk` and
//! `atlas::ground::WalkSelection` still resolve.

use crate::atlas_traversal::question_kind::{shared_classifier, KindScore, KindSource};
use crate::types::EmbedFn;
use corpus_engine_vocab::ontology::{NavigationPolicy, QuestionKind, WalkPolicy};

use super::super::inventory::{AtlasInventory, RowFit, RowInert};
use super::super::provider::{AtlasProvider, NavigationSource};
use super::PolicySource;

/// Which navigation map governs this walk.
///
/// The first graph in scope whose `ontology.json` carries rows wins
/// ([`NavigationSource::Declared`], types or no types); failing that, the
/// first graph a loader attached a pipeline's map to
/// ([`NavigationSource::PipelineDefault`], map-conversion rung 3); failing
/// that, the pre-registered table. A mixed-corpus query is the ambiguous case
/// the spec left open, and it is resolved by declaration-beats-default rather
/// than by merging two maps into a third that neither corpus wrote.
///
/// Until rung 3 this read `ontology()`, which is `Some` only for a corpus
/// that declared TYPES — so engineering's typeless map could never reach the
/// walk, and an installed SEP atlas with no file at all had no way to its
/// pipeline's rows short of a rebuild. Both come through
/// [`AtlasProvider::navigation`] now.
pub fn navigation_policy_for(graphs: &[&dyn AtlasProvider]) -> (NavigationPolicy, PolicySource) {
    let mut pipeline_default: Option<(NavigationPolicy, PolicySource)> = None;
    for g in graphs {
        match g.navigation() {
            Some(NavigationSource::Declared(p)) => {
                return (
                    p.clone(),
                    PolicySource::Declared(g.atlas_corpus_id().to_string()),
                );
            }
            Some(NavigationSource::PipelineDefault { pipeline, policy }) => {
                if pipeline_default.is_none() {
                    pipeline_default = Some((
                        policy.clone(),
                        PolicySource::PipelineDefault {
                            atlas: g.atlas_corpus_id().to_string(),
                            pipeline: pipeline.to_string(),
                        },
                    ));
                }
            }
            None => {}
        }
    }
    pipeline_default.unwrap_or((NavigationPolicy::default(), PolicySource::PreRegistered))
}

/// A classified row that could not fire, and what ran instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowInertReport {
    /// The kind the classifier picked.
    pub winner: QuestionKind,
    /// What the atlases in scope do not carry for that row.
    pub why: RowInert,
    /// The next admissible kind in race order, or `None` when the walk fell
    /// to the unfiltered row.
    pub fell_to: Option<QuestionKind>,
}

impl RowInertReport {
    /// One sentence for a result body or a log line.
    pub fn sentence(&self) -> String {
        let ran = match self.fell_to {
            Some(k) => format!("the {} row", k.as_str()),
            None => "the unfiltered row".to_string(),
        };
        format!(
            "the {} row is inert on the atlases in scope ({}); the walk ran {ran}",
            self.winner.as_str(),
            self.why.clause()
        )
    }
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
    /// `Some` exactly when `kind_source` is [`KindSource::RowInert`]: the
    /// row the classifier picked, why it could not fire, and what ran.
    pub inert: Option<RowInertReport>,
}

impl WalkSelection {
    /// The row a caller that ALREADY knows the kind wants — an `ask` argument,
    /// a test. Skips the embedder entirely and records that a caller, not a
    /// centroid, decided. Not checked against the inventory: the caller
    /// named the row, and the walk's `seed_kinds_unseen` reports what it
    /// then failed to find.
    pub fn named(kind: QuestionKind, policy: &NavigationPolicy, source: PolicySource) -> Self {
        Self {
            kind,
            walk: policy.walk(kind).clone(),
            kind_source: KindSource::Caller,
            kind_score: None,
            policy_source: source,
            inert: None,
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
            inert: None,
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
        if let Some(i) = &self.inert {
            s.push_str(" — ");
            s.push_str(&i.sentence());
        }
        s
    }
}

/// What the admissibility check decided for a classified winner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The winner's row fits; run it.
    Fits,
    /// The winner's row is inert; run `fell_to`'s row, or the unfiltered
    /// row when `fell_to` is `None`.
    Inert(RowInertReport),
}

/// Check a classified winner against the inventory and, when its row is
/// inert, pick what runs instead: the next kind in race order whose row
/// fits AND whose similarity clears the floor.
///
/// The ONE decider (§10.6): [`select_walk`] applies it to a live walk and
/// `svrn atlas kind` applies it to a bank against a corpus's `_summary.json`
/// census, so the instrument and the walk cannot disagree.
///
/// The floor on the fall-through is the classifier's own `min_sim`, not a
/// second threshold. The margin gate does not apply — the runner-up lost it
/// by definition — but the floor keeps its meaning: a kind below it is near
/// nothing in particular, and walking it because the winner was refused
/// would be a guess. Below the floor, the unfiltered row runs, which is the
/// status quo ante and composes every source.
pub fn admit_winner(
    winner: QuestionKind,
    race: &[(QuestionKind, f32)],
    min_sim: f32,
    policy: &NavigationPolicy,
    inventory: &AtlasInventory,
) -> Admission {
    let why = match inventory.fit(policy.walk(winner)) {
        RowFit::Fits => return Admission::Fits,
        RowFit::Inert(why) => why,
    };
    let fell_to = race
        .iter()
        .filter(|(k, sim)| *k != winner && *sim >= min_sim)
        .find(|(k, _)| inventory.fit(policy.walk(*k)).fits())
        .map(|(k, _)| *k);
    Admission::Inert(RowInertReport {
        winner,
        why,
        fell_to,
    })
}

/// Classify the question, check the row against what the atlases carry, and
/// read the row that runs.
///
/// Separated from [`ground`] so a caller that already KNOWS the kind uses
/// [`WalkSelection::named`] and skips the embedder, and so the classification
/// decision has one home. `inventory` is [`AtlasInventory::of`] the same
/// graphs the walk will run over.
pub async fn select_walk(
    question_embedding: &[f32],
    policy: &NavigationPolicy,
    policy_source: PolicySource,
    inventory: &AtlasInventory,
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
    let (kind, score) = match classifier.classify(question_embedding) {
        (Some(kind), score) => (kind, score),
        (None, score) => {
            return WalkSelection::unfiltered(KindSource::Abstained, score, policy_source)
        }
    };
    let race = classifier.race(question_embedding).unwrap_or_default();
    let (min_sim, _) = classifier.gates();
    match admit_winner(kind, &race, min_sim, policy, inventory) {
        Admission::Fits => WalkSelection {
            kind,
            walk: policy.walk(kind).clone(),
            kind_source: KindSource::Classified,
            kind_score: score,
            policy_source,
            inert: None,
        },
        Admission::Inert(report) => {
            tracing::debug!(
                target: "retrieval_audit",
                winner = kind.as_str(),
                fell_to = report.fell_to.map(|k| k.as_str()).unwrap_or("unfiltered"),
                why = %report.why.clause(),
                "question-kind: the classified row is inert on the atlases in scope"
            );
            let mut sel = match report.fell_to {
                Some(k) => WalkSelection {
                    kind: k,
                    walk: policy.walk(k).clone(),
                    kind_source: KindSource::RowInert,
                    kind_score: score,
                    policy_source,
                    inert: None,
                },
                None => WalkSelection::unfiltered(KindSource::RowInert, score, policy_source),
            };
            sel.inert = Some(report);
            sel
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::AtomType;
    use crate::enrichment::atlas::edges::EdgeType;
    use std::collections::BTreeMap;

    /// Wikipedia's census: Entities typed `article`, Involves edges.
    fn wikipedia() -> AtlasInventory {
        AtlasInventory {
            atoms: BTreeMap::from([(AtomType::Entity, 100)]),
            entity_types: BTreeMap::from([("article".to_string(), 100)]),
            edges: BTreeMap::from([(EdgeType::Involves, 400)]),
            declares_types: false,
        }
    }

    /// THE FAILING INPUT, as a decision: tension wins the race on wikipedia.
    /// The row is inert (no Claim, no Position), and the walk falls to the
    /// next kind in race order whose row fits — lookup, past a thematic that
    /// is itself inert there — and says so by name.
    #[test]
    fn an_inert_winner_falls_to_the_next_admissible_kind_in_race_order() {
        let race = vec![
            (QuestionKind::Tension, 0.52),
            (QuestionKind::Thematic, 0.44),
            (QuestionKind::Lookup, 0.40),
            (QuestionKind::Trajectory, 0.38),
        ];
        let a = admit_winner(
            QuestionKind::Tension,
            &race,
            0.34,
            &NavigationPolicy::default(),
            &wikipedia(),
        );
        let Admission::Inert(r) = a else {
            panic!("tension must be inert on wikipedia");
        };
        assert_eq!(r.winner, QuestionKind::Tension);
        assert_eq!(r.fell_to, Some(QuestionKind::Lookup));
        assert_eq!(
            r.why.seeds_missing,
            vec![AtomType::Claim, AtomType::Position]
        );
        assert_eq!(
            r.sentence(),
            "the tension row is inert on the atlases in scope (no claim or position atoms; \
             no tension or opposes_in edges); the walk ran the lookup row"
        );
    }

    /// The fall-through respects the floor: an admissible kind below
    /// `min_sim` is not walked, and the unfiltered row runs instead. The
    /// margin is NOT re-applied — lookup here is 0.02 behind thematic and
    /// still runs when it is above the floor.
    #[test]
    fn the_fall_through_stops_at_the_floor() {
        let policy = NavigationPolicy::default();
        let race = vec![
            (QuestionKind::Tension, 0.52),
            (QuestionKind::Thematic, 0.36),
            (QuestionKind::Lookup, 0.34),
        ];
        let Admission::Inert(at_floor) =
            admit_winner(QuestionKind::Tension, &race, 0.34, &policy, &wikipedia())
        else {
            panic!()
        };
        assert_eq!(at_floor.fell_to, Some(QuestionKind::Lookup));

        let race = vec![
            (QuestionKind::Tension, 0.52),
            (QuestionKind::Thematic, 0.36),
            (QuestionKind::Lookup, 0.33),
        ];
        let Admission::Inert(below) =
            admit_winner(QuestionKind::Tension, &race, 0.34, &policy, &wikipedia())
        else {
            panic!()
        };
        assert_eq!(below.fell_to, None);
        assert!(below
            .sentence()
            .ends_with("the walk ran the unfiltered row"));
    }

    /// A winner whose row fits is admitted untouched — the check must not
    /// cost an admitted row anything.
    #[test]
    fn a_fitting_winner_is_admitted() {
        let a = admit_winner(
            QuestionKind::Lookup,
            &[(QuestionKind::Lookup, 0.5)],
            0.34,
            &NavigationPolicy::default(),
            &wikipedia(),
        );
        assert_eq!(a, Admission::Fits);
    }
}
