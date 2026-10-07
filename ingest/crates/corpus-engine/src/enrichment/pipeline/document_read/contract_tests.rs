// SPDX-License-Identifier: AGPL-3.0-or-later
use super::tests::{input_chapter, policies, row};
use super::*;
use serde_json::Value;

fn schema_for_one_document() -> Value {
    let p = policies();
    let input = input_chapter(&[row(
        1,
        "doc-a",
        "Issue 842 was closed.",
        r#"{"author":"maintainer"}"#,
    )]);
    compose(&input, &p, "phase1")
        .response_schema
        .expect("the document-read request carries a decoder schema")
}

fn outcome_branch(schema: &Value, status: &str) -> Value {
    schema["properties"]["documents"]["items"]["oneOf"]
        .as_array()
        .expect("each outcome status is its own decoder branch")
        .iter()
        .find(|branch| branch["properties"]["status"]["const"] == status)
        .cloned()
        .unwrap_or_else(|| panic!("no branch for status `{status}`"))
}

fn claim_branch(schema: &Value, kind: &str) -> Value {
    outcome_branch(schema, "read")["properties"]["claims"]["items"]["oneOf"]
        .as_array()
        .expect("each eligible claim kind is its own decoder branch")
        .iter()
        .find(|branch| branch["properties"]["kind"]["const"] == kind)
        .cloned()
        .unwrap_or_else(|| panic!("no claim branch for kind `{kind}`"))
}

#[test]
fn decoder_contract_binds_fields_and_subject_to_each_claim_kind() {
    let schema = schema_for_one_document();
    let member = claim_branch(&schema, "membership");
    assert_eq!(member["properties"]["subject_type"]["const"], "case");
    assert_eq!(
        member["properties"]["fields"]["properties"]
            .as_object()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        member["properties"]["fields"]["additionalProperties"],
        false
    );
    let state = claim_branch(&schema, "reported_status");
    assert_eq!(
        state["properties"]["fields"]["properties"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        state["properties"]["fields"]["required"],
        serde_json::json!(["status"])
    );
    assert_eq!(
        state["properties"]["subject_fields"]["required"],
        serde_json::json!(["number", "project"])
    );
    assert!(state["properties"]["fields"]["properties"]
        .get("due")
        .is_none());
    assert_eq!(
        state["properties"]["fields"]["properties"]["status"]["oneOf"][1]["required"],
        serde_json::json!(["status", "reason"])
    );
}

#[test]
fn decoder_outcome_requires_claims_for_read_and_reason_for_abstention() {
    let schema = schema_for_one_document();
    assert_eq!(schema["properties"]["documents"]["minItems"], 1);
    assert_eq!(schema["properties"]["documents"]["maxItems"], 1);
    assert_eq!(
        outcome_branch(&schema, "read")["properties"]["claims"]["minItems"],
        1
    );
    for status in ["nothing_applicable", "could_not_judge", "not_read"] {
        let branch = outcome_branch(&schema, status);
        assert_eq!(branch["properties"]["claims"]["maxItems"], 0);
        assert_eq!(branch["properties"]["reason"]["minLength"], 1);
        assert!(branch["required"]
            .as_array()
            .unwrap()
            .contains(&Value::String("reason".into())));
    }
    assert_eq!(
        schema["properties"]["documents"]["items"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}
