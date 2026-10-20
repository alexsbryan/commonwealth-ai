// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed-query tests over a small purchasing ledger: two suppliers, purchase
//! orders that reference them, invoices that reference an order, and stock
//! counts. The S5 census (`research/ontology-apps/census/judgments.json`)
//! judged "how much we have ordered from each supplier this year"
//! (04-procurement n6) and "totals per employee, per project, per category
//! for a period" (11-expenses n4) inexpressible: no answer shape summed.
//! It judged "parts below their reorder level" (07-inventory n6) and "whether
//! the invoiced amount matches the order" (04-procurement n4) blocked: a
//! filter compared an attribute only with a literal.

use super::*;
use crate::atlas_traversal::classifier::QueryPlan;
use crate::atlas_traversal::engine::{traverse, AtlasView};
use crate::enrichment::atlas::atoms::{AtomId, ChunkRef, Claim, Entity};
use crate::enrichment::ontology::OntologyPolicies;
use crate::taxonomy::{ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus};
use understanding_vocab::ontology::decl::{
    AttrDecl, AttrFamily, Force, OntologyTypeDecl, OntologyV1, TypeKind,
};

fn attr(name: &str, family: AttrFamily) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family,
        description: String::new(),
        derived: None,
        by: None,
    }
}

fn claim_type(name: &str, attributes: Vec<AttrDecl>) -> OntologyTypeDecl {
    OntologyTypeDecl {
        name: name.into(),
        kind: TypeKind::Claim,
        force: Some(Force::Assertive),
        attributes,
        ..Default::default()
    }
}

fn policies() -> OntologyPolicies {
    let text = || AttrFamily::Text { values: vec![] };
    let money = || AttrFamily::Quantity { unit: None };
    OntologyV1 {
        types: vec![
            OntologyTypeDecl {
                name: "supplier".into(),
                kind: TypeKind::Entity,
                ..Default::default()
            },
            claim_type(
                "purchase_order",
                vec![
                    attr("reference", text()),
                    attr(
                        "supplier",
                        AttrFamily::Ref {
                            of: "supplier".into(),
                        },
                    ),
                    attr("order_total", money()),
                    attr("order_date", AttrFamily::Time { range: false }),
                ],
            ),
            claim_type(
                "invoice",
                vec![
                    attr(
                        "order",
                        AttrFamily::Ref {
                            of: "purchase_order".into(),
                        },
                    ),
                    attr("invoiced_amount", money()),
                ],
            ),
            claim_type(
                "stock_count",
                vec![
                    attr("counted", money()),
                    attr("reorder_level", money()),
                    attr("counted_on", AttrFamily::Time { range: false }),
                ],
            ),
        ],
        ..Default::default()
    }
    .into_policies()
}

fn supplier(idx: usize, name: &str) -> Entity {
    Entity {
        id: AtomId::entity(idx),
        canonical_name: name.into(),
        aliases: vec![],
        entity_type: EntityType::Other("supplier".into()),
        first_appearance: ChunkRef::new(format!("sec_e{idx}"), None),
        description: String::new(),
        salience: 0.5,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: Default::default(),
        concept_kind: None,
    }
}

fn claim(idx: usize, kind: &str, attrs: &[(&str, &str)]) -> Claim {
    Claim {
        attributes: attrs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect(),
        subject: None,
        id: AtomId::claim(idx),
        content: format!("{kind} {idx}"),
        discourse_act: DiscourseAct::Assert,
        epistemic_status: EpistemicStatus::Confident,
        scope: ClaimScope::Universal,
        evidence: vec![ChunkRef::new(format!("sec_c{idx}"), None)],
        attributed_to: None,
        confidence: Some(0.9),
        anchor: None,
        enrichment_depth: EnrichmentDepth::Extracted,
        quotable_excerpt: None,
        claim_kind: Some(kind.into()),
        concession_outcome: None,
        evidence_kind: None,
    }
}

