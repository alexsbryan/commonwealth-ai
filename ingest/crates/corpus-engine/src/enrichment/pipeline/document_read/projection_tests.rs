// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{json, Value};

use super::*;
use crate::enrichment::pipeline::document_read::CLAIM_FIELDS_ATTRIBUTE;
use crate::recipe::Recipe;

const RECIPE: &str = r#"
[corpus]
id = "field-provenance"
name = "field-provenance"
[acquire]
type = "local_file"
path = "/tmp/field-provenance.jsonl"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
domain = "declared records"
[enrichment.ontology]
version = 1

[[enrichment.ontology.types]]
name = "issue"
kind = "entity"
identity_criterion = "the same issue"
attributes = [{ name = "number", type = "text" }]

[[enrichment.ontology.types]]
name = "update"
kind = "claim"
force = "assertive"
subject = "issue"
attributes = [
  { name = "action", type = "text" },
  { name = "polarity", type = "text", values = ["affirmative", "negative"] },
]
"#;

fn policies() -> crate::enrichment::ontology::OntologyPolicies {
    Recipe::from_toml(RECIPE)
        .unwrap()
        .custom_atlas_spec()
        .expect("the recipe has a custom atlas")
        .policies()
}

#[test]
fn compatibility_claim_keeps_supported_and_unknown_document_field_evidence() {
    let source = "Alice wrote: Issue 842 was closed by change 1380.";
    let response = json!({
        "documents": [{
            "document_id": "doc-842",
            "status": "read",
            "claims": [{
                "kind": "update",
                "content": "Issue 842 was closed by change 1380.",
                "subject_type": "issue",
                "subject_local_ref": "issue-842",
                "subject_name": "Issue 842",
                "speaker": "Alice",
                "evidence": source,
                "fields": {
                    "action": {
                        "status": "supported",
                        "value": "closed_by_change",
                        "evidence": "closed by change 1380"
                    },
                    "polarity": {
                        "status": "unknown",
                        "reason": "the passage does not state polarity"
                    }
                },
                "subject_fields": {
                    "number": {
                        "status": "supported",
                        "value": "842",
                        "evidence": "842"
                    }
                }
            }]
        }]
    });
    let parsed = parse_response(&response.to_string(), &policies()).unwrap();
    let extraction = parsed.section_extraction.unwrap();
    let attrs = &extraction.claims[0].attributes;
    let fields = attrs[CLAIM_FIELDS_ATTRIBUTE]
        .as_object()
        .expect("compatibility projection retains the typed field carrier");

    assert_eq!(attrs["action"], Value::String("closed_by_change".into()));
    assert!(
        !attrs.contains_key("polarity"),
        "unknown is not flattened into an answer"
    );
    assert_eq!(fields["action"]["status"], "supported");
    assert_eq!(fields["action"]["value"], "closed_by_change");
    assert_eq!(fields["action"]["evidence"], "closed by change 1380");
    assert_eq!(fields["polarity"]["status"], "unknown");
    assert_eq!(
        fields["polarity"]["reason"],
        "the passage does not state polarity"
    );
    assert!(
        !attrs.contains_key("speaker"),
        "speaker attribution is not a scalar qualification"
    );
    assert!(
        !fields.contains_key("speaker"),
        "attribution has no separate field-evidence contract"
    );
    assert_eq!(extraction.claims[0].attributed_to.as_deref(), Some("Alice"));
}
