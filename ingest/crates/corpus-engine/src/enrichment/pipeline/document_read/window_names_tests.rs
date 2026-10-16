//! A particular's other names (`other_names`, the answer's typed list):
//! found by any, carried for RESOLVE to show, and never split (`window.rs`).

use serde_json::{json, Value};

use super::super::tests::{input_chapter, row, scripted};
use super::super::{parse_response, validate_and_stamp, DocumentReadField};
use super::read;

fn value(field: &DocumentReadField) -> Option<&Value> {
    match field {
        DocumentReadField::Supported { value, .. } => Some(value),
        DocumentReadField::Unknown { .. } => None,
    }
}

/// Failing input, named: world-s5's window stated "pellica chemical is elk
/// bluff cement company now same folks same deal just new name" in a
/// statement about another deal, and RESOLVE, shown one name on each side,
/// read two counterparties (arm c123-verify, 2026-10-15). A " / " joined
/// name was the first repair (def855657) and split any name that holds one;
/// the answer now lists a particular's other names (`other_names`, typed). A
/// reference to it is found by any of its names, to the record the
/// document's fields name (its identity is the value) or to words the
/// document holds (the value is the name held); the statement carries the
/// other names for RESOLVE to show (`other_names` on the claim); a subject's
/// sourced identity and an unstated particular's line are found by any too.
#[tokio::test]
async fn a_particular_with_other_names_is_found_by_any_of_them() {
    let ontology: crate::enrichment::ontology::OntologyV1 = toml::from_str(
        r#"
[[types]]
name = "firm"
kind = "entity"
attributes = [{ name = "site", type = "text" }, { name = "label", type = "text" }]
identity = ["site"]
source = { metadata = ["sender"], attributes = { site = "domain", label = "display_name" } }

[[types]]
name = "job"
kind = "entity"
identity_criterion = "same job"

[[types]]
name = "note"
kind = "claim"
force = "assertive"
subject = "job"
attributes = [{ name = "party", type = "ref", of = "firm" }]

[[types]]
name = "remark"
kind = "claim"
force = "assertive"
subject = "firm"
"#,
    )
    .unwrap();
    let policies = ontology.into_policies();
    let chapter = input_chapter(&[
        row(
            1,
            "doc-a",
            "Away Corp quoted the job.",
            r#"{"sender": "Ann <ann@away.example>"}"#,
        ),
        row(2, "doc-b", "The job went to Bayside.", "{}"),
    ]);
    let answer = json!({
        "particulars": [
            {"ref": "p1", "type": "job", "name": "the job", "fields": {}},
            {"ref": "p2", "type": "firm", "name": "Away Corp", "other_names": ["Ann"], "fields": {}},
            {"ref": "p3", "type": "job", "name": "the harbor work", "other_names": ["Bayside"], "fields": {}},
            {"ref": "p4", "type": "firm", "name": "Coastline", "other_names": ["Bayside"], "fields": {}}],
        "claims": [
            {"kind": "note", "about": "p1", "document": "d1", "quote": "Away Corp quoted the job.",
             "fields": {"party": "Away Corp"}},
            {"kind": "note", "about": "p1", "document": "d2", "quote": "The job went to Bayside.",
             "fields": {"party": "Coastline"}},
            {"kind": "remark", "about": "p2", "document": "d1", "quote": "Away Corp quoted the job.",
             "fields": {}}]});
    let (infer, seen) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let user = seen.lock().unwrap()[0].user.clone();
    assert!(user.contains("\"other_names\""), "the answer shape lists other names");
    assert!(
        !user.contains("joined by") && !user.contains("stated change"),
        "no prompt rule carries a rename or an amendment"
    );
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let read = extraction.document_read.as_ref().unwrap();
    let party = |d: usize| value(&read.documents[d].claims[0].fields["party"]).cloned();
    assert_eq!(
        party(0),
        Some(json!("away.example")),
        "found by the other name the document's fields name: {:?}",
        read.documents[0]
    );
    assert_eq!(
        party(1),
        Some(json!("Bayside")),
        "found by the other name its document holds: {:?}",
        read.documents[1]
    );
    assert_eq!(
        read.documents[1].claims[0].other_names.get("Bayside"),
        Some(&vec!["Coastline".to_string()]),
        "the statement carries the other name for RESOLVE to show"
    );
    let remark = read.documents[0]
        .claims
        .iter()
        .find(|c| c.kind == "remark")
        .expect("the remark is read");
    assert_eq!(
        value(&remark.subject_fields["site"]),
        Some(&json!("away.example")),
        "{remark:?}"
    );
    let placed: Vec<_> = read.documents[1]
        .particulars
        .iter()
        .map(|p| (p.evidence.as_str(), p.other_names.get("the harbor work").cloned()))
        .collect();
    assert_eq!(
        placed,
        [(
            "The job went to Bayside.",
            Some(vec!["Bayside".to_string()])
        )],
        "an unstated particular is placed by any of its names: {:?}",
        read.documents[1]
    );
}

/// A name that holds " / " is one name: a reference written "Input / Output"
/// is never found by "Output" alone, as the retired " / " notation did.
#[tokio::test]
async fn a_name_holding_a_slash_is_one_name() {
    let ontology: crate::enrichment::ontology::OntologyV1 = toml::from_str(
        r#"
[[types]]
name = "firm"
kind = "entity"
identity_criterion = "same firm"

[[types]]
name = "job"
kind = "entity"
identity_criterion = "same job"

[[types]]
name = "note"
kind = "claim"
force = "assertive"
subject = "job"
attributes = [{ name = "party", type = "ref", of = "firm" }]
"#,
    )
    .unwrap();
    let policies = ontology.into_policies();
    let chapter = input_chapter(&[row(1, "doc-a", "Output shipped the job.", "{}")]);
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "job", "name": "the job", "fields": {}}],
        "claims": [{"kind": "note", "about": "p1", "document": "d1",
                    "quote": "Output shipped the job.", "fields": {"party": "Input / Output"}}]});
    let (infer, _) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let read = extraction.document_read.as_ref().unwrap();
    assert!(
        matches!(&read.documents[0].claims[0].fields["party"], DocumentReadField::Unknown { .. }),
        "\"Input / Output\" is not in the document, and \"Output\" alone is another name: {:?}",
        read.documents[0].claims[0].fields
    );
}
