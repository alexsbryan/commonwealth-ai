use serde_json::{json, Value};

use super::super::line_classes::LineClasses;
use super::super::tests::{input_chapter, policies, row, scripted};
use super::super::{passes, DocumentReadClaim, DocumentReadField, DocumentReadStatus};
use super::*;
use crate::enrichment::ontology::OntologyPolicies;
use crate::enrichment::pipeline::types::ChapterInput;

/// One label chosen among `labels`, the rest sharing what is left.
fn pick(labels: &[&str], chosen: &str) -> Value {
    let rest = 0.1 / (labels.len() - 1) as f64;
    Value::Object(
        labels
            .iter()
            .map(|l| (l.to_string(), json!(if *l == chosen { 0.9 } else { rest })))
            .collect(),
    )
}

/// The fixture's Locate, status and project questions: membership,
/// reported_status, none.
fn three(chosen: &str) -> Value {
    pick(&["A", "B", "0"], chosen)
}

/// The kept claims of one document read by the reader and validated.
async fn read_claims(
    chapter: &ChapterInput,
    p: &OntologyPolicies,
    infer: &InferenceFn,
) -> Vec<DocumentReadClaim> {
    let classes = LineClasses::of(&chapter.source_documents, p);
    let envelope = passes::by_plan(chapter, p, &classes, infer).await.unwrap();
    let mut extraction = super::super::parse_response(&envelope, p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(chapter, p, &mut extraction).unwrap();
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    assert_eq!(outcome.status, DocumentReadStatus::Read, "{outcome:?}");
    assert!(outcome.refused.is_empty(), "{:?}", outcome.refused);
    outcome.claims.clone()
}

const BODY: &str = "Issue 42 install times out\nThe install is open and hangs.\nLogs attached.\nA maintainer closed it as fixed.";

#[tokio::test]
async fn statements_about_the_one_particular_a_document_is_about_share_its_reference_and_its_identity_is_read_once(
) {
    let p = policies();
    let chapter = input_chapter(&[row(1, "doc-1", BODY, "{}")]);
    let (infer, seen) = scripted(vec![
        // Locate: lines 2 and 4 state reported_status.
        three("0"),
        three("B"),
        three("0"),
        three("B"),
        // About: one case, first named on line 1, its words "Issue 42".
        three("A"),
        pick(&["A", "B", "C", "D", "0"], "A"),
        pick(&["A", "B", "C", "D", "E", "0"], "A"),
        pick(&["A", "B", "C", "D", "E"], "B"),
        // The case's identity, once, at its words: its number pointed at
        // ("42"), its project chosen. Then line 2's status and line 4's.
        // Nothing else.
        pick(&["A", "B", "0"], "B"),
        three("A"),
        three("A"),
        three("B"),
    ]);
    let claims = read_claims(&chapter, &p, &infer).await;
    let refs: Vec<(&str, &str)> = claims
        .iter()
        .map(|c| (c.subject_local_ref.as_str(), c.subject_name.as_str()))
        .collect();
    assert_eq!(
        refs,
        [
            ("l1:Issue 42", "case l1:Issue 42"),
            ("l1:Issue 42", "case l1:Issue 42")
        ]
    );
    assert_eq!(claims[0].subject_fields, claims[1].subject_fields);
    assert!(
        matches!(&claims[0].subject_fields["number"], DocumentReadField::Supported { value, evidence, .. } if value == "42" && evidence == "42"),
        "{:?}",
        claims[0].subject_fields
    );
    assert!(
        matches!(&claims[1].fields["status"], DocumentReadField::Supported { value, .. } if value == "closed")
    );
    let prompts = seen.lock().unwrap();
    assert_eq!(
        prompts.len(),
        12,
        "the case's fields are asked once, not per statement"
    );
    assert_eq!(prompts[4].phase_id.as_deref(), Some(ABOUT_PHASE));
    assert!(
        prompts[4].user.contains(
            "Kind of thing: case (A support case.)\nSame particular when: same issue number\n\
             \nIs the document about a particular case?\nA yes, one case\nB yes, more than one case\n0 no"
        ),
        "{}",
        prompts[4].user
    );
    assert!(
        prompts[5]
            .user
            .contains("On which line is the case the document is about first named or described?\nA line 1: Issue 42 install times out\n"),
        "{}",
        prompts[5].user
    );
    assert!(
        prompts[8].user.contains("[[Issue 42]]"),
        "the case's fields are asked at its words: {}",
        prompts[8].user
    );
}

#[tokio::test]
async fn statements_of_a_document_about_several_go_to_the_particular_each_is_asked_to_be_about() {
    let p = policies();
    let body = "Issue 7 and issue 9\nIssue 7 is open.\nNoted.\nIssue 9 is closed.\nAlso.\nThe build is closed.";
    let chapter = input_chapter(&[row(1, "doc-1", body, "{}")]);
    let six = ["A", "B", "C", "D", "E", "F", "0"];
    let (infer, seen) = scripted(vec![
        // Locate: lines 2, 4 and 6 state reported_status.
        three("0"),
        three("B"),
        three("0"),
        three("B"),
        three("0"),
        three("B"),
        // About: more than one case. The first on line 1, "Issue 7"; the
        // second on line 1 too, "issue 9"; then none of the lines.
        three("B"),
        pick(&six, "A"),
        pick(&["A", "B", "C", "D", "E", "0"], "A"),
        pick(&["A", "B", "C", "D", "E"], "B"),
        pick(&six, "A"),
        pick(&["A", "B", "C", "D", "E", "0"], "D"),
        pick(&["A", "B"], "B"),
        pick(&six, "0"),
        // Each statement asked which case: issue 7, issue 9, none of them.
        three("A"),
        three("B"),
        three("0"),
        // Issue 7's identity at its words: number ("7"), project; then
        // issue 9's: number ("9"), project.
        pick(&["A", "B", "0"], "B"),
        three("A"),
        pick(&["A", "B", "0"], "B"),
        three("A"),
        // Line 2's status, line 4's.
        three("A"),
        three("B"),
        // Line 6, about none: status, then its own number (not stated) and
        // project (none), as a statement of its own.
        three("B"),
        pick(&["A", "B", "C", "D", "0"], "0"),
        three("0"),
    ]);
    let claims = read_claims(&chapter, &p, &infer).await;
    let refs: Vec<&str> = claims
        .iter()
        .map(|c| c.subject_local_ref.as_str())
        .collect();
    assert_eq!(refs, ["l1:Issue 7", "l1:issue 9", "l6-6"]);
    let number = |c: &DocumentReadClaim| match &c.subject_fields["number"] {
        DocumentReadField::Supported { value, .. } => value.clone(),
        DocumentReadField::Unknown { .. } => Value::Null,
    };
    assert_eq!(
        claims.iter().map(number).collect::<Vec<_>>(),
        [json!("7"), json!("9"), Value::Null]
    );
    let prompts = seen.lock().unwrap();
    assert_eq!(prompts.len(), 26);
    assert!(
        prompts[10].user.contains("Already found, each where it is first named or described:\n- line 1: \"Issue 7\"\n")
            && prompts[10].user.contains("On which line is another case, not the same particular as any already found, the document is about"),
        "{}",
        prompts[10].user
    );
    assert!(
        prompts[14]
            .user
            .contains("[[Issue 7 is open.]]")
            && prompts[14].user.contains(
                "Which case is the statement about?\nA Issue 7 (line 1)\nB issue 9 (line 1)\n0 none of them"
            ),
        "{}",
        prompts[14].user
    );
}

#[tokio::test]
async fn a_document_about_no_particular_keeps_each_statement_its_own() {
    let p = policies();
    let chapter = input_chapter(&[row(1, "doc-1", BODY, "{}")]);
    let (infer, seen) = scripted(vec![
        three("0"),
        three("B"),
        three("0"),
        three("B"),
        // About: no case. No line or statement is asked about one.
        three("0"),
        three("A"),
        pick(&["A", "B", "C", "D", "E", "F", "0"], "0"),
        three("0"),
        three("B"),
        pick(&["A", "B", "C", "D", "E", "F", "0"], "0"),
        three("0"),
    ]);
    let claims = read_claims(&chapter, &p, &infer).await;
    let refs: Vec<&str> = claims
        .iter()
        .map(|c| c.subject_local_ref.as_str())
        .collect();
    assert_eq!(refs, ["l2-2", "l4-4"]);
    assert_eq!(seen.lock().unwrap().len(), 11);
}

#[test]
fn the_about_questions_are_in_the_read_contract_and_generated_from_the_declaration_alone() {
    let p = policies();
    let body = "first line\nsecond line";
    assert_eq!(
        passes::plan_contract_value(&p)["about"],
        contract_sample(body, &passes::lines(body))
    );
    let mut case = p.type_decl("case").unwrap().clone();
    case.name = "ticket".into();
    let q = count_question("", "", &case);
    assert!(
        q.user.contains(
            "Kind of thing: ticket (A support case.)\nSame particular when: same issue number\n\nIs the document about a particular ticket?"
        ),
        "{}",
        q.user
    );
    assert_eq!(q.phase_id.as_deref(), Some(ABOUT_PHASE));
}

#[tokio::test]
async fn a_particular_no_statement_is_about_is_carried_and_projected_with_its_type() {
    let p = policies();
    let chapter = input_chapter(&[row(1, "doc-1", "Issue 42 install times out\nThanks", "{}")]);
    let (infer, seen) = scripted(vec![
        // Locate: neither line states a claim kind.
        three("0"),
        three("0"),
        // About: one case, on line 1, "Issue 42"; its number and project.
        three("A"),
        three("A"),
        pick(&["A", "B", "C", "D", "E", "0"], "A"),
        pick(&["A", "B", "C", "D", "E"], "B"),
        pick(&["A", "B", "0"], "B"),
        three("A"),
    ]);
    let classes = LineClasses::of(&chapter.source_documents, &p);
    let envelope = passes::by_plan(&chapter, &p, &classes, &infer)
        .await
        .unwrap();
    let mut extraction = super::super::parse_response(&envelope, &p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, &p, &mut extraction).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 8);
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    assert_eq!(outcome.status, DocumentReadStatus::Read, "{outcome:?}");
    assert!(
        outcome.claims.is_empty() && outcome.refused.is_empty(),
        "{outcome:?}"
    );
    let [particular] = &outcome.particulars[..] else {
        panic!("one particular: {outcome:?}");
    };
    assert_eq!(
        (
            particular.subject_type.as_str(),
            particular.subject_local_ref.as_str(),
            particular.evidence.as_str()
        ),
        ("case", "l1:Issue 42", "Issue 42 install times out"),
        "it cites the whole line that first names it"
    );
    assert!(
        matches!(&particular.subject_fields["number"], DocumentReadField::Supported { value, .. } if value == "42")
    );
    let [sketch] = &extraction.claims[..] else {
        panic!("one projected claim: {:?}", extraction.claims);
    };
    assert_eq!(sketch.claim_kind, None);
    assert_eq!(
        crate::enrichment::pipeline::document_read::about_type(&sketch.attributes),
        Some("case")
    );
    assert_eq!(
        sketch.attributes[crate::enrichment::pipeline::document_read::LOCAL_REF_ATTRIBUTE],
        "l1:Issue 42"
    );
}

