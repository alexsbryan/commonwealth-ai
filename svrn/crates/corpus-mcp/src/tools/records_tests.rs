// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use understanding_vocab::ontology::decl::{AttrDecl, OntologyTypeDecl, TypeKind};

fn attr(name: &str, family: AttrFamily) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family,
        description: String::new(),
        derived: None,
        by: None,
    }
}

/// A company, a deal whose `counterparty` refs it, and a stage update about
/// the deal whose `party` refs the company: the shape the delivery journey's
/// question "which deals are with this company, at what stage" reads.
fn policies() -> OntologyPolicies {
    let mut p = OntologyPolicies::default();
    let text = || AttrFamily::Text { values: Vec::new() };
    for (name, kind, attributes) in [
        ("company", TypeKind::Entity, vec![attr("domain", text())]),
        (
            "deal",
            TypeKind::Entity,
            vec![
                attr(
                    "counterparty",
                    AttrFamily::Ref {
                        of: "company".into(),
                    },
                ),
                attr(
                    "amount",
                    AttrFamily::Quantity {
                        unit: Some("USD".into()),
                    },
                ),
                attr("stage", text()),
            ],
        ),
        (
            "stage_update",
            TypeKind::Claim,
            vec![
                attr("stage", text()),
                attr(
                    "party",
                    AttrFamily::Ref {
                        of: "company".into(),
                    },
                ),
            ],
        ),
    ] {
        p.shape.types.push(OntologyTypeDecl {
            name: name.into(),
            kind,
            attributes,
            ..Default::default()
        });
    }
    p
}

fn entity(id: &str, name: &str, of: &str, attributes: Value) -> Value {
    json!({"atom_type": "Entity", "data": {
        "id": id, "canonical_name": name, "entity_type": of,
        "first_appearance": {"chunk_id": "sec_00001"}, "description": "",
        "salience": 0.0, "enrichment_depth": "structural",
        "provenance": {"extractor_id": "document_fields", "source_doc_id": "m1", "signal_kind": "document_field"},
        "attributes": attributes}})
}

/// Writes the atlas: `atoms.json` always, `ontology.json` when `declared`.
fn atlas(declared: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let atlas = dir.path();
    let atoms = json!({"schema_version": "2.5", "atoms": [
        entity("entity-tep", "tucsonelectric.com", "company", json!({"domain": "tucsonelectric.com", "document_count": 3})),
        entity("entity-isda", "ISDA master agreement with TEP", "deal",
            json!({"counterparty": "entity-tep", "stage": "negotiating", "__document_read_fields": {}})),
        entity("entity-gone", "a deal whose company is not in this atlas", "deal",
            json!({"counterparty": "entity-elsewhere"})),
        {"atom_type": "Claim", "data": {
            "id": "claim-1", "content": "We in Legal will redraft the ISDA and send it to TEP.",
            "discourse_act": "assert", "epistemic_status": "attributed", "scope": "universal",
            "evidence": [{"chunk_id": "sec_00026"}], "subject": "entity-isda",
            "claim_kind": "stage_update",
            "enrichment_depth": "extracted",
            "attributes": {"stage": "negotiating", "party": "entity-tep"}}},
    ]});
    std::fs::write(
        atlas.join("atoms.json"),
        serde_json::to_vec(&atoms).unwrap(),
    )
    .unwrap();
    if declared {
        let file = AtlasOntologyFile {
            schema_version: AtlasOntologyFile::SCHEMA_VERSION.to_string(),
            ontology_version: 1,
            pipeline_id: "custom_atlas".into(),
            policies: policies(),
        };
        std::fs::write(
            atlas.join(AtlasOntologyFile::FILE),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();
    }
    dir
}

fn row<'a>(out: &'a ToolOutcome, id: &str) -> &'a Value {
    assert!(!out.is_error, "{}", out.text);
    out.structured.as_ref().unwrap()["atoms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no row {id}: {}", out.text))
}

/// The journey's MCP hop (delivery review 2026-10-12): looked up by name, a
/// company's row lists the deals whose `counterparty` is it and the stage
/// updates whose `party` is it; a deal's row holds its declared type, its
/// declared attributes (an absent one named unset, an internal key not
/// shown) and its counterparty as that company's record.
#[test]
fn a_record_shows_its_type_attributes_and_the_records_its_refs_join() {
    let dir = atlas(true);
    let company = lookup(
        "ward-story",
        dir.path(),
        &json!({"query": "tucsonelectric"}),
    );
    assert!(!company.is_error, "{}", company.text);
    let tep = row(&company, "entity-tep");
    assert_eq!(tep["type"], "company");
    assert_eq!(tep["attributes"], json!({"domain": "tucsonelectric.com"}));
    let from: Vec<(&str, &str)> = tep["referenced_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["attribute"].as_str().unwrap(), r["id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        from,
        [
            ("deal.counterparty", "entity-isda"),
            ("stage_update.party", "claim-1")
        ],
        "{}",
        company.text
    );
    assert!(
        company
            .text
            .contains("deal.counterparty ← ISDA master agreement with TEP (entity-isda)"),
        "{}",
        company.text
    );

    let deal = lookup(
        "ward-story",
        dir.path(),
        &json!({"query": "ISDA", "kind": "entity"}),
    );
    let isda = row(&deal, "entity-isda");
    assert_eq!(isda["type"], "deal");
    assert_eq!(
        isda["attributes"],
        json!({"counterparty": "entity-tep", "stage": "negotiating"})
    );
    assert_eq!(isda["unset"], json!(["amount"]));
    assert_eq!(
        isda["refs"],
        json!([{"attribute": "counterparty", "id": "entity-tep", "type": "company", "name": "tucsonelectric.com"}])
    );
    assert_eq!(
        isda["referenced_by"][0]["attribute"],
        "stage_update.subject"
    );
    assert!(
        deal.text.contains(
            "type deal · stage: negotiating · counterparty → tucsonelectric.com (entity-tep)"
        ),
        "{}",
        deal.text
    );
    assert_eq!(deal.structured.as_ref().unwrap()["refs_resolved"], true);
}

/// A ref to a record the atlas does not hold is shown absent, never dropped
/// and never named.
#[test]
fn a_ref_to_a_record_the_atlas_lacks_is_marked_absent() {
    let dir = atlas(true);
    let out = lookup(
        "ward-story",
        dir.path(),
        &json!({"query": "not in this atlas"}),
    );
    assert_eq!(
        row(&out, "entity-gone")["refs"],
        json!([{"attribute": "counterparty", "id": "entity-elsewhere", "absent": true}])
    );
    assert!(
        out.text
            .contains("counterparty → (not in this atlas) (entity-elsewhere)"),
        "{}",
        out.text
    );
}

/// With no recorded ontology there is no declaration to read refs by: the
/// attributes are shown as stored, only a claim's subject joins, and the
/// answer says why.
#[test]
fn with_no_ontology_refs_are_ids_and_the_answer_says_so() {
    let dir = atlas(false);
    let out = lookup(
        "ward-story",
        dir.path(),
        &json!({"query": "tucsonelectric"}),
    );
    let tep = row(&out, "entity-tep");
    assert_eq!(tep["attributes"]["document_count"], 3);
    assert_eq!(tep["referenced_by_total"], 0);
    assert_eq!(out.structured.as_ref().unwrap()["refs_resolved"], false);
    assert!(out.text.contains("no ontology recorded at"), "{}", out.text);
}