/// Suppliers 1-2; purchase orders are claims 1-7, invoices 11-13, stock
/// counts 21-24.
fn ledger() -> (Vec<Entity>, Vec<Claim>) {
    let po = |idx, supplier: &str, total: Option<&str>, date: &str| {
        let mut attrs = vec![("supplier", supplier), ("order_date", date)];
        if let Some(t) = total {
            attrs.push(("order_total", t));
        }
        claim(idx, "purchase_order", &attrs)
    };
    let entities = vec![supplier(1, "Acme Ltd"), supplier(2, "Borealis GmbH")];
    let claims = vec![
        po(1, "entity-0001", Some("1200.50"), "2026-03-02"),
        po(2, "entity-0001", Some("300"), "2026-07-15"),
        po(3, "entity-0002", Some("99.5"), "2026-05-01"),
        // Ordered last year: outside the period.
        po(4, "entity-0001", Some("5000"), "2025-11-30"),
        // No total stated.
        po(5, "entity-0002", None, "2026-06-01"),
        // A thousands separator reads as two numbers, never as 1 or 250.
        po(6, "entity-0002", Some("1,250.00"), "2026-06-12"),
        // A supplier no record carries: the order still names it.
        po(7, "Cobalt Supplies", Some("75"), "2026-02-02"),
        // Invoiced more than its order's 1200.50.
        claim(
            11,
            "invoice",
            &[("order", "claim-0001"), ("invoiced_amount", "1300")],
        ),
        // Invoiced exactly its order's 300.
        claim(
            12,
            "invoice",
            &[("order", "claim-0002"), ("invoiced_amount", "300.00")],
        ),
        // Its order states no total: nothing to compare with.
        claim(
            13,
            "invoice",
            &[("order", "claim-0005"), ("invoiced_amount", "80")],
        ),
        claim(
            21,
            "stock_count",
            &[("counted", "3"), ("reorder_level", "5")],
        ),
        claim(
            22,
            "stock_count",
            &[("counted", "10"), ("reorder_level", "5")],
        ),
        // The sheet prints no reorder level.
        claim(23, "stock_count", &[("counted", "2")]),
        claim(
            24,
            "stock_count",
            &[("counted", "5"), ("reorder_level", "5")],
        ),
    ];
    (entities, claims)
}

/// Parse and run a query the way `svrn enrich atlas-query --typed` does:
/// serde into a [`TypedQuery`], then `traverse` its `QueryPlan::Typed`.
fn run(json: &str) -> TraversalResult {
    let vocab = policies();
    let (entities, claims) = ledger();
    let query: TypedQuery = serde_json::from_str(json).expect("query parses");
    traverse(
        &QueryPlan::Typed(query),
        AtlasView {
            entities: &entities,
            events: &[],
            states: &[],
            relations: &[],
            claims: &claims,
            questions: &[],
            configurations: &[],
            edges: &[],
            positions: &[],
            oppositions: &[],
            vocab: Some(&vocab),
        },
    )
}

/// "How much we have ordered from each supplier this year" (04-procurement
/// n6): a sum of `order_total` per referenced supplier record, over the
/// orders dated in 2026. An order with no total, or with a total that reads
/// as more than one number, is reported by id, never summed as zero or as
/// its first number.
#[test]
fn a_sum_groups_by_the_referenced_record_and_reports_what_it_cannot_add() {
    let r = run(r#"{"target_type":"purchase_order",
            "filters":[{"attribute":"order_date","op":"eq","value":2026,"negate":false}],
            "aggregate":"sum","aggregate_over":"order_total","group_by":"supplier"}"#);
    assert!(r.hit, "{}", r.headline);
    assert_eq!(r.kind, "aggregate");
    assert_eq!(
        r.headline,
        "6 purchase_order, order_total summed by supplier — Acme Ltd: 1500.5 (2), Borealis GmbH: 99.5 (1; 2 unjudged), Cobalt Supplies: 75 (1)"
    );
    let table = r.table.expect("a table");
    assert_eq!(table.matched, 6);
    assert_eq!(table.rows.len(), 6, "every summed atom is a cited row");
    assert!(
        table.notes.iter().any(|n| n
            == "2 matching purchase_order atom(s) carry no single readable order_total, so were not summed: claim-0005 (unset), claim-0006 (more than one number)."),
        "{:?}",
        table.notes
    );

    let all = run(
        r#"{"target_type":"purchase_order","aggregate":"sum","aggregate_over":"order_total","group_by":null}"#,
    );
    assert_eq!(
        all.headline,
        "7 purchase_order, order_total summed — 6675 (5; 2 unjudged)"
    );
}

