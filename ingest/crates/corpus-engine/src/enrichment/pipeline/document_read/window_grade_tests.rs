//! The answer shape: it names the types and statement kinds apart, offers the
//! grade slot only when a kind lists grades, and a grade written as an object
//! is fields.

use serde_json::json;

use super::super::tests::{input_chapter, policies, row, scripted};
use super::super::{parse_response, validate_and_stamp};
use super::read;
use super::tests::{value, window};

/// Failing input, named: local Qwen3.6-35B on world-s4 at 20k
/// (runs/loop/local35b-s4-base-a) wrote 29 claims' stage as
/// `"grade": {"stage": "lead"}` beside an empty `"fields"`, a recipe whose
/// kinds list no grades, and every record lost its stage (0 of 15 served). A
/// grade is one of its kind's grades, never an object, so an object's keys are
/// read in `fields`; one `fields` already holds is not, and a grade written as
/// a grade stays one.
#[tokio::test]
async fn a_field_written_in_grade_is_read_in_fields() {
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "grade": {"status": "Closed"}, "fields": {}},
                   {"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "grade": {"status": "Open"},
                    "fields": {"status": "Closed"}},
                   {"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "grade": "firm", "fields": {"status": "Closed"}}]});
    let parsed = super::answer::parse(&answer.to_string(), "sec_1").unwrap();
    assert_eq!(parsed.in_grade, 1);
    assert_eq!(parsed.value["claims"][2]["grade"], json!("firm"));
    let (policies, chapter) = (policies(), window());
    let (infer, _) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let claims = &extraction.document_read.as_ref().unwrap().documents[1].claims;
    let status: Vec<_> = claims
        .iter()
        .map(|c| value(&c.fields["status"]).cloned())
        .collect();
    assert_eq!(
        status,
        vec![
            Some(json!("closed")),
            Some(json!("closed")),
            Some(json!("closed"))
        ]
    );
}

/// Failing input, named: world-s4's recipe lists no grades, yet every claim in
/// the answer shape offered a `grade`, and the stage went there: local
/// Qwen3.6-35B wrote it only there (`{"stage": "lead"}` in one read, `"lead"`
/// in the next; 0 of 15 deals with a stage), and DeepSeek beside `fields` in
/// 421 claims. The slot is shown only when a kind the window reads lists
/// grades, and stays in the envelope an answer is read with.
#[tokio::test]
async fn the_grade_slot_is_shown_only_when_a_kind_lists_grades() {
    let recipe = |grades: &str| {
        let ontology: crate::enrichment::ontology::OntologyV1 = toml::from_str(&format!(
            r#"
[[types]]
name = "case"
kind = "entity"
identity_criterion = "same issue number"

[[types]]
name = "remark"
kind = "claim"
force = "assertive"
subject = "case"
{grades}
"#
        ))
        .unwrap();
        ontology.into_policies()
    };
    let chapter = input_chapter(&[row(1, "doc-a", "Issue 42 is open.", "{}")]);
    let mut shown = Vec::new();
    for policies in [recipe(""), recipe(r#"grades = ["saw it", "heard it"]"#)] {
        let empty = json!({"particulars": [], "claims": []});
        let (infer, seen) = scripted(vec![empty]);
        read(&chapter, &policies, &Default::default(), &infer)
            .await
            .unwrap();
        let user = seen.lock().unwrap()[0].user.clone();
        shown.push(user.contains("\"grade\": <one of its kind's grades"));
    }
    assert_eq!(shown, [false, true]);
    assert!(super::answer::CLAIM_KEYS.contains("grade"));
}

/// Failing input, named: the layer-news journey (2026-10-20). Its one window
/// wrote each happening as a particular of type `report` and each report as a
/// claim of kind `happening`, and all 12 claims were refused: the prompt listed
/// kinds and types in one list and asked for "a type above". Each set is listed
/// under its own heading, and the answer shape names its members.
#[tokio::test]
async fn the_types_and_statement_kinds_are_named_apart() {
    let ontology: crate::enrichment::ontology::OntologyV1 = toml::from_str(
        r#"
[[types]]
name = "happening"
kind = "event"
description = "something that happens"
identity_criterion = "the same occurrence"

[[types]]
name = "report"
kind = "claim"
force = "assertive"
subject = "happening"
description = "a statement that a happening took place"
"#,
    )
    .unwrap();
    let policies = ontology.into_policies();
    let chapter = input_chapter(&[row(1, "doc-a", "A fire broke out on Mill Lane.", "{}")]);
    let (infer, seen) = scripted(vec![json!({"particulars": [], "claims": []})]);
    read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let user = seen.lock().unwrap()[0].user.clone();
    for shown in [
        "Statement kinds, written in `claims`:\n- report (a statement about one happening)",
        "Types, written in `particulars`:\n- happening: something that happens",
        "\"type\": <one of: happening>",
        "\"kind\": <one of: report>",
    ] {
        assert!(user.contains(shown), "{shown} not in:\n{user}");
    }
}
