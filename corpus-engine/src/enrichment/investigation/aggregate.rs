// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic post-coalesce aggregation for the investigation
//! pipeline.
//!
//! Some threshold patterns count edges per entity rather than reading
//! a numeric attribute the LLM emitted on an edge — e.g. "installations
//! with > N sightings". The model never emits a per-edge count, so we
//! compute it here, after coalescing, and stamp it as a numeric
//! attribute on the target entity where
//! [`detect_threshold`](super::patterns::detect_threshold)'s
//! entity-attribute scan can read it.

use std::collections::{HashMap, HashSet};

use super::graph::{InvestigationEntity, Relationship};

/// Count *distinct* `from_entity_id` per `to_entity_id` over edges of
/// `edge_type`, and stamp the count as `attribute` (a JSON number) on
/// each matching target entity.
///
/// "Distinct" so two edges from the same source (e.g. a sighting
/// mentioned in two chunks) don't double-count. Proximity is
/// approximated by reconciled-entity identity — geocoding / radius is
/// deferred (UFO.md open decision #5), so an edge counts toward a
/// target iff it points at that coalesced entity. This is exactly why
/// the entity coalescing in `extract.rs` is load-bearing: variant
/// surface forms must already have merged for the count to be right.
pub fn stamp_edge_counts(
    entities: &mut [InvestigationEntity],
    relationships: &[Relationship],
    edge_type: &str,
    attribute: &str,
) {
    let mut counts: HashMap<&str, HashSet<&str>> = HashMap::new();
    for r in relationships {
        if r.relationship_type == edge_type {
            counts
                .entry(r.to_entity_id.as_str())
                .or_default()
                .insert(r.from_entity_id.as_str());
        }
    }
    for ent in entities.iter_mut() {
        if let Some(sources) = counts.get(ent.id.as_str()) {
            ent.attributes.insert(
                attribute.to_string(),
                serde_json::Value::Number(serde_json::Number::from(sources.len() as u64)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::investigation::graph::{
        ExtractionExcerpt, InvestigationEntity, Relationship,
    };

    fn ent(id: &str, ty: &str) -> InvestigationEntity {
        InvestigationEntity {
            id: id.into(),
            canonical_name: id.into(),
            entity_type: ty.into(),
            attributes: Default::default(),
            aliases: Vec::new(),
        }
    }

    fn edge(id: &str, from: &str, to: &str, rtype: &str) -> Relationship {
        Relationship {
            id: id.into(),
            from_entity_id: from.into(),
            to_entity_id: to.into(),
            relationship_type: rtype.into(),
            attributes: Default::default(),
            evidence: ExtractionExcerpt {
                chunk_id: "c".into(),
                excerpt: "x".into(),
            },
            confidence: 1.0,
        }
    }

    #[test]
    fn stamps_distinct_source_count_on_target() {
        let mut entities = vec![ent("e-installation-wpafb", "installation")];
        let rels = vec![
            edge(
                "r0",
                "e-sighting-1",
                "e-installation-wpafb",
                "occurred_near",
            ),
            edge(
                "r1",
                "e-sighting-2",
                "e-installation-wpafb",
                "occurred_near",
            ),
            // duplicate source — must not double-count.
            edge(
                "r2",
                "e-sighting-2",
                "e-installation-wpafb",
                "occurred_near",
            ),
        ];
        stamp_edge_counts(&mut entities, &rels, "occurred_near", "sighting_count");
        assert_eq!(
            entities[0].attributes.get("sighting_count"),
            Some(&serde_json::json!(2))
        );
    }

    #[test]
    fn ignores_non_target_edge_types() {
        let mut entities = vec![ent("e-installation-wpafb", "installation")];
        let rels = vec![edge(
            "r0",
            "e-witness-a",
            "e-installation-wpafb",
            "investigated_at",
        )];
        stamp_edge_counts(&mut entities, &rels, "occurred_near", "sighting_count");
        assert!(entities[0].attributes.get("sighting_count").is_none());
    }
}
