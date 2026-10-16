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

/// A company known by its domain, an order whose `client` refs it, and a
/// statement about the order.
fn policies() -> OntologyPolicies {
    let mut p = OntologyPolicies::default();
    p.shape.types.push(OntologyTypeDecl {
        name: "company".into(),
        kind: TypeKind::Entity,
        attributes: vec![attr("web_domain", AttrFamily::Text { values: Vec::new() })],
        identity: vec!["web_domain".into()],
        ..Default::default()
    });
    p.shape.types.push(OntologyTypeDecl {
        name: "order".into(),
        kind: TypeKind::Entity,
        attributes: vec![
            attr(
                "client",
                AttrFamily::Ref {
                    of: "company".into(),
                },
            ),
            attr(
                "price",
                AttrFamily::Quantity {
                    unit: Some("USD".into()),
                },
            ),
        ],
        ..Default::default()
    });
    p.shape.types.push(OntologyTypeDecl {
        name: "order_update".into(),
        kind: TypeKind::Claim,
        ..Default::default()
    });
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

fn atoms() -> Vec<AtomEnvelope> {
    let reading = |value: Value, claim: &str, evidence: &str| json!({"value": value, "claim": claim, "document": "m", "evidence": evidence});
    serde_json::from_value(json!([
        entity("entity-birch", "birchhall.example", "company", json!({"web_domain": "birchhall.example"})),
        entity("entity-order", "an order of six chairs", "order", json!({
            "client": "entity-birch",
            "__folds": {
                "client": {"outcome": "decided", "values": ["entity-birch"], "documents": ["m"],
                    "readings": [reading(json!("entity-birch"), "claim-1", "Birch Hall Trust")]},
                "price": {"outcome": "conflict", "values": ["2400", "2250"],
                    "readings": [reading(json!(2400.0), "claim-1", "2,400 dollars"),
                                 reading(json!(2250.0), "claim-2", "2,250 dollars")]}
            }
        })),
        {"atom_type": "Claim", "data": {
            "id": "claim-1", "content": "please go ahead with the six oak chairs at 2,400 dollars",
            "discourse_act": "assert", "epistemic_status": "attributed", "scope": "universal",
            "evidence": [{"chunk_id": "sec_00002"}], "subject": "entity-order",
            "claim_kind": "order_update", "enrichment_depth": "extracted",
            "attributes": {"document_date": "2026-03-05T16:05:00Z",
                "__resolve_link": {"decided_by": "chosen", "choice_score": 0.9, "none_score": 0.05}}}},
        {"atom_type": "Claim", "data": {
            "id": "claim-2", "content": "the six oak chairs at 2,250 dollars",
            "discourse_act": "assert", "epistemic_status": "attributed", "scope": "universal",
            "evidence": [{"chunk_id": "sec_00003"}], "subject": "entity-order",
            "claim_kind": "order_update", "enrichment_depth": "extracted",
            "attributes": {"document_date": "2026-03-06T18:20:00Z"}}},
    ]))
    .unwrap()
}

/// An order's row: its client as the company's record with the domain it is
/// known by; its price, which its statements disagree on, no value but a
/// conflict whose readings are each cited and dated by their statements; and
/// its client's decided value traced to the words that named the company.
#[test]
fn a_row_shows_refs_by_key_and_every_fold_with_its_cited_readings() {
    let atoms = atoms();
    let p = policies();
    let view = RecordView::new(&atoms, Some(&p));
    let order = view.row(view.atom("entity-order").unwrap());
    let client = order["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["attribute"] == "client")
        .unwrap();
    assert_eq!(client["key"], "birchhall.example", "{order}");
    assert!(order["attributes"].get("price").is_none(), "{order}");
    assert_eq!(order["unset"], json!(["price"]), "{order}");
    let price = &order["folds"]["price"];
    assert_eq!(price["outcome"], "conflict", "{order}");
    let dates: Vec<&Value> = price["readings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["date"])
        .collect();
    assert_eq!(
        dates,
        [
            &json!("2026-03-05T16:05:00Z"),
            &json!("2026-03-06T18:20:00Z")
        ],
        "each reading is dated by its statement: {price}"
    );
    let named = &order["folds"]["client"]["readings"][0];
    assert_eq!(named["names"]["key"], "birchhall.example", "{named}");
    assert_eq!(named["evidence"], "Birch Hall Trust");
    assert_eq!(
        fold_line("price", price),
        "price: conflict \"2400\" | \"2250\" (2 reading(s))"
    );
}

/// A statement's row says how its subject was decided, and the company's row
/// lists the order whose client it is.
#[test]
fn a_statement_row_carries_its_link_and_a_record_its_inbound_refs() {
    let atoms = atoms();
    let p = policies();
    let view = RecordView::new(&atoms, Some(&p));
    let claim = view.row(view.atom("claim-1").unwrap());
    assert_eq!(claim["link"]["decided_by"], "chosen", "{claim}");
    assert_eq!(claim["refs"][0]["attribute"], "subject", "{claim}");
    let company = view.row(view.atom("entity-birch").unwrap());
    assert!(
        company["referenced_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["attribute"] == "order.client" && r["id"] == "entity-order"),
        "{company}"
    );
    assert!(company.get("folds").is_none(), "{company}");
}
