use super::super::tests::{input_chapter, row};
use super::*;
use crate::enrichment::ontology::OntologyV1;

/// A declaration naming header fields through a source, a date and a thread,
/// a set over the sourced type and one over documents.
fn declared() -> OntologyPolicies {
    let ontology: OntologyV1 = toml::from_str(
        r#"
[[types]]
name = "firm"
kind = "entity"
attributes = [{ name = "site", type = "text" }]
identity = ["site"]
source = { metadata = ["sender", "copied"], attributes = { site = "domain" } }

[[types]]
name = "job"
kind = "entity"
identity_criterion = "same job"

[[types]]
name = "note"
kind = "claim"
force = "assertive"
subject = "job"

[change]
document = { date = "sent", thread = "sender" }

[[sets]]
id = "home"
type = "firm"
where = { site = { suffix = "home.example" } }

[[sets]]
id = "staff_post"
type = "document"
where = { rank = ["staff"] }
"#,
    )
    .unwrap();
    ontology.into_policies()
}

#[test]
fn the_declared_fields_are_named_once_in_declaration_order() {
    assert_eq!(declared_fields(&declared()), ["sender", "copied", "sent"]);
}

#[test]
fn facts_are_the_declared_fields_and_the_sets_they_put_records_and_the_document_in() {
    let c = input_chapter(&[row(
        1,
        "d1",
        "Body.",
        r#"{"sender": "Ann <ann@away.example>", "copied": "Bo <bo@desk.home.example>",
            "sent": "2026-01-02", "rank": "staff", "unread": "never shown"}"#,
    )]);
    let facts = facts(&c.source_documents[0], &declared());
    assert_eq!(
        facts,
        "Declared facts of this document:\n\
         sender: Ann <ann@away.example>\n\
         copied: Bo <bo@desk.home.example>\n\
         sent: 2026-01-02\n\
         copied names firm desk.home.example, which is in `home`\n\
         this document is in `staff_post`\n\n"
    );
}

#[test]
fn a_document_holding_no_declared_field_has_no_facts() {
    let c = input_chapter(&[row(1, "d1", "Body.", r#"{"other": "x"}"#)]);
    assert_eq!(facts(&c.source_documents[0], &declared()), "");
}

#[test]
fn every_question_carries_the_facts_first() {
    let prompt = crate::enrichment::pipeline::types::ChatPrompt::new("s", "question");
    assert_eq!(carrying(prompt.clone(), "").user, "question");
    assert_eq!(carrying(prompt, "facts\n\n").user, "facts\n\nquestion");
}
