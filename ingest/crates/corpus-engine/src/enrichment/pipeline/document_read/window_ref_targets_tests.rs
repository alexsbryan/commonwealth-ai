//! A type a reference targets that nothing else records is decided by name
//! (`recipe_ontology::resolve_defaults`): the window names its particulars
//! and sends each to RESOLVE as a statement of its own.

use serde_json::json;

use super::super::tests::{input_chapter, row, scripted};
use super::super::{parse_response, validate_and_stamp, DocumentReadField};
use super::read;
use crate::recipe_ontology::OntologyBlock;

/// 02-contracts n1: `owner` refers to the base kind `person`, undeclared.
const CONTRACTS: &str = r#"
[[types]]
name = "contract"
kind = "entity"
description = "An agreement with a counterparty."
identity_criterion = "Two mentions are one contract when they name the same agreement."

[[types]]
name = "signed"
kind = "claim"
force = "assertive"
subject = "contract"
description = "A contract is signed."
attributes = [{ name = "owner", type = "ref", of = "person", description = "Who owns it on our side." }]
"#;

const LINE: &str = "Dana Reyes signed the MSA for us.";

/// Failing input, named: 02-contracts n1. The window named Dana Reyes as a
/// `person` and dropped her ("no statement kind is about this type"), so
/// `owner` named no record. With `person` decided by name, the prompt lists
/// it with the default criterion, and she is sent to RESOLVE on the line that
/// names her.
#[tokio::test]
async fn a_referenced_person_is_named_and_sent_to_resolve() {
    let policies = OntologyBlock {
        version: 1,
        body: toml::from_str(CONTRACTS).unwrap(),
    }
    .policies()
    .unwrap();
    let chapter = input_chapter(&[row(1, "doc-a", LINE, r#"{"author":"ana"}"#)]);
    let answer = json!({
        "particulars": [
            {"ref": "p1", "type": "contract", "name": "the MSA", "fields": {}},
            {"ref": "p2", "type": "person", "name": "Dana Reyes", "fields": {}}],
        "claims": [{"kind": "signed", "about": "p1", "document": "d1", "quote": LINE,
                    "fields": {"owner": "p2"}}]});
    let (infer, seen) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();

    let prompt = &seen.lock().unwrap()[0];
    assert!(
        prompt.user.contains(
            "- person: A person the statements refer to.\n  Same person when: Two mentions are \
             one person when they name the same person, however the name is written."
        ),
        "{}",
        prompt.user
    );
    let read = extraction.document_read.as_ref().unwrap();
    let [a] = read.documents.as_slice() else {
        panic!("one outcome: {:?}", read.documents)
    };
    assert!(
        matches!(&a.claims[0].fields["owner"], DocumentReadField::Supported { value, .. } if value == "Dana Reyes"),
        "{:?}",
        a.claims[0].fields
    );
    let [dana] = a.particulars.as_slice() else {
        panic!("one particular sent: {:?}", a.particulars)
    };
    assert_eq!(
        (dana.subject_type.as_str(), dana.subject_name.as_str()),
        ("person", "Dana Reyes")
    );
    assert_eq!(dana.evidence, LINE);
}
