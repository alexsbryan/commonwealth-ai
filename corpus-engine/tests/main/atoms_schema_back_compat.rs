// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atoms-file schema back-compatibility regression suite.
//!
//! Mirrors `recipe_back_compat.rs` for the atlas's `atoms.json`
//! schema (AtomsFile::SCHEMA_VERSION). The architecture-over-Enron
//! Phase 1 bumped this from 2.0 → 2.1 by adding the `Asset` variant
//! to `AtomEnvelope`. **Old atoms.json files must still deserialise.**
//!
//! Policy (ARCH §1 schema-back-compat discipline):
//! 1. Adding an envelope variant: pre-existing variants must
//!    round-trip unchanged. Old readers seeing a new variant fail
//!    loudly per the deliberate "no `#[serde(other)]`" choice.
//! 2. Adding a field to an existing variant: MUST carry
//!    `#[serde(default)]` so old atoms parse.
//!
//! Each fixture below pins a wire shape from a past schema version.

use corpus_engine::enrichment::atlas::atoms::{Asset, AtomEnvelope, AtomsFile};

// ---------------------------------------------------------------------------
// 2.0 atoms file — no Asset variant; should still deserialise.
// ---------------------------------------------------------------------------
const V2_0_ATOMS: &str = r#"{
  "schema_version": "2.0",
  "atoms": [
    {
      "atom_type": "Entity",
      "data": {
        "id": "entity-0001",
        "canonical_name": "Albert Einstein",
        "entity_type": "person",
        "first_appearance": { "chunk_id": "sec-001" },
        "description": "German-born theoretical physicist.",
        "salience": 0.92,
        "enrichment_depth": "extracted"
      }
    }
  ]
}"#;

#[test]
fn v2_0_atoms_file_round_trips_with_2_1_reader() {
    let file: AtomsFile = serde_json::from_str(V2_0_ATOMS).expect("parse v2.0 atoms");
    assert_eq!(file.atoms.len(), 1);
    match &file.atoms[0] {
        AtomEnvelope::Entity(e) => {
            assert_eq!(e.canonical_name, "Albert Einstein");
        }
        other => panic!("expected Entity, got {other:?}"),
    }
}

#[test]
fn v2_1_atoms_file_with_asset_variant_round_trips() {
    // The 2.1 schema introduced the Asset envelope variant + Attaches
    // edge. Confirm both wire-shape and Rust-side fields stay stable.
    let atom = Asset {
        id: corpus_engine::enrichment::atlas::atoms::AtomId::from_raw("asset-deadbeef00000001"),
        sha256: "deadbeef00000001ffffffffffffffffffffffffffffffffffffffffffffffff".into(),
        mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        original_filename: "q3_forecast.xlsx".into(),
        size: 24_576,
        asset_kind: "xlsx".into(),
        described_by: None,
        parsed_form: Some(std::path::PathBuf::from(
            "/tmp/assets/parsed/deadbeef00000001.parquet",
        )),
        first_seen_source_doc_id: "msg-12345".into(),
        enrichment_depth: corpus_engine::enrichment::pipeline::atlas::EnrichmentDepth::Extracted,
    };
    let file = AtomsFile::new(vec![AtomEnvelope::Asset(atom.clone())]);
    let json = serde_json::to_string(&file).expect("serialise 2.1 atoms");
    let back: AtomsFile = serde_json::from_str(&json).expect("re-parse 2.1 atoms");
    assert_eq!(back.schema_version, AtomsFile::SCHEMA_VERSION);
    match &back.atoms[0] {
        AtomEnvelope::Asset(a) => {
            assert_eq!(a.sha256, atom.sha256);
            assert_eq!(a.asset_kind, "xlsx");
            assert_eq!(a.parsed_form, atom.parsed_form);
            assert_eq!(a.first_seen_source_doc_id, "msg-12345");
        }
        other => panic!("expected Asset, got {other:?}"),
    }
}

#[test]
fn schema_version_constant_is_current() {
    // Phase 1 bumped from 2.0 → 2.1 (Asset variant). Phase 4 bumped
    // from 2.1 → 2.2 (Entity::provenance). The SF-LVT tabular track
    // bumped 2.2 → 2.3 (Entity::attributes). Ontology v1 P2 bumped
    // 2.3 → 2.4 (Relation/Event/Claim::attributes, Claim::subject). If a
    // later phase bumps further, update this assertion deliberately —
    // the test is the canary.
    assert_eq!(AtomsFile::SCHEMA_VERSION, "2.4");
}

