//! The window answer's JSON Schema, from the same declaration the prompt
//! renders (`ontology_text`): the engine holds the answer to it after the
//! reasoning, so a brace cannot go missing and a field cannot be written
//! anywhere but under its declared name (`2b9c1b527` lets a schema wait for
//! the think block). Values are typed by `attribute_schema`, closed lists as
//! enums; a field two kinds both declare takes the first declaration.

use serde_json::{json, Map, Value};

use super::{read_attrs, subject_attrs, Contract};
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::pipelines::ontology_schema::attribute_schema;

pub(super) fn answer_schema(contract: &Contract<'_>, policies: &OntologyPolicies) -> Value {
    let index = TypeIndex::from_policies(policies);
    let names = |types: Vec<&str>| Value::from(types);
    let (mut claim_fields, mut particular_fields) = (Map::new(), Map::new());
    for (kind, _) in &contract.kinds {
        for attr in read_attrs(&index, kind) {
            claim_fields
                .entry(attr.name.clone())
                .or_insert_with(|| attribute_schema(&attr));
        }
    }
    for subject in &contract.subjects {
        for attr in subject_attrs(policies, &index, &subject.name) {
            particular_fields
                .entry(attr.name.clone())
                .or_insert_with(|| {
                    json!({"type": "object", "properties": {"value": attribute_schema(attr),
                       "quote": {"type": "string"}}, "required": ["value", "quote"]})
                });
        }
    }
    let mut claim = json!({
        "kind": {"enum": names(contract.kinds.iter().map(|(k, _)| k.name.as_str()).collect())},
        "about": {"type": "string"},
        "document": {"type": "string"},
        "quote": {"type": "string"},
        "said_by": {"type": "string"},
        "fields": {"type": "object", "properties": claim_fields},
    });
    let mut grades: Vec<&str> = Vec::new();
    for g in contract.kinds.iter().flat_map(|(k, _)| &k.grades) {
        if !grades.contains(&g.as_str()) {
            grades.push(g);
        }
    }
    if !grades.is_empty() {
        claim["grade"] = json!({"enum": grades});
    }
    json!({
        "type": "object",
        "properties": {
            "particulars": {"type": "array", "items": {"type": "object", "properties": {
                "ref": {"type": "string"},
                "type": {"enum": names(contract.subjects.iter().map(|t| t.name.as_str()).collect())},
                "name": {"type": "string"},
                "other_names": {"type": "array", "items": {"type": "string"}},
                "fields": {"type": "object", "properties": particular_fields},
            }, "required": ["ref", "type", "name", "other_names", "fields"]}},
            "claims": {"type": "array", "items": {"type": "object", "properties": claim,
                "required": ["kind", "about", "document", "quote", "said_by", "fields"]}},
        },
        "required": ["particulars", "claims"],
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::tests::{input_chapter, row, scripted};
    use super::super::read;

    /// Failing input, named: local Qwen3.6-35B lost world-s4's window 1 whole
    /// to six particulars missing a closing brace (runs/loop/local35b-s4-base-b),
    /// and filed stages under a grade slot. The window call carries the
    /// answer's schema from the declaration: kinds, types and closed values as
    /// enums, grades only where declared.
    #[tokio::test]
    async fn the_window_call_carries_its_answer_schema() {
        let ontology: crate::enrichment::ontology::OntologyV1 = toml::from_str(
            r#"
[[types]]
name = "deal"
kind = "entity"
identity_criterion = "the same transaction"
attributes = [{ name = "product", type = "text" }]

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "deal"
attributes = [{ name = "stage", type = "text", values = ["lead", "won", "lost"] }]
"#,
        )
        .unwrap();
        let policies = ontology.into_policies();
        let chapter = input_chapter(&[row(1, "doc-a", "The deal is won.", "{}")]);
        let (infer, seen) = scripted(vec![json!({"particulars": [], "claims": []})]);
        read(&chapter, &policies, &Default::default(), &infer)
            .await
            .unwrap();
        let prompt = seen.lock().unwrap()[0].clone();
        let schema = prompt
            .response_schema
            .expect("the window call asks for a schema");
        let (claim, particular) = (
            &schema["properties"]["claims"]["items"]["properties"],
            &schema["properties"]["particulars"]["items"]["properties"],
        );
        assert_eq!(claim["kind"]["enum"], json!(["stage_update"]));
        assert_eq!(
            claim["fields"]["properties"]["stage"]["enum"],
            json!(["lead", "won", "lost"])
        );
        assert!(claim.get("grade").is_none());
        assert_eq!(particular["type"]["enum"], json!(["deal"]));
        assert!(particular["fields"]["properties"]["product"]["properties"]["quote"].is_object());
        assert!(prompt.thinking_tokens.is_some());
    }
}