/// Summing a non-quantity is refused at check time, as is a group the type
/// does not declare; `group_by` belongs to `sum` alone and does not parse
/// with another aggregate.
#[test]
fn a_sum_refuses_what_it_cannot_add() {
    for (q, why) in [
        (
            r#"{"target_type":"purchase_order","aggregate":"sum","aggregate_over":"reference"}"#,
            "'reference' is a text attribute; sum adds up a quantity attribute.",
        ),
        (
            r#"{"target_type":"purchase_order","aggregate":"sum","aggregate_over":"order_totl"}"#,
            "'order_totl' is not an attribute of purchase_order, so it cannot be summed.",
        ),
        (
            r#"{"target_type":"purchase_order","aggregate":"sum","aggregate_over":"order_total","group_by":"vendor"}"#,
            "'vendor' is not an attribute of purchase_order, so it cannot be grouped by.",
        ),
    ] {
        let r = run(q);
        assert!(!r.hit, "{q} should be refused");
        assert_eq!(r.headline, why, "{q}");
    }
    for bad in [
        r#"{"target_type":"purchase_order","aggregate":"sum"}"#,
        r#"{"target_type":"purchase_order","aggregate":"tally","aggregate_over":"supplier","group_by":"supplier"}"#,
    ] {
        assert!(
            serde_json::from_str::<TypedQuery>(bad).is_err(),
            "{bad} should not parse"
        );
    }
    let q = r#"{"target_type":"purchase_order","filters":[],"relations":[],"aggregate":"sum","aggregate_over":"order_total","group_by":"supplier"}"#;
    let parsed: TypedQuery = serde_json::from_str(q).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::from_str::<serde_json::Value>(q).unwrap(),
        "the sum round-trips"
    );
}

fn ids(r: &TraversalResult) -> Vec<String> {
    let mut ids: Vec<String> = r
        .table
        .as_ref()
        .expect("a typed query answers with a table")
        .rows
        .iter()
        .map(|row| row.atom_id.clone())
        .collect();
    ids.sort();
    ids
}

fn notes(r: &TraversalResult) -> Vec<String> {
    r.table
        .as_ref()
        .expect("a typed query answers with a table")
        .notes
        .clone()
}

/// "Parts below their reorder level" (07-inventory n6): a count's `counted`
/// compared with its own `reorder_level`. A count with no reorder level is
/// could-not-judge, set aside and said, never "not below".
#[test]
fn a_filter_compares_two_attributes_of_one_atom() {
    let below = |op: &str, negate: bool| {
        run(&format!(
            r#"{{"target_type":"stock_count","filters":[{{"attribute":"counted","op":"{op}","value":{{"attribute":"reorder_level"}},"negate":{negate}}}]}}"#
        ))
    };
    let r = below("lt", false);
    assert!(r.hit, "{}", r.headline);
    assert_eq!(ids(&r), ["claim-0021"]);
    assert!(
        notes(&r).contains(
            &"1 stock_count atom(s) set aside: reorder_level unset or unreadable.".to_string()
        ),
        "{:?}",
        notes(&r)
    );
    // At or below: not above.
    assert_eq!(ids(&below("gt", true)), ["claim-0021", "claim-0024"]);
    assert_eq!(ids(&below("eq", false)), ["claim-0024"]);
}