#[test]
fn v2_1_entity_without_provenance_loads_with_default() {
    // Entities written by a 2.1 reader carry no `provenance` field;
    // the 2.2 reader must give them a default-constructed
    // `Provenance` so the schema-bump is back-compat.
    let toml_2_1_entity = r#"{
      "schema_version": "2.1",
      "atoms": [
        {
          "atom_type": "Entity",
          "data": {
            "id": "entity-001",
            "canonical_name": "Pre-2.2 Entity",
            "entity_type": "person",
            "first_appearance": { "chunk_id": "sec-001" },
            "description": "Authored under 2.1.",
            "salience": 0.5,
            "enrichment_depth": "extracted"
          }
        }
      ]
    }"#;
    let file: AtomsFile = serde_json::from_str(toml_2_1_entity).expect("parse 2.1 entity");
    match &file.atoms[0] {
        AtomEnvelope::Entity(e) => {
            assert!(e.provenance.extractor_id.is_empty());
            assert!(e.provenance.source_chunk_id.is_none());
        }
        other => panic!("expected Entity, got {other:?}"),
    }
}

#[test]
fn v2_2_entity_without_attributes_loads_with_default() {
    // Entities written by a ≤2.2 reader carry no `attributes` field;
    // the 2.3 reader must give them a default-constructed (empty) map
    // so the schema-bump is back-compat.
    let json_2_2_entity = r#"{
      "schema_version": "2.2",
      "atoms": [
        {
          "atom_type": "Entity",
          "data": {
            "id": "entity-001",
            "canonical_name": "Pre-2.3 Entity",
            "entity_type": "person",
            "first_appearance": { "chunk_id": "sec-001" },
            "description": "Authored under 2.2.",
            "salience": 0.5,
            "enrichment_depth": "extracted"
          }
        }
      ]
    }"#;
    let file: AtomsFile = serde_json::from_str(json_2_2_entity).expect("parse 2.2 entity");
    match &file.atoms[0] {
        AtomEnvelope::Entity(e) => {
            assert!(e.attributes.is_empty(), "absent attributes → empty map");
        }
        other => panic!("expected Entity, got {other:?}"),
    }
}

#[test]
fn v2_3_claim_relation_event_without_attributes_load_with_default() {
    // 2.4 added `attributes` to Relation / Event / Claim and `subject` to
    // Claim. Atoms written under 2.3 carry none of them and must load with
    // an empty map / `None`, everything else intact.
    let json_2_3 = r#"{
      "schema_version": "2.3",
      "atoms": [
        {
          "atom_type": "Relation",
          "data": {
            "id": "relation-0001",
            "label": "struck at",
            "participants": ["entity-0001", "entity-0002"],
            "relation_type": "association",
            "section_range": { "start": "sec-001", "end": "sec-001" },
            "enrichment_depth": "extracted"
          }
        },
        {
          "atom_type": "Event",
          "data": {
            "id": "event-0001",
            "description": "The mint opens.",
            "event_type": "decision",
            "section_position": { "section_id": "sec-001" },
            "enrichment_depth": "extracted"
          }
        },
        {
          "atom_type": "Claim",
          "data": {
            "id": "claim-0001",
            "content": "The coin weighs 1.29 g.",
            "discourse_act": "assert",
            "epistemic_status": "confident",
            "scope": "contextual",
            "attributed_to": "entity-0001",
            "enrichment_depth": "extracted"
          }
        }
      ]
    }"#;
    let file: AtomsFile = serde_json::from_str(json_2_3).expect("parse 2.3 atoms");
    assert_eq!(file.atoms.len(), 3);
    match &file.atoms[0] {
        AtomEnvelope::Relation(r) => {
            assert_eq!(r.label, "struck at");
            assert!(r.attributes.is_empty(), "absent attributes → empty map");
        }
        other => panic!("expected Relation, got {other:?}"),
    }
    match &file.atoms[1] {
        AtomEnvelope::Event(e) => {
            assert_eq!(e.description, "The mint opens.");
            assert!(e.attributes.is_empty(), "absent attributes → empty map");
        }
        other => panic!("expected Event, got {other:?}"),
    }
    match &file.atoms[2] {
        AtomEnvelope::Claim(c) => {
            assert_eq!(
                c.attributed_to.as_ref().map(|a| a.as_str()),
                Some("entity-0001")
            );
            assert!(c.subject.is_none(), "absent subject → None");
            assert!(c.attributes.is_empty(), "absent attributes → empty map");
        }
        other => panic!("expected Claim, got {other:?}"),
    }
}
