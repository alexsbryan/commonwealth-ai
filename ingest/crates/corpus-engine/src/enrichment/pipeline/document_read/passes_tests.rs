use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::super::tests::{input_chapter, policies, row};
use super::*;

fn scripted(answers: Vec<Value>) -> (InferenceFn, Arc<Mutex<Vec<ChatPrompt>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(answers.into_iter()));
    let kept = seen.clone();
    let f: InferenceFn = Arc::new(move |p: &ChatPrompt, _| {
        kept.lock().unwrap().push(p.clone());
        let next = queue.lock().unwrap().next().map(|v| v.to_string());
        Box::pin(async move {
            next.ok_or_else(|| crate::Error::Extraction("no scripted answer left".into()))
        })
    });
    (f, seen)
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
    let status = &plan.kinds[1];
    assert_eq!(status.subject.name, "case");
    assert!(
        matches!(&status.fields[..], [FieldPlan::Choose(a)] if a.name == "status" && a.values == ["open", "closed"])
    );
    let subject: Vec<(&str, bool)> = status
        .subject_fields
        .iter()
        .map(|f| match f {
            FieldPlan::Choose(a) => (a.name.as_str(), true),
            FieldPlan::Unasked(a) => (a.name.as_str(), false),
        })
        .collect();
    assert_eq!(subject, [("number", false), ("project", true)]);
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
        json!({"A": 0.1, "B": 0.8, "0": 0.1}),
        json!({"A": 0.2, "B": 0.1, "0": 0.7}),
    ]);
    let envelope = read(&chapter, &p, &infer).await.unwrap();
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
        matches!(&claim.subject_fields["number"], DocumentReadField::Unknown { reason } if reason.contains("not asked"))
    );
    assert!(!extraction.claims.is_empty(), "the kept claim is projected");

    let prompts = seen.lock().unwrap();
    assert_eq!(
        prompts.len(),
        5,
        "three lines located, then status and project chosen"
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
        Some("document_passes_choose")
    );
    assert!(prompts[3]
        .user
        .contains("Type: reported_status\nAttribute: status"));
    assert!(
        prompts[3].user.contains("[[The case is closed now.]]"),
        "{}",
        prompts[3].user
    );
    assert!(prompts[4]
        .user
        .contains("Type: case (A support case.)\nAttribute: project"));
}

#[tokio::test]
async fn a_document_with_nothing_located_reads_nothing_applicable() {
    let p = passes_policies();
    let chapter = input_chapter(&[row(1, "doc-1", "Hello\nBye", "{}")]);
    let (infer, _) = scripted(vec![none(), none()]);
    let envelope = read(&chapter, &p, &infer).await.unwrap();
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
    let envelope = read(&chapter, &p, &infer).await.unwrap();
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
    };
    assert_eq!(bare.kinds.len(), 1, "membership declares no field");
    let without = locate_system(&bare);
    assert!(without.contains("with what it means. Answer"), "{without}");
    assert!(
        !without.contains("values") && !without.contains('{'),
        "{without}"
    );
}