/// A type RESOLVE decides that no statement kind is about is read all the
/// same: About asks for it after the kinds' subjects, its particular carries
/// its own identifying fields, and neither check names it unread. Failing
/// input, named: the plan held only the kinds' subjects, so `case` below was
/// never asked and made no record.
#[tokio::test]
async fn a_decided_type_no_statement_kind_is_about_is_read_by_about() {
    let mut p = policies();
    p.shape
        .types
        .push(crate::enrichment::ontology::OntologyTypeDecl {
            name: "person".into(),
            kind: crate::enrichment::ontology::TypeKind::Entity,
            description: "Someone.".into(),
            identity_criterion: Some("the same name".into()),
            ..Default::default()
        });
    for t in p.shape.types.iter_mut() {
        if t.subject.as_deref() == Some("case") {
            t.subject = Some("person".into());
        }
    }
    let plan = passes::Plan::of(&p);
    let decided: Vec<&str> = plan.decided.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(decided, ["person", "case"], "the kinds' subjects first");
    let identifying: Vec<&str> = plan
        .identifying("case")
        .iter()
        .flat_map(|f| f.names())
        .collect();
    assert_eq!(identifying, ["number", "project"]);
    assert!(super::super::unread_types(&p).is_empty());
    assert!(crate::enrichment::atlas::resolution_records::types_without_statements(&p).is_empty());

    let chapter = input_chapter(&[row(1, "doc-1", "Issue 42 install times out\nThanks", "{}")]);
    let (infer, seen) = scripted(vec![
        // Locate: neither line states a claim kind.
        three("0"),
        three("0"),
        // About counts: no person; one case.
        three("0"),
        three("A"),
        // The case: on line 1, "Issue 42"; its number and project.
        three("A"),
        pick(&["A", "B", "C", "D", "E", "0"], "A"),
        pick(&["A", "B", "C", "D", "E"], "B"),
        pick(&["A", "B", "0"], "B"),
        three("A"),
    ]);
    let classes = LineClasses::of(&chapter.source_documents, &p);
    let envelope = passes::by_plan(&chapter, &p, &classes, &infer)
        .await
        .unwrap();
    let mut extraction = super::super::parse_response(&envelope, &p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, &p, &mut extraction).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 9);
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    let [particular] = &outcome.particulars[..] else {
        panic!("one particular: {outcome:?}");
    };
    assert_eq!(
        (
            particular.subject_type.as_str(),
            particular.subject_local_ref.as_str()
        ),
        ("case", "l1:Issue 42")
    );
    assert!(
        matches!(&particular.subject_fields["number"], DocumentReadField::Supported { value, .. } if value == "42"),
        "{:?}",
        particular.subject_fields
    );
}

