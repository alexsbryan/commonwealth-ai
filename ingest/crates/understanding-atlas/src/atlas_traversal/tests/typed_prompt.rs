// SPDX-License-Identifier: AGPL-3.0-or-later
//! The port holds the probe's bytes. `data/` was written by
//! `research/ontology-retrieval/ontology-proof/ans/k2_query.py`'s own
//! `documentation()` and `schema()` over ft-ans-dev-b's `atlas/ontology.json`,
//! with each attribute gloss moved into the attribute's `description`
//! (`ans_policies.json`) — the one departure the module names.

use super::*;

fn ans() -> OntologyPolicies {
    serde_json::from_str(include_str!("data/ans_policies.json")).expect("ans policies parse")
}

#[test]
fn the_documentation_is_the_probes_bytes() {
    let got = query_grammar(&ans()).documentation;
    let want = include_str!("data/ans_query_documentation.txt");
    assert_eq!(got, want);
}

#[test]
fn the_schema_is_the_probes_schema_in_its_order() {
    let got = query_grammar(&ans()).schema;
    let want: Value =
        serde_json::from_str(include_str!("data/ans_query_schema.json")).expect("schema parse");
    // Compared as text: the map compares order-blind, and property order is
    // what the sampler generates in.
    assert_eq!(
        serde_json::to_string(&got).unwrap(),
        serde_json::to_string(&want).unwrap()
    );
}

#[test]
fn a_list_query_in_the_schemas_shape_parses_as_a_typed_query() {
    let schema = query_grammar(&ans()).schema;
    let mint = schema["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["properties"]["target_type"]["const"] == "mint")
        .expect("a mint branch");
    let reaches_hoard = mint["properties"]["relations"]["items"]["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| {
            r["properties"]["relation"]["const"] == "holds_coins_of"
                && r["properties"]["other_type"]["const"] == "hoard"
        });
    assert!(reaches_hoard, "holds_coins_of is usable from its `to` end");
    let q: super::super::typed::TypedQuery = serde_json::from_str(
        r#"{"target_type": "mint", "filters": [], "relations": [{"relation": "holds_coins_of",
            "other_type": "hoard", "other_name": "Demanhur hoard", "negate": false, "where": null}],
            "aggregate": "none", "aggregate_over": null}"#,
    )
    .expect("the executor's parser takes what the schema admits");
    assert_eq!(q.target_type, "mint");
}

#[test]
fn a_type_nothing_reaches_admits_no_relations() {
    let mut p = ans();
    p.shape.types.retain(|t| t.name == "mint");
    let schema = query_grammar(&p).schema;
    assert_eq!(
        schema["anyOf"][0]["properties"]["relations"],
        serde_json::json!({"type": "array", "maxItems": 0})
    );
}
