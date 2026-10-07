// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AtlasGraph::typed_answer` answers what the typed executor answers.
//!
//! Chat holds an `AtlasGraph` (projected records over `atoms.lance`), the CLI
//! (`svrn enrich atlas-query --typed`) holds the atoms themselves. The graph
//! re-parses entities, claims and relations from the records' payloads, so a
//! field the store's projection drops would be a field the chat answer cannot
//! see while the CLI's can. This writes a real store, loads it, and requires
//! the two tables to be identical.

use corpus_engine::atlas_traversal::engine::AtlasView;
use corpus_engine::atlas_traversal::typed::{self, TypedQuery};
use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef, Entity, Relation, SectionRange};
use corpus_engine::enrichment::atlas::context::AtlasGraph;
use corpus_engine::enrichment::atlas::{store, write_atlas_ontology, AtomEnvelope};
use corpus_engine::enrichment::ontology::OntologyPolicies;
use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, RelationType};
use understanding_vocab::ontology::decl::{
    AttrDecl, AttrFamily, OntologyTypeDecl, OntologyV1, TypeKind,
};

fn policies() -> OntologyPolicies {
    OntologyV1 {
        types: vec![
            OntologyTypeDecl {
                name: "hoard".into(),
                kind: TypeKind::Entity,
                attributes: vec![AttrDecl {
                    name: "findspot".into(),
                    family: AttrFamily::Text { values: vec![] },
                    description: String::new(),
                    derived: None,
                }],
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "mint".into(),
                kind: TypeKind::Entity,
                ..Default::default()
            },
            OntologyTypeDecl {
                name: "holds_coins_of".into(),
                kind: TypeKind::Relation,
                from: Some("hoard".into()),
                to: Some("mint".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
    .into_policies()
}

fn ent(idx: usize, name: &str, ty: &str, findspot: Option<&str>) -> Entity {
    let mut attributes = serde_json::Map::new();
    if let Some(f) = findspot {
        attributes.insert("findspot".into(), serde_json::Value::String(f.into()));
    }
    Entity {
        id: AtomId::entity(idx),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Other(ty.into()),
        first_appearance: ChunkRef::new(format!("sec_e{idx}"), None),
        description: format!("{name}, a {ty}."),
        defining_quote: None,
        salience: 0.9,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes,
        concept_kind: None,
    }
}

fn holds(idx: usize, hoard: usize, mint: usize) -> Relation {
    Relation {
        attributes: Default::default(),
        id: AtomId::relation(idx),
        label: "holds_coins_of".into(),
        participants: vec![AtomId::entity(hoard), AtomId::entity(mint)],
        relation_type: RelationType::Other("holds_coins_of".into()),
        evidence: vec![ChunkRef::new(format!("sec_r{idx}"), None)],
        section_range: SectionRange::point("sec_0001"),
        enrichment_depth: EnrichmentDepth::Extracted,
    }
}

fn query(json: &str) -> TypedQuery {
    serde_json::from_str(json).expect("typed query parses")
}

#[test]
fn the_graph_answers_a_typed_query_as_the_executor_does() {
    let entities = vec![
        ent(1, "Demanhur hoard", "hoard", Some("Egypt")),
        ent(2, "Kuft hoard", "hoard", Some("Kuft, Egypt")),
        ent(11, "Babylon", "mint", None),
        ent(12, "Sidon", "mint", None),
        ent(13, "Tyre", "mint", None),
    ];
    let relations = vec![holds(1, 1, 11), holds(2, 1, 12), holds(3, 2, 13)];
    let p = policies();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let atoms: Vec<AtomEnvelope> = entities
        .iter()
        .cloned()
        .map(AtomEnvelope::Entity)
        .chain(relations.iter().cloned().map(AtomEnvelope::Relation))
        .collect();
    store::write_store_blocking(dir, "typed-graph", &atoms, &[]).unwrap();
    write_atlas_ontology(dir, "custom_atlas", 1, &p).unwrap();
    let graph = AtlasGraph::load_lance_from_disk("typed-graph", dir, Default::default()).unwrap();

    for q in [
        // Members of one named hoard, from the relation's far end.
        r#"{"target_type": "mint", "filters": [], "relations": [{"relation": "holds_coins_of",
            "other_type": "hoard", "other_name": "Demanhur hoard", "negate": false, "where": null}],
            "aggregate": "none", "aggregate_over": null}"#,
        // A filter on an attribute, and a count.
        r#"{"target_type": "hoard", "filters": [{"attribute": "findspot", "op": "contains",
            "value": "Kuft", "negate": false}], "relations": [], "aggregate": "count",
            "aggregate_over": null}"#,
    ] {
        let q = query(q);
        let from_graph = graph.typed_answer(&q).expect("a declared corpus answers");
        let from_slices = typed::execute(
            &q,
            AtlasView {
                entities: &entities,
                events: &[],
                states: &[],
                relations: &relations,
                claims: &[],
                questions: &[],
                configurations: &[],
                edges: &[],
                positions: &[],
                oppositions: &[],
                vocab: Some(&p),
            },
        );
        assert_eq!(
            serde_json::to_value(&from_graph).unwrap(),
            serde_json::to_value(&from_slices).unwrap(),
            "graph and slices disagree on {q:?}"
        );
    }

    let members = graph
        .typed_answer(&query(
            r#"{"target_type": "mint", "filters": [], "relations": [{"relation": "holds_coins_of",
                "other_type": "hoard", "other_name": "Demanhur hoard", "negate": false, "where": null}],
                "aggregate": "none", "aggregate_over": null}"#,
        ))
        .unwrap()
        .table
        .expect("a typed answer is a table");
    let names: Vec<&str> = members.rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names.len(), 2, "Babylon and Sidon, not Tyre: {names:?}");
    assert!(names.contains(&"Babylon") && names.contains(&"Sidon"));
    let babylon = members.rows.iter().find(|r| r.name == "Babylon").unwrap();
    assert!(
        babylon.evidence.iter().any(|c| c == "sec_r1"),
        "the row cites the relation that put it in the answer: {:?}",
        babylon.evidence
    );
}

#[test]
fn an_undeclared_graph_has_no_typed_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let atoms = vec![AtomEnvelope::Entity(ent(11, "Babylon", "mint", None))];
    store::write_store_blocking(tmp.path(), "plain", &atoms, &[]).unwrap();
    let graph = AtlasGraph::load_lance_from_disk("plain", tmp.path(), Default::default()).unwrap();
    let q = query(
        r#"{"target_type": "mint", "filters": [], "relations": [], "aggregate": "none",
            "aggregate_over": null}"#,
    );
    assert!(graph.typed_answer(&q).is_none());
}
