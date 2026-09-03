// SPDX-License-Identifier: AGPL-3.0-or-later
//! Axis 5's `patterns`, run over the atlas graph.
//!
//! The three graph-level detectors — circular flow, role overlap,
//! threshold — already exist and are already declared the same way
//! (`[[enrichment.patterns]]` / `[[enrichment.ontology.patterns]]`, both
//! `PatternDecl`). What they were missing is a graph: they were written for
//! the investigation pipeline's own `entities.json` /
//! `relationships.json`, and an atlas corpus has neither.
//!
//! So this module is an ADAPTER, not a second detector set (ARCH §19 — the
//! inventory outranks the plan): it projects `atoms.json` + `edges.json`
//! into the two shapes [`crate::enrichment::investigation::patterns::detect_all`]
//! already reads, and hands them over unchanged. There is one
//! implementation of "what a circular flow is", and it is still the one in
//! `investigation/patterns.rs`.
//!
//! ## What projects, and what does not
//!
//! - Every `Entity` atom becomes a `graph::Entity`. Its `entity_type` is
//!   the DECLARED type name where one was declared (the parser stores it as
//!   `EntityType::Other("coin")`), which is what a `PatternDecl`'s
//!   `entity_roles` names.
//! - A `Relation` atom with EXACTLY TWO participants becomes a
//!   `graph::Relationship` from the first to the second. The detectors are
//!   binary-edge algorithms — a cycle is over edges, a role overlap is a
//!   pair of edges between one pair of endpoints — so an n-ary relation has
//!   no unambiguous projection and is skipped rather than guessed at. The
//!   count of skipped relations is returned, never swallowed (ARCH §18.3).
//! - `Edge`s are NOT projected. They are the derived layer (Involves,
//!   Grounds, Tension); a declared pattern speaks about the author's own
//!   relation types, which are `Relation` ATOMS.

use crate::enrichment::atlas::atoms::{AtomEnvelope, AtomsFile};
use crate::enrichment::investigation::graph::{
    Entity, ExtractionExcerpt, PatternFinding, Relationship,
};

/// On-disk shape of `atlas/pattern_findings.json` — what the declared
/// `patterns` matched over the atlas graph.
///
/// Same envelope as [`super::gaps::GapsOutput`] (a version plus a list) so
/// the atlas directory reads uniformly and one writer contract covers both.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatternFindingsOutput {
    pub schema_version: String,
    pub findings: Vec<PatternFinding>,
    /// `Relation` atoms the projection could not map (see
    /// [`InvestigationGraph::non_binary_relations`]). On the file because a
    /// zero-finding run over a graph that lost half its edges is not the
    /// same outcome as a zero-finding run over a whole one.
    #[serde(default)]
    pub non_binary_relations: usize,
}

impl PatternFindingsOutput {
    pub const SCHEMA_VERSION: &'static str = "1.0";

    pub fn new(findings: Vec<PatternFinding>, non_binary_relations: usize) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            findings,
            non_binary_relations,
        }
    }
}

/// The projected graph, plus what could not be projected.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InvestigationGraph {
    pub entities: Vec<Entity>,
    pub relationships: Vec<Relationship>,
    /// `Relation` atoms whose participant count was not exactly two. Named
    /// so a corpus whose patterns find nothing can tell "no matches" from
    /// "the edges never reached the detector".
    pub non_binary_relations: usize,
}

