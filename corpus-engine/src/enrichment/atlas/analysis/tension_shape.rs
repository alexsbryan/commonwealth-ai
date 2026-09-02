// SPDX-License-Identifier: AGPL-3.0-or-later
//! Axis 5's derived selector — the corpus shape a genre picks a Phase-6
//! candidate strategy from.
//!
//! The selector is DERIVED, not declared: `ONTOLOGY_PRIMITIVES.md` §2 axis
//! 5 lists it among the seven facets demoted from declared to inferable,
//! because a user who has told you what their claims are about has already
//! told you enough. [`CorpusShape::of`] measures the two numbers,
//! `AtlasGenre::derive_tension_strategy` answers with a strategy, and the
//! build step PRINTS the derivation — a derived choice nobody can see is
//! not glassbox (ARCH §9.1).
//!
//! Sibling of [`super::tension_policy`], which holds what the DECLARATION
//! contributes to the same phase.

use super::super::atoms::Claim;
use super::tensions::TensionStrategy;

/// The two numbers a genre reads to derive its Phase-6 selector.
///
/// `doc_count` is the number of distinct **units of separate authorship**
/// the claims cite: distinct `source_doc_id` where the atlas knows it,
/// falling back to distinct `chunk_id` (the section) where it does not. The
/// fallback is the point — a one-file catalogue with a section per
/// catalogue entry is cross-*document* in every way the selector cares
/// about, and counting files would call it a single document and hand it
/// the within-document selector.
///
/// `attributed_ratio` is the fraction of claims carrying an
/// `attributed_to`, which is the key the graph selector's entity-overlap
/// signal groups on (`select_entity_overlap_claim_claim`). A corpus whose
/// claims mostly lack one gives that signal nothing to work with.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CorpusShape {
    /// Claims in scope after `tension.between`.
    pub claims: usize,
    /// Distinct documents (or sections, see above) the claims cite.
    pub doc_count: usize,
    /// Fraction of claims carrying an `attributed_to`, in `[0, 1]`.
    pub attributed_ratio: f32,
}

impl CorpusShape {
    /// Measure the shape of a claim set. Pure; the caller has already
    /// applied `tension.between`, so this sees exactly the pool the
    /// selector will run over.
    pub fn of(claims: &[Claim]) -> Self {
        let mut docs: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let mut attributed = 0usize;
        for c in claims {
            if c.attributed_to.is_some() {
                attributed += 1;
            }
            for e in &c.evidence {
                docs.insert(e.source_doc_id.as_deref().unwrap_or(e.chunk_id.as_str()));
            }
        }
        Self {
            claims: claims.len(),
            doc_count: docs.len(),
            attributed_ratio: if claims.is_empty() {
                0.0
            } else {
                attributed as f32 / claims.len() as f32
            },
        }
    }

    /// One line naming the shape, for the operator's view of why a
    /// selector was chosen (ARCH §9.1 — a branch of production code with
    /// no tracing event is a smell, and a derived choice with no printed
    /// derivation is the same smell one level up).
    pub fn describe(&self) -> String {
        format!(
            "{} claim(s) across {} document(s)/section(s), {:.0}% attributed",
            self.claims,
            self.doc_count,
            self.attributed_ratio * 100.0
        )
    }
}

/// Derive the Phase-6 selector for a corpus that DECLARES an ontology.
///
/// Cross-document, sparsely-attributed material gets the embedding top-K
/// net; a single-unit corpus whose claims are densely attributed gets the
/// graph signals, which have something to group on there and cost no model
/// call.
///
/// **Why the disjunction, and not the conjunction the P4 plan wrote.** The
/// plan reads "embedding top-K when `doc_count >= 2 && attributed_ratio <
/// 0.5`, graph otherwise". Taken literally that sends the governance
/// template — cross-document rules densely attributed to their topics — to
/// the graph selector, off the `EmbeddingTopK { k: 10, floor: 0.5 }` its
/// recall/precision bar was measured at
/// ([`super::tensions::TensionStrategy`] carries the measurement), and the
/// P4 order pre-registers that bar. It also sends a one-file catalogue with
/// a section per entry to a selector that groups claim pairs by
/// `attributed_to` — the wrong key for a declared corpus, whose
/// comparability key is `subject` and whose two attributions of one coin
/// are by *different* scholars, so entity-overlap would never pair them.
/// Graph is therefore the narrow case, not the default: a corpus has to be
/// BOTH single-unit AND densely attributed to earn it. Flipping either of
/// the two measured corpora off its measured selector on an unmeasured
/// rule is what ARCH §18.5 and §18.6 forbid.
pub fn derive_declared_strategy(shape: &CorpusShape, default: TensionStrategy) -> TensionStrategy {
    if shape.doc_count < 2 && shape.attributed_ratio >= 0.5 {
        TensionStrategy::Graph
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef};
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EpistemicStatus,
    };

    fn claim(id: usize, kind: Option<&str>) -> Claim {
        Claim {
            id: AtomId::claim(id),
            content: format!("claim {id}"),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: Vec::new(),
            quotable_excerpt: None,
            attributed_to: None,
            subject: None,
            attributes: Default::default(),
            confidence: None,
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            claim_kind: kind.map(str::to_string),
            concession_outcome: None,
            evidence_kind: None,
        }
    }

    #[test]
    fn corpus_shape_counts_sections_when_no_source_doc_id() {
        let mut a = claim(1, Some("attribution"));
        a.attributed_to = Some(AtomId::entity(7));
        a.evidence = vec![ChunkRef::new("sec-0001", None)];
        let mut b = claim(2, Some("attribution"));
        b.evidence = vec![ChunkRef::new("sec-0002", None)];
        let shape = CorpusShape::of(&[a, b]);
        assert_eq!(shape.claims, 2);
        assert_eq!(shape.doc_count, 2, "sections stand in for documents");
        assert!((shape.attributed_ratio - 0.5).abs() < f32::EPSILON);
        assert!(shape.describe().contains("2 document(s)/section(s)"));
    }

    #[test]
    fn derived_strategy_keeps_the_measured_default_for_cross_document_corpora() {
        let default = TensionStrategy::EmbeddingTopK { k: 10, floor: 0.5 };
        // The governance shape: many sections, densely attributed to topics.
        let cross_doc = CorpusShape {
            claims: 40,
            doc_count: 12,
            attributed_ratio: 0.95,
        };
        assert_eq!(derive_declared_strategy(&cross_doc, default), default);
        // The numismatic shape, as MEASURED on the real wessex-hoard atlas
        // 2026-09-02: 48 claims across 20 sections, 31% attributed.
        let catalogue = CorpusShape {
            claims: 48,
            doc_count: 20,
            attributed_ratio: 0.31,
        };
        assert_eq!(derive_declared_strategy(&catalogue, default), default);
    }

    #[test]
    fn derived_strategy_picks_graph_only_for_a_single_densely_attributed_unit() {
        let default = TensionStrategy::EmbeddingTopK { k: 10, floor: 0.5 };
        let one_unit = CorpusShape {
            claims: 9,
            doc_count: 1,
            attributed_ratio: 0.9,
        };
        assert_eq!(
            derive_declared_strategy(&one_unit, default),
            TensionStrategy::Graph
        );
        // One unit but sparsely attributed: the graph signal has nothing
        // to group on, so the net stays.
        let one_unit_sparse = CorpusShape {
            claims: 9,
            doc_count: 1,
            attributed_ratio: 0.1,
        };
        assert_eq!(derive_declared_strategy(&one_unit_sparse, default), default);
    }
}