#[test]
fn validation_refuses_a_particular_of_a_type_resolve_does_not_decide_or_cited_off_its_document() {
    let p = policies();
    let chapter = input_chapter(&[row(1, "doc-1", "Issue 42 install times out\nThanks", "{}")]);
    let particular = |of: &str, line: &str| {
        json!({
            "subject_type": of, "subject_local_ref": format!("l1:{line}"),
            "subject_name": format!("{of} l1"), "evidence": line,
            "subject_fields": {
                "number": {"status": "unknown", "reason": "not stated"},
                "project": {"status": "unknown", "reason": "not stated"}
            }
        })
    };
    let envelope = json!({"documents": [{
        "document_id": "doc-1", "status": "read",
        "particulars": [
            particular("author", "Issue 42 install times out"),
            particular("case", "Issue 43 install hangs"),
            particular("case", "Issue 42 install times out"),
        ]
    }]})
    .to_string();
    let mut extraction = super::super::parse_response(&envelope, &p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, &p, &mut extraction).unwrap();
    let outcome = &extraction.document_read.as_ref().unwrap().documents[0];
    assert_eq!(outcome.particulars.len(), 1, "{outcome:?}");
    let reasons: Vec<&str> = outcome.refused.iter().map(|r| r.reason.as_str()).collect();
    assert!(
        reasons[0].contains("not a type RESOLVE decides")
            && reasons[1].contains("not an exact passage"),
        "{reasons:?}"
    );
    assert_eq!(
        extraction.claims.len(),
        1,
        "only the kept particular is projected"
    );
}

