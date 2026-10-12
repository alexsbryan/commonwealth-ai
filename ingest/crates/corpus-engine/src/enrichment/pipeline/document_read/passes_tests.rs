use serde_json::{json, Value};

use super::super::tests::{input_chapter, policies, row, scripted};
use super::*;
use crate::enrichment::pipeline::document_read::DocumentReadField;

/// The reader over `chapter`, its line classes indexed over the chapter alone.
async fn read_all(
    chapter: &ChapterInput,
    p: &OntologyPolicies,
    infer: &InferenceFn,
) -> Result<String> {
    read(
        chapter,
        p,
        &LineClasses::of(&chapter.source_documents, p),
        infer,
    )
    .await
}

fn passes_policies() -> OntologyPolicies {
    policies()
}

fn none() -> Value {
    json!({"A": 0.05, "B": 0.05, "0": 0.9})
}

#[test]
fn the_plan_is_generated_from_the_contract() {
    let p = passes_policies();
    let plan = Plan::of(&p);
    let kinds: Vec<&str> = plan.kinds.iter().map(|k| k.decl.name.as_str()).collect();
    assert_eq!(kinds, ["membership", "reported_status"]);
    let decided: Vec<&str> = plan.decided.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(decided, ["case"], "About asks of each decided subject once");
    let status = &plan.kinds[1];
    assert_eq!(status.subject.name, "case");
    assert!(
        matches!(&status.fields[..], [FieldPlan::Choose(a)] if a.name == "status" && a.values == ["open", "closed"])
    );
    let subject: Vec<(&str, Option<&str>)> = status
        .subject_fields
        .iter()
        .flat_map(|f| f.names().into_iter().map(|n| (n, f.pass())))
        .collect();
    assert_eq!(
        subject,
        [("number", Some("Point")), ("project", Some("Choose"))]
    );
    assert!(plan.kinds[0].fields.is_empty());
}

#[test]
fn lines_are_the_non_empty_lines_with_their_trimmed_spans() {
    let body = "  a b \n\n c\r\nlast";
    let got: Vec<(usize, &str)> = lines(body)
        .iter()
        .map(|l| (l.n, &body[l.start..l.end]))
        .collect();
    assert_eq!(got, [(1, "a b"), (2, "c"), (3, "last")]);
}

#[test]
fn consecutive_lines_of_one_kind_are_cut_into_statements_of_at_most_three() {
    let located = [
        None,
        Some(0),
        Some(0),
        Some(0),
        Some(0),
        Some(1),
        None,
        Some(0),
    ];
    assert_eq!(
        statements(&located),
        [(0, 1..4), (0, 4..5), (1, 5..6), (0, 7..8)]
    );
}

#[tokio::test]
async fn a_located_line_becomes_a_claim_that_validation_keeps() {
    let p = passes_policies();
    let body = "Thread title\nThe case   is closed now.\nThanks";
    let chapter = input_chapter(&[row(1, "doc-1", body, "{}")]);
    let (infer, seen) = scripted(vec![
        none(),
        json!({"A": 0.1, "B": 0.85, "0": 0.05}),
        none(),
        // About: the document is about no case, so the statement is its own.
        none(),
        json!({"A": 0.1, "B": 0.8, "0": 0.1}),
        // `number` is pointed at: none of the statement's five words.
        json!({"A": 0.02, "B": 0.02, "C": 0.02, "D": 0.02, "E": 0.02, "0": 0.9}),
        json!({"A": 0.2, "B": 0.1, "0": 0.7}),
    ]);
    let envelope = read_all(&chapter, &p, &infer).await.unwrap();
    let mut extraction = super::super::parse_response(&envelope, &p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, &p, &mut extraction).unwrap();
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    assert_eq!(outcome.status, DocumentReadStatus::Read, "{outcome:?}");
    assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
    let claim = &outcome.claims[0];
    assert_eq!(
        (claim.kind.as_str(), claim.subject_type.as_str()),
        ("reported_status", "case")
    );
    assert_eq!(claim.evidence, "The case   is closed now.");
    assert_eq!(
        (
            claim.subject_local_ref.as_str(),
            claim.subject_name.as_str()
        ),
        ("l2-2", "case l2-2")
    );
    assert_eq!(claim.content, "The case is closed now.");
    assert_eq!(
        claim.fields["status"],
        DocumentReadField::Supported {
            value: json!("closed"),
            evidence: "The case   is closed now.".into(),
            by: Some(crate::enrichment::atlas::precision::SourcePrecision::new(
                "reader_choose",
                crate::enrichment::atlas::precision::Precision::Unmeasured
            )),
        }
    );
    assert!(
        matches!(&claim.subject_fields["project"], DocumentReadField::Unknown { reason } if reason.contains("none of the values"))
    );
    assert!(
        matches!(&claim.subject_fields["number"], DocumentReadField::Unknown { reason } if reason.contains("not stated"))
    );
    assert!(!extraction.claims.is_empty(), "the kept claim is projected");

    let prompts = seen.lock().unwrap();
    assert_eq!(
        prompts.len(),
        7,
        "three lines located, About asked, then status chosen, number pointed at, project chosen"
    );
    assert_eq!(
        prompts[0].phase_id.as_deref(),
        Some("document_passes_locate")
    );
    assert!(prompts[1]
        .user
        .contains("<<<\n1 Thread title\n2 The case   is closed now.\n3 Thanks\n>>>\n\nLine 2: \"The case   is closed now.\""));
    assert!(prompts[1]
        .user
        .contains("A membership\nB reported_status\n   status: open, closed\n0 none of them"));
    assert_eq!(
        prompts[3].phase_id.as_deref(),
        Some("document_passes_about")
    );
    assert_eq!(
        prompts[4].phase_id.as_deref(),
        Some("document_passes_choose")
    );
    assert!(prompts[4]
        .user
        .contains("Type: reported_status\nAttribute: status"));
    assert!(
        prompts[4].user.contains("[[The case is closed now.]]"),
        "{}",
        prompts[4].user
    );
    assert_eq!(
        prompts[5].phase_id.as_deref(),
        Some("document_passes_point")
    );
    assert!(prompts[5]
        .user
        .contains("Type: case (A support case.)\nAttribute: number, words of the statement"));
    assert!(prompts[6]
        .user
        .contains("Type: case (A support case.)\nAttribute: project"));
}

