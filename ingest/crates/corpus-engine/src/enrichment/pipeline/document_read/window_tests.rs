use serde_json::{json, Value};

use super::super::tests::{input_chapter, policies, row, scripted};
use super::super::{
    parse_response, validate_and_stamp, DocumentReadField, DocumentReadStatus, SubjectScope,
    CHAPTER_SUBJECT_ATTRIBUTE,
};
use super::{read, WINDOW_PHASE};

fn value(field: &DocumentReadField) -> Option<&Value> {
    match field {
        DocumentReadField::Supported { value, .. } => Some(value),
        DocumentReadField::Unknown { .. } => None,
    }
}

/// Two documents of one window: an issue opened in uv, then closed.
fn window() -> crate::enrichment::pipeline::types::ChapterInput {
    input_chapter(&[
        row(1, "doc-a", "Issue 42 in uv is open.", r#"{"author":"ana"}"#),
        row(2, "doc-b", "Issue 42 is now closed.", r#"{"author":"bo"}"#),
    ])
}

/// One particular read across two documents reaches the envelope as one
/// chapter-wide reference on both claims, survives parse and validation, and
/// each field is supported only by what its claim's own document holds.
#[tokio::test]
async fn one_particular_across_a_window_is_one_chapter_subject() {
    let policies = policies();
    let chapter = window();
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {
            "number": {"value": "42", "document": "d1", "quote": "Issue 42"},
            "project": {"value": "UV", "document": "d1", "quote": "in uv"}}}],
        "claims": [
            {"kind": "membership", "about": "p1", "document": "d1", "quote": "Issue 42 in uv is open.", "fields": {}},
            {"kind": "reported_status", "about": "p1", "document": "d2", "quote": "Issue 42 is now closed.",
             "fields": {"status": "Closed"}}]});
    let (infer, seen) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();

    let read = extraction.document_read.as_ref().unwrap();
    assert_eq!(read.subjects, SubjectScope::Chapter);
    let [a, b] = read.documents.as_slice() else {
        panic!("one outcome per document: {:?}", read.documents)
    };
    assert_eq!((a.claims.len(), b.claims.len()), (1, 1));
    assert_eq!(a.claims[0].subject_local_ref, "sec_1/p1");
    assert_eq!(b.claims[0].subject_local_ref, "sec_1/p1");
    let closed = &b.claims[0];
    assert_eq!(
        value(&closed.fields["status"]),
        Some(&json!("closed")),
        "a declared value, as declared"
    );
    assert_eq!(
        value(&closed.subject_fields["number"]),
        Some(&json!("42")),
        "its words are in d2"
    );
    assert_eq!(
        value(&closed.subject_fields["project"]),
        None,
        "read in d1 only"
    );
    assert_eq!(
        value(&a.claims[0].subject_fields["project"]),
        Some(&json!("uv"))
    );
    assert!(extraction
        .claims
        .iter()
        .all(|c| c.attributes[CHAPTER_SUBJECT_ATTRIBUTE] == json!("sec_1/p1")));

    let prompt = &seen.lock().unwrap()[0];
    assert_eq!(prompt.phase_id.as_deref(), Some(WINDOW_PHASE));
    assert!(
        prompt.user.contains("=== d2 ===\nauthor: bo\n"),
        "{}",
        prompt.user
    );
    assert!(prompt.user.contains("Same case when: same issue number"));
    assert!(prompt.user.contains("One of: open | closed."));
}

/// What the model wrote that the contract cannot hold is refused by name or
/// left unknown with a reason, never projected and never defaulted.
#[tokio::test]
async fn what_the_contract_cannot_hold_is_refused_or_unknown_by_name() {
    let policies = policies();
    let chapter = window();
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [
            {"kind": "reported_status", "about": "p9", "document": "d1", "quote": "Issue 42 in uv is open."},
            {"kind": "guess", "about": "p1", "document": "d1", "quote": "Issue 42 in uv is open."},
            {"kind": "membership", "about": "p1", "document": "d7", "quote": "Issue 42 in uv is open."},
            {"kind": "reported_status", "about": "p1", "document": "d2", "quote": "Issue 42 is now closed.",
             "fields": {"status": "maybe"}}]});
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
    let [a, b] = read.documents.as_slice() else {
        panic!("{:?}", read.documents)
    };
    assert_eq!(a.status, DocumentReadStatus::CouldNotJudge);
    let reasons: Vec<&str> = a.refused.iter().map(|r| r.reason.as_str()).collect();
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("`p9`, which names no particular")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("`guess` is not a statement kind")),
        "{reasons:?}"
    );
    assert_eq!(b.status, DocumentReadStatus::Read);
    assert!(
        matches!(&b.claims[0].fields["status"], DocumentReadField::Unknown { reason } if reason.contains("none of the declared values")),
        "{:?}",
        b.claims[0].fields
    );
}