/// "Whether the invoiced amount matches the order" (04-procurement n4): inside
/// the relation's `where`, the order's `order_total` compared with the
/// invoice's own `invoiced_amount`. An order with no total is could-not-judge
/// for its invoice, not a mismatch and not a match.
#[test]
fn a_where_filter_compares_the_far_end_with_the_target() {
    let invoices = |negate: bool| {
        run(&format!(
            r#"{{"target_type":"invoice","relations":[{{"relation":"invoice.order","other_type":"purchase_order",
                "where":{{"filters":[{{"attribute":"order_total","op":"eq","value":{{"target":"invoiced_amount"}},"negate":{negate}}}]}}}}]}}"#
        ))
    };
    let differs = invoices(true);
    assert!(differs.hit, "{}", differs.headline);
    assert_eq!(ids(&differs), ["claim-0011"]);
    let unjudged =
        "1 purchase_order atom(s) set aside: order_total unset or unreadable.".to_string();
    assert!(notes(&differs).contains(&unjudged), "{:?}", notes(&differs));
    let matches = invoices(false);
    assert_eq!(ids(&matches), ["claim-0012"]);
    assert!(notes(&matches).contains(&unjudged), "{:?}", notes(&matches));
}

/// An operand naming an attribute the vocabulary does not hold is refused,
/// as is one of another kind, `contains` against an attribute, and an
/// operand spelled any other way.
#[test]
fn an_attribute_operand_is_checked_against_the_vocabulary() {
    for (q, why) in [
        (
            r#"{"target_type":"stock_count","filters":[{"attribute":"counted","op":"lt","value":{"attribute":"reorder_lvl"}}]}"#,
            "'reorder_lvl' is not an attribute of stock_count, so a filter cannot compare with it.",
        ),
        (
            r#"{"target_type":"invoice","relations":[{"relation":"invoice.order","other_type":"purchase_order","where":{"filters":[{"attribute":"order_total","op":"eq","value":{"target":"amount"}}]}}]}"#,
            "'amount' is not an attribute of invoice, so a filter cannot compare with it.",
        ),
        (
            r#"{"target_type":"stock_count","filters":[{"attribute":"counted","op":"lt","value":{"attribute":"counted_on"}}]}"#,
            "'counted' (quantity) cannot be compared with 'counted_on' (time).",
        ),
        (
            r#"{"target_type":"purchase_order","filters":[{"attribute":"reference","op":"contains","value":{"attribute":"reference"}}]}"#,
            "'reference' cannot be compared by Contains with an attribute; contains takes a value.",
        ),
        (
            r#"{"target_type":"purchase_order","filters":[{"attribute":"order_total","op":"eq","value":{"attribute":"supplier"}}]}"#,
            "'supplier' is a ref; a filter compares with a text, time or quantity attribute.",
        ),
    ] {
        let r = run(q);
        assert!(!r.hit, "{q} should be refused");
        assert_eq!(r.headline, why, "{q}");
    }
    for bad in [
        r#"{"target_type":"stock_count","filters":[{"attribute":"counted","op":"lt","value":{"near":"reorder_level"}}]}"#,
        r#"{"target_type":"stock_count","filters":[{"attribute":"counted","op":"lt","value":{"attribute":"reorder_level","target":"counted"}}]}"#,
    ] {
        assert!(
            serde_json::from_str::<TypedQuery>(bad).is_err(),
            "{bad} should not parse"
        );
    }
    let q = r#"{"target_type":"stock_count","filters":[{"attribute":"counted","op":"lt","value":{"attribute":"reorder_level"},"negate":false}],"relations":[],"aggregate":"none","aggregate_over":null}"#;
    let parsed: TypedQuery = serde_json::from_str(q).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::from_str::<serde_json::Value>(q).unwrap(),
        "an attribute operand round-trips"
    );
}