#[tokio::test]
async fn a_document_with_nothing_located_reads_nothing_applicable() {
    let p = passes_policies();
    let chapter = input_chapter(&[row(1, "doc-1", "Hello\nBye", "{}")]);
    let (infer, _) = scripted(vec![none(), none()]);
    let envelope = read_all(&chapter, &p, &infer).await.unwrap();
    let mut extraction = super::super::parse_response(&envelope, &p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, &p, &mut extraction).unwrap();
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    assert_eq!(outcome.status, DocumentReadStatus::NothingApplicable);
    assert!(outcome.reason.as_deref().unwrap().contains("no line of 2"));
}

#[tokio::test]
async fn a_refused_locate_call_is_never_a_located_line() {
    let p = passes_policies();
    let chapter = input_chapter(&[row(1, "doc-1", "Hello\nBye", "{}")]);
    // The model answers no distribution at all: both lines refuse.
    let (infer, _) = scripted(vec![json!("A"), json!({"A": 1.0})]);
    let envelope = read_all(&chapter, &p, &infer).await.unwrap();
    let outcome: Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(outcome["documents"][0]["status"], "could_not_judge");
}

/// The Locate system prompt mentions field values only when some kind in the
/// question shows them: a declaration with no closed claim value (GVC's) asks
/// the pre-E4 question byte for byte.
#[test]
fn the_locate_system_prompt_names_values_only_when_a_kind_shows_them() {
    let p = passes_policies();
    let full = Plan::of(&p);
    let with = locate_system(&full);
    assert!(
        with.contains("with what it means, followed by the values its fields can take. Answer"),
        "{with}"
    );
    let bare = Plan {
        kinds: Plan::of(&p)
            .kinds
            .into_iter()
            .filter(|k| k.fields.is_empty())
            .collect(),
        picked: Vec::new(),
        decided: Vec::new(),
    };
    assert_eq!(bare.kinds.len(), 1, "membership declares no field");
    let without = locate_system(&bare);
    assert!(without.contains("with what it means. Answer"), "{without}");
    assert!(
        !without.contains("values") && !without.contains('{'),
        "{without}"
    );
}