/// Project an atlas into the investigation pipeline's entity/relationship
/// graph, so [`crate::enrichment::investigation::patterns::detect_all`] can
/// run over a declared ontology's own types.
///
/// Deterministic: atoms are visited in file order and ids are carried
/// through unchanged, so the same atlas yields the same findings.
pub fn to_investigation_graph(atoms: &AtomsFile) -> InvestigationGraph {
    let mut out = InvestigationGraph::default();
    for a in &atoms.atoms {
        match a {
            AtomEnvelope::Entity(e) => out.entities.push(Entity {
                id: e.id.as_str().to_string(),
                canonical_name: e.canonical_name.clone(),
                entity_type: e.entity_type.as_str_repr().to_string(),
                attributes: e.attributes.clone(),
                aliases: e.aliases.clone(),
            }),
            AtomEnvelope::Relation(r) => {
                let [from, to] = match r.participants.as_slice() {
                    [from, to] => [from, to],
                    _ => {
                        out.non_binary_relations += 1;
                        continue;
                    }
                };
                out.relationships.push(Relationship {
                    id: r.id.as_str().to_string(),
                    from_entity_id: from.as_str().to_string(),
                    to_entity_id: to.as_str().to_string(),
                    relationship_type: r.relation_type.as_str_repr().to_string(),
                    attributes: r.attributes.clone(),
                    evidence: ExtractionExcerpt {
                        chunk_id: r
                            .evidence
                            .first()
                            .map(|c| c.chunk_id.clone())
                            .unwrap_or_default(),
                        excerpt: r.label.clone(),
                    },
                    confidence: 1.0,
                });
            }
            _ => {}
        }
    }
    tracing::debug!(
        target: "atlas.patterns",
        entities = out.entities.len(),
        relationships = out.relationships.len(),
        non_binary_relations = out.non_binary_relations,
        "to_investigation_graph: atlas projected for the pattern detectors"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Relation, SectionRange};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, RelationType};

    fn entity(id: usize, name: &str, ty: &str) -> AtomEnvelope {
        AtomEnvelope::Entity(crate::enrichment::atlas::atoms::Entity {
            id: AtomId::entity(id),
            canonical_name: name.to_string(),
            aliases: Vec::new(),
            entity_type: EntityType::from_str_repr(ty),
            first_appearance: ChunkRef::new("sec-0001", None),
            description: String::new(),
            defining_quote: None,
            salience: 0.5,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: Default::default(),
            concept_kind: None,
        })
    }

    fn relation(id: usize, ty: &str, participants: Vec<AtomId>) -> AtomEnvelope {
        AtomEnvelope::Relation(Relation {
            id: AtomId::relation(id),
            label: "struck_at".to_string(),
            participants,
            relation_type: RelationType::from_str_repr(ty),
            evidence: vec![ChunkRef::new("sec-0002", None)],
            section_range: SectionRange::point("sec-0002"),
            attributes: Default::default(),
            enrichment_depth: EnrichmentDepth::Extracted,
        })
    }

    #[test]
    fn patterns_adapter_maps_two_participant_relations() {
        let atoms = AtomsFile {
            schema_version: AtomsFile::SCHEMA_VERSION.to_string(),
            atoms: vec![
                entity(1, "Aldfrith penny", "coin"),
                entity(2, "Eoforwic", "mint"),
                relation(1, "struck_at", vec![AtomId::entity(1), AtomId::entity(2)]),
                // Three participants: no unambiguous from/to, so skipped
                // and COUNTED.
                relation(
                    2,
                    "struck_at",
                    vec![AtomId::entity(1), AtomId::entity(2), AtomId::entity(1)],
                ),
                // One participant: same rule.
                relation(3, "struck_at", vec![AtomId::entity(1)]),
            ],
        };
        let g = to_investigation_graph(&atoms);

        assert_eq!(g.entities.len(), 2);
        assert_eq!(g.entities[0].entity_type, "coin", "the DECLARED type name");
        assert_eq!(g.entities[1].entity_type, "mint");

        assert_eq!(g.relationships.len(), 1, "only the binary relation maps");
        let r = &g.relationships[0];
        assert_eq!(r.from_entity_id, "entity-0001");
        assert_eq!(r.to_entity_id, "entity-0002");
        assert_eq!(r.relationship_type, "struck_at");
        assert_eq!(r.evidence.chunk_id, "sec-0002");

        assert_eq!(
            g.non_binary_relations, 2,
            "what could not be projected is reported, not swallowed"
        );
    }

    #[test]
    fn an_atlas_with_no_relations_projects_an_empty_edge_set() {
        let atoms = AtomsFile {
            schema_version: AtomsFile::SCHEMA_VERSION.to_string(),
            atoms: vec![entity(1, "Eoforwic", "mint")],
        };
        let g = to_investigation_graph(&atoms);
        assert_eq!(g.entities.len(), 1);
        assert!(g.relationships.is_empty());
        assert_eq!(g.non_binary_relations, 0);
    }
}