/// Failing input, named: world-s4's January reply quotes October's "gone with
/// another supplier", and a window that shows the quote reads the deal lost in
/// January. Each document shows only its own lines (`line_classes.rs`).
#[tokio::test]
async fn a_reply_shows_only_its_own_lines_not_the_message_it_quotes() {
    let mut policies = policies();
    policies.change.document = Some(crate::enrichment::ontology::DocumentFieldsDecl {
        date: Some("date".into()),
        thread: None,
        id: None,
        author: None,
    });
    let chapter = input_chapter(&[
        row(
            1,
            "doc-a",
            "Issue 42 in uv is open.\nIt fails on start.",
            r#"{"date":"Mon, 14 May 2001 09:00:00 -0700"}"#,
        ),
        row(
            2,
            "doc-b",
            "Issue 42 is now closed.\n> Issue 42 in uv is open.\n> It fails on start.",
            r#"{"date":"Tue, 15 May 2001 09:00:00 -0700"}"#,
        ),
    ]);
    let empty = json!({"particulars": [], "claims": []});
    let shown = |classes: super::super::LineClasses| {
        let (policies, chapter, empty) = (policies.clone(), chapter.clone(), empty.clone());
        async move {
            let (infer, seen) = scripted(vec![empty]);
            read(&chapter, &policies, &classes, &infer).await.unwrap();
            let user = seen.lock().unwrap()[0].user.clone();
            user[user.find("=== d2 ===").unwrap()..].to_string()
        }
    };
    let control = shown(Default::default()).await;
    assert!(
        control.contains("> Issue 42 in uv is open."),
        "control: no classes, the quote is shown"
    );
    let d2 = shown(super::super::LineClasses::of(
        &chapter.source_documents,
        &policies,
    ))
    .await;
    assert!(d2.contains("Issue 42 is now closed."), "{d2}");
    assert!(
        !d2.contains("in uv is open"),
        "the carried lines are not shown: {d2}"
    );
}

/// The reader asks the model to reason first, and reads the answer that
/// follows the reasoning: a reasoning engine opens its answer with a
/// `<think>` block, as every window read now does.
#[tokio::test]
async fn a_window_read_reasons_first_and_reads_what_follows_the_reasoning() {
    use std::sync::{Arc, Mutex};
    let policies = policies();
    let chapter = window();
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "fields": {"status": "Closed"}}]});
    let raw = format!(
        "<think>\nd2 says issue 42 is closed. {{\"not\": \"the answer\"}}\n</think>\n\n{answer}"
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    let infer: crate::InferenceFn = Arc::new(
        move |p: &crate::enrichment::pipeline::types::ChatPrompt, _| {
            kept.lock().unwrap().push(p.clone());
            let raw = raw.clone();
            Box::pin(async move { Ok(raw) })
        },
    );
    let out = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let mut extraction = parse_response(&out, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let read = extraction.document_read.as_ref().unwrap();
    assert_eq!(read.documents[1].claims.len(), 1, "{:?}", read.documents);

    let prompt = &seen.lock().unwrap()[0];
    let thinking = prompt
        .thinking_tokens
        .expect("the reader sets its reasoning budget");
    let answer_cap = prompt
        .max_output_tokens
        .expect("the reader sets its answer cap");
    assert!(thinking > 0, "the reader reasons");
    assert!(
        answer_cap >= thinking + 16_000,
        "the reasoning leaves the JSON room: {thinking} of {answer_cap}"
    );
}