/// Steps 1 and 2 of order ontology-layer-15: every question about a document
/// opens with its declared facts, and a line carried, under a mark, from a
/// document dated earlier is never asked.
#[tokio::test]
async fn facts_open_every_question_and_a_quoted_line_is_never_located() {
    let mut p = passes_policies();
    p.change.document = Some(crate::enrichment::ontology::DocumentFieldsDecl {
        date: Some("date".into()),
        thread: None,
        id: None,
        author: Some("from".into()),
    });
    let chapter = input_chapter(&[
        row(
            1,
            "doc-1",
            "Hello\nThe case is closed now.",
            r#"{"date": "2026-01-01", "from": "a@x.example"}"#,
        ),
        row(
            2,
            "doc-2",
            "> The case is closed now.\nThanks",
            r#"{"date": "2026-01-02", "from": "b@y.example"}"#,
        ),
    ]);
    let (infer, seen) = scripted(vec![
        none(),
        json!({"A": 0.1, "B": 0.85, "0": 0.05}),
        // About doc-1: no case.
        none(),
        json!({"A": 0.1, "B": 0.8, "0": 0.1}),
        json!({"A": 0.02, "B": 0.02, "C": 0.02, "D": 0.02, "E": 0.02, "0": 0.9}),
        json!({"A": 0.2, "B": 0.1, "0": 0.7}),
        none(),
        // About doc-2: no case.
        none(),
    ]);
    read_all(&chapter, &p, &infer).await.unwrap();
    let prompts = seen.lock().unwrap();
    assert_eq!(prompts.len(), 8, "doc-2's quoted line is not asked");
    for (i, prompt) in prompts.iter().enumerate() {
        let from = if i < 6 { "a@x" } else { "b@y" };
        assert!(
            prompt.user.starts_with(&format!(
                "Declared facts of this document:\ndate: 2026-01-0{}\nfrom: {from}.example\n\n",
                if i < 6 { 1 } else { 2 }
            )),
            "{}",
            prompt.user
        );
    }
    assert!(prompts[6].user.contains("Line 2: \"Thanks\""));
    assert!(!prompts
        .iter()
        .any(|p| p.user.contains("Line 1: \"> The case")));
}

/// A subject its type's metadata source names (ONTOLOGY_METHOD §Reading): a
/// promise's person is picked among the people the message's fields name, not
/// pointed at in the promising sentence, and the pick is written as the
/// person's identity reading, which validation keeps.
#[tokio::test]
async fn a_sourced_subject_is_picked_among_the_records_its_source_reads() {
    let p: OntologyPolicies = toml::from_str::<crate::enrichment::ontology::OntologyV1>(
        r#"
[[types]]
name = "person"
kind = "entity"
attributes = [{ name = "address", type = "text" }]
identity = ["address"]
source = { metadata = ["sender", "copied"], attributes = { address = "address" } }

[[types]]
name = "promise"
kind = "claim"
force = "commissive"
subject = "person"
"#,
    )
    .unwrap()
    .into_policies();
    let plan = Plan::of(&p);
    assert!(
        plan.decided.is_empty(),
        "a sourced subject's particulars are its source's, never asked About"
    );
    let promise = &plan.kinds[0];
    assert!(
        matches!(&promise.subject_fields[..], [FieldPlan::Subject(k, "person", keys)]
            if k.name == "promise" && keys.iter().map(|a| a.name.as_str()).eq(["address"])),
        "the person is picked, never pointed at"
    );
    let body = "Hello\nI will send the signed contract by Friday.\nAnn";
    let metadata = r#"{"sender": "Ann <ann@away.example>", "copied": "Bo <bo@desk.example>"}"#;
    let chapter = input_chapter(&[row(1, "doc-1", body, metadata)]);
    let (infer, seen) = scripted(vec![
        json!({"A": 0.05, "0": 0.95}),
        json!({"A": 0.9, "0": 0.1}),
        json!({"A": 0.05, "0": 0.95}),
        json!({"A": 0.9, "B": 0.05, "0": 0.05}),
    ]);
    let envelope = read_all(&chapter, &p, &infer).await.unwrap();
    let mut extraction = super::super::parse_response(&envelope, &p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, &p, &mut extraction).unwrap();
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    assert_eq!(outcome.status, DocumentReadStatus::Read, "{outcome:?}");
    assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
    assert_eq!(
        outcome.claims[0].subject_fields["address"],
        DocumentReadField::Supported {
            value: json!("ann@away.example"),
            evidence: "Ann <ann@away.example>".into(),
            by: Some(crate::enrichment::atlas::precision::SourcePrecision::new(
                "reader_pick",
                crate::enrichment::atlas::precision::Precision::Unmeasured
            )),
        }
    );
    let prompts = seen.lock().unwrap();
    assert_eq!(
        prompts.len(),
        4,
        "three lines located, then the subject picked"
    );
    assert_eq!(prompts[3].phase_id.as_deref(), Some(pick::PICK_PHASE));
    assert!(
        prompts[3]
            .user
            .contains("Type: promise\nAttribute: subject, one person")
            && prompts[3].user.contains(
                "Which person does the statement's subject refer to?\n\
                 A person ann@away.example, named in the field `sender`\n\
                 B person bo@desk.example, named in the field `copied`\n"
            ),
        "{}",
        prompts[3].user
    );
}