#[test]
fn the_count_reads_about_over_yes_against_no_then_one_against_several() {
    let dist = |one: f64, several: f64, no: f64| {
        [("A", one), ("B", several), ("0", no)]
            .into_iter()
            .map(|(l, p)| (l.to_string(), p))
            .collect::<std::collections::BTreeMap<String, f64>>()
    };
    // The two yeses outweigh no although no is the largest single label.
    assert_eq!(count_of(&dist(0.24, 0.29, 0.48)).0, Count::Several);
    assert_eq!(count_of(&dist(0.30, 0.25, 0.45)).0, Count::One);
    assert_eq!(count_of(&dist(0.16, 0.26, 0.58)).0, Count::NotAbout);
    assert_eq!(
        count_of(&dist(0.25, 0.25, 0.50)).0,
        Count::NotAbout,
        "a tie is not about"
    );
}

const DEALS: &str = "Deal one with Acme.\nDeal two with Brill.\nDeal three with Cato.\nDeal four with Dax.\nDeal five with Emo.\nDeal six with Fenn.\nDeal seven with Gale.";

/// About's questions answered by what they ask: the count as `count` gives it
/// (`None` refuses it: a distribution that leaves labels out), the next
/// particular's line as `line(k)` gives it with `k` already found, every word
/// question with its first word.
fn about_oracle(count: Option<&'static str>, line: fn(usize) -> &'static str) -> InferenceFn {
    let f: InferenceFn = std::sync::Arc::new(move |prompt: &ChatPrompt, _| {
        let labels: Vec<String> = prompt.response_schema.as_ref().unwrap()["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let u = &prompt.user;
        let answer = if u.contains("Is the document about a particular") {
            count.map_or_else(|| json!({ "A": 1.0 }), |c| pick(&labels, c))
        } else if u.contains("first named or described?") {
            pick(&labels, line(u.matches("\n- line ").count()))
        } else {
            pick(&labels, "A")
        };
        Box::pin(async move { Ok(answer.to_string()) })
    });
    crate::enrichment::atlas::resolve_records::one_by_one(f)
}

async fn about_deals(infer: &InferenceFn) -> (About, u32) {
    let p = policies();
    let of = p.shape.types.iter().find(|t| t.name == "case").unwrap();
    let lines = super::super::passes::lines(DEALS);
    let view = super::super::passes::numbered(DEALS, &lines);
    let d = Doc {
        infer,
        facts: "",
        document: "doc-1",
        body: DEALS,
        lines: &lines,
        headers: &[],
        view: &view,
    };
    let mut calls = 0;
    let mut abouts = about(&d, &[of], &mut calls).await;
    (abouts.remove(0), calls)
}

/// A refused count is not "about none": the document's particulars are still
/// pointed at, as for several, until none is chosen.
#[tokio::test]
async fn a_refused_count_is_unjudged_and_its_particulars_are_still_pointed_at() {
    let infer = about_oracle(None, |k| if k == 0 { "B" } else { "0" });
    let (about, _) = about_deals(&infer).await;
    assert_eq!(about.count, Count::Unjudged);
    let lines: Vec<usize> = about.particulars.iter().map(|p| p.line).collect();
    assert_eq!(lines, [2], "pointed at line 2, then none: {about:?}");
    assert!(about.complete, "it stopped where the document said it did");
}

/// Pointing that reaches its bound with no "none" chosen says the particulars
/// found are incomplete; one that stops at "none" says they are complete.
#[tokio::test]
async fn pointing_stopped_by_its_bound_is_incomplete_and_stopped_by_none_is_complete() {
    const AT: [&str; 7] = ["A", "B", "C", "D", "E", "F", "G"];
    let infer = about_oracle(Some("B"), |k| AT[k]);
    let (about, _) = about_deals(&infer).await;
    assert_eq!(about.count, Count::Several);
    assert_eq!(about.particulars.len(), MAX_PARTICULARS);
    assert!(
        !about.complete,
        "a sixth and seventh deal were never asked for"
    );
    let infer = about_oracle(Some("B"), |k| if k < 2 { AT[k] } else { "0" });
    let (about, _) = about_deals(&infer).await;
    assert_eq!(about.particulars.len(), 2);
    assert!(about.complete);
}
