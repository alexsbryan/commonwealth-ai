// SPDX-License-Identifier: AGPL-3.0-or-later
//! `resolve_typed_extension_section` and `apply_citations_to_resolved`.
//!
//! Sibling of `tests_trajectory.rs`; see `tests.rs` for why these are four files.

use super::tests_fixtures::*;
use super::*;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntitySketch, EntityType, EventSketch};
use std::sync::Arc;

// ── resolve_typed_extension_section + apply_citations_to_resolved ──

use crate::enrichment::pipeline::atlas::{
    ArgumentativeExtension, MechanismSketch, OppositionSketch, PositionSketch, TypeExtension,
};

fn argumentative_with_mechanism_and_opposition() -> TypeExtension {
    TypeExtension::Argumentative(ArgumentativeExtension {
        positions: vec![PositionSketch {
            name: "rent concentration thesis".into(),
            content: "Deepest rents pool at uncopyable chokepoints.".into(),
            proponent: "".into(),
            stance: "endorse".into(),
            anchor: "rent concentration".into(),
        }],
        mechanisms: vec![MechanismSketch {
            name: "spread pricing".into(),
            description: "PBMs charge payers more than they reimburse.".into(),
            domain: "economics".into(),
            anchor: "spread pricing".into(),
        }],
        evidence_invocations: vec![],
        oppositions: vec![OppositionSketch {
            left: "markets".into(),
            right: "regulation".into(),
            axis: "governance".into(),
            framing: "".into(),
            anchor: "markets vs regulation".into(),
        }],
        concessions: vec![],
    })
}

#[test]
fn resolve_typed_extension_section_wraps_and_projects() {
    let resolved = resolve_typed_extension_section(
        argumentative_with_mechanism_and_opposition(),
        "chunk:42".into(),
        EnrichmentDepth::Extracted,
        NextIdxBundle::default(),
    );
    assert_eq!(
        resolved.new_entities.len(),
        1,
        "mechanism projects to one Concept Entity atom"
    );
    assert_eq!(resolved.new_positions.len(), 1);
    assert_eq!(resolved.new_oppositions.len(), 1);
    // ChunkRefs all carry the section_id this helper threaded
    // through — the chunk_id is exactly what the caller provided.
    assert_eq!(
        resolved.new_entities[0].first_appearance.chunk_id,
        "chunk:42"
    );
    assert_eq!(
        resolved.new_positions[0].first_appearance.chunk_id,
        "chunk:42"
    );
    assert_eq!(
        resolved.new_oppositions[0].first_appearance.chunk_id,
        "chunk:42"
    );
}

#[test]
fn apply_citations_to_resolved_populates_previews_across_collections() {
    let mut resolved = resolve_typed_extension_section(
        argumentative_with_mechanism_and_opposition(),
        "chunk:7".into(),
        EnrichmentDepth::Extracted,
        NextIdxBundle::default(),
    );
    let mut citations = std::collections::HashMap::new();
    citations.insert(
        "chunk:7".into(),
        super::super::citation::SourceCitation {
            section_id: "chunk:7".into(),
            passage_preview: Some("Verbatim source sentence about spread pricing.".into()),
        },
    );

    apply_citations_to_resolved(&mut resolved, &citations);

    assert_eq!(
        resolved.new_entities[0]
            .first_appearance
            .passage_preview
            .as_deref(),
        Some("Verbatim source sentence about spread pricing.")
    );
    assert_eq!(
        resolved.new_positions[0]
            .first_appearance
            .passage_preview
            .as_deref(),
        Some("Verbatim source sentence about spread pricing.")
    );
    assert_eq!(
        resolved.new_oppositions[0]
            .first_appearance
            .passage_preview
            .as_deref(),
        Some("Verbatim source sentence about spread pricing.")
    );
}

#[test]
fn apply_citations_to_resolved_is_noop_without_matching_section_id() {
    let mut resolved = resolve_typed_extension_section(
        argumentative_with_mechanism_and_opposition(),
        "chunk:7".into(),
        EnrichmentDepth::Extracted,
        NextIdxBundle::default(),
    );
    // Citations map keyed on a DIFFERENT section_id — no preview
    // should land on any atom.
    let mut citations = std::collections::HashMap::new();
    citations.insert(
        "chunk:999".into(),
        super::super::citation::SourceCitation {
            section_id: "chunk:999".into(),
            passage_preview: Some("Should not appear anywhere.".into()),
        },
    );

    apply_citations_to_resolved(&mut resolved, &citations);

    for ent in &resolved.new_entities {
        assert!(ent.first_appearance.passage_preview.is_none());
    }
    for pos in &resolved.new_positions {
        assert!(pos.first_appearance.passage_preview.is_none());
    }
}
