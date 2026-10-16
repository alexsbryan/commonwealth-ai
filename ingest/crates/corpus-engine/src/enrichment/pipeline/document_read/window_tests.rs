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

/// A quote cut at a mail's line wrap is cited as its whole sentence, across
/// the wrap and no further; a price's decimal point ends no sentence; a quote
/// whose words are not in the text is left for validation to refuse.
#[test]
fn a_quote_is_widened_to_its_sentence_across_line_wraps() {
    use super::sentence_of;
    let text = "Thanks for the call.\nHartosn the Border piece is .50, subject to credit and\nKim's got the attached confirm. Talk soon.\n\nNext para.";
    assert_eq!(
        sentence_of(text, "Hartosn the Border piece is .50").as_deref(),
        Some("Hartosn the Border piece is .50, subject to credit and Kim's got the attached confirm.")
    );
    assert_eq!(
        sentence_of(text, "talk SOON.").as_deref(),
        Some("Talk soon.")
    );
    assert_eq!(sentence_of(text, "Next").as_deref(), Some("Next para."));
    assert_eq!(sentence_of(text, "a sentence the mail never says"), None);
}

/// A particular no statement of the window is about goes to RESOLVE as one
/// of its own, on the first document whose own lines name it; one no line
/// names is dropped. Failing input, named: saltgrass (2026-10-14), nine
/// characters named, four records.
#[tokio::test]
async fn an_unstated_particular_is_sent_on_the_line_that_names_it() {
    let policies = policies();
    let chapter = window();
    let answer = json!({
        "particulars": [
            {"ref": "p1", "type": "case", "name": "issue 42", "fields": {}},
            {"ref": "p2", "type": "case", "name": "in uv", "fields": {}},
            {"ref": "p3", "type": "case", "name": "issue 99", "fields": {}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "fields": {"status": "Closed"}}]});
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
        panic!("one outcome per document: {:?}", read.documents)
    };
    assert_eq!(a.particulars.len(), 1, "{:?}", a.particulars);
    assert_eq!(a.particulars[0].subject_local_ref, "sec_1/p2");
    assert_eq!(a.particulars[0].evidence, "Issue 42 in uv is open.");
    assert!(
        b.particulars.is_empty(),
        "p1 has a statement; p3 is named by no line"
    );
}

/// The window names particulars of every type RESOLVE decides, a statement
/// kind about it or not, after the kinds' subjects; the prompt shows each.
#[test]
fn the_window_names_every_decided_type_a_statement_is_about_or_not() {
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
    let contract = super::Contract::of(&p);
    let subjects: Vec<&str> = contract.subjects.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(subjects, ["person", "case"]);
    let index = crate::enrichment::ontology::TypeIndex::from_policies(&p);
    let shown = super::ontology_text(&contract, &p, &index);
    assert!(
        shown.contains("- case: A support case.\n  Same case when: same issue number"),
        "{shown}"
    );
}

/// Failing input, named: saltgrass-asked's window wrote `speaker: "p6"`, the
/// label it gave a particular, and every such reading was unknown. A label of
/// a particular of the reference's type reads as that particular's name; a
/// label naming no particular, or one of another type, stays unknown.
#[tokio::test]
async fn a_window_ref_label_reads_as_its_particulars_name() {
    let ontology: crate::enrichment::ontology::OntologyV1 = toml::from_str(
        r#"
[[types]]
name = "case"
kind = "entity"
identity_criterion = "same issue number"
identity = ["number"]

[[types.attributes]]
name = "number"
type = "text"

[[types]]
name = "person"
kind = "entity"

[[types]]
name = "firm"
kind = "entity"

[[types]]
name = "remark"
kind = "claim"
force = "assertive"
subject = "case"
attributes = [{ name = "speaker", type = "ref", of = "person" }]
"#,
    )
    .unwrap();
    let policies = ontology.into_policies();
    let chapter = input_chapter(&[
        row(1, "doc-a", "Issue 42 is open, she said.", "{}"),
        row(2, "doc-b", "Issue 42 is still open.", "{}"),
        row(3, "doc-c", "Issue 42 stays open.", "{}"),
    ]);
    let remark = |document: &str, quote: &str, speaker: &str| {
        json!({"kind": "remark", "about": "p1", "document": document, "quote": quote,
               "fields": {"speaker": speaker}})
    };
    let answer = json!({
        "particulars": [
            {"ref": "p1", "type": "case", "name": "issue 42", "fields": {}},
            {"ref": "p2", "type": "person", "name": "Mara Vell", "fields": {}},
            {"ref": "p3", "type": "firm", "name": "Acme", "fields": {}}],
        "claims": [
            remark("d1", "Issue 42 is open, she said.", "p2"),
            remark("d2", "Issue 42 is still open.", "p9"),
            remark("d3", "Issue 42 stays open.", "p3")]});
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
    let speakers: Vec<&DocumentReadField> = read
        .documents
        .iter()
        .map(|d| &d.claims[0].fields["speaker"])
        .collect();
    let [named, none, firm] = speakers.as_slice() else {
        panic!("{:?}", read.documents)
    };
    assert!(
        matches!(named, DocumentReadField::Supported { value, evidence, .. }
            if value == &json!("Mara Vell") && evidence == "Issue 42 is open, she said."),
        "{named:?}"
    );
    assert!(
        matches!(none, DocumentReadField::Unknown { reason } if reason.contains("\"p9\" is neither a person")),
        "{none:?}"
    );
    assert!(
        matches!(firm, DocumentReadField::Unknown { reason } if reason.contains("\"p3\" is neither a person")),
        "{firm:?}"
    );
}

/// Failing input, named: a local model's window answer wrote a price as
/// `"value":.02` (runs/loop/local35b-s4-think-w10k, window 5). JSON requires
/// the leading zero, so the strict parse refused the whole window. The number
/// is read as written, counted; text inside a string is never rewritten.
#[test]
fn a_bare_fraction_reads_as_its_number() {
    let parsed = super::answer::parse(
        r#"{"particulars": [{"ref": "p1", "fields": {"amount": {"value": .02, "quote": "at .02"},
            "low": {"value":-.5, "quote": "down .5 \".5\""}, "list": [.1, 2]}}], "claims": []}"#,
        "sec_1",
    )
    .unwrap();
    let f = &parsed.value["particulars"][0]["fields"];
    assert_eq!(f["amount"]["value"], json!(0.02));
    assert_eq!(f["low"]["value"], json!(-0.5));
    assert_eq!(f["list"], json!([0.1, 2]));
    assert_eq!(f["amount"]["quote"], json!("at .02"));
    assert_eq!(f["low"]["quote"], json!("down .5 \".5\""));
    assert_eq!((parsed.repaired, parsed.dropped), (3, 0));
}

/// Failing input, named: one element of a window answer that is not JSON
/// (a missing comma) failed the strict parse and lost every statement of the
/// window. Each element is parsed on its own: the malformed particular and
/// the malformed claim are dropped and counted, a claim about the dropped
/// particular goes with it, and the rest of the window is read.
#[tokio::test]
async fn one_malformed_element_costs_only_itself() {
    use std::sync::Arc;
    let raw = r#"{"particulars": [
        {"ref": "p1", "type": "case", "name": "issue 42", "fields": {}},
        {"ref": "p2", "type": "case" "name": "issue 7", "fields": {}}],
      "claims": [
        {"kind": "membership", "about": "p1", "document": "d1", "quote": "Issue 42 in uv is open.", "fields": {}},
        {"kind": "reported_status", "about": "p2", "document": "d2", "quote": "Issue 42 is now closed.",
         "fields": {"status": "Closed"}},
        {"kind": "reported_status", "about": "p1", "document": "d2" "quote": "Issue 42 is now closed."}]}"#;
    let parsed = super::answer::parse(raw, "sec_1").unwrap();
    assert_eq!(
        parsed.value["particulars"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(parsed.value["claims"].as_array().map(Vec::len), Some(1));
    assert_eq!((parsed.repaired, parsed.dropped), (0, 3));

    let policies = policies();
    let chapter = window();
    let infer: crate::InferenceFn =
        Arc::new(move |_, _| Box::pin(async move { Ok(raw.to_string()) }));
    let out = read(&chapter, &policies, &Default::default(), &infer)
        .await
        .expect("the window is read without its malformed elements");
    let mut extraction = parse_response(&out, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let read = extraction.document_read.as_ref().unwrap();
    assert_eq!(read.documents[0].claims.len(), 1, "{:?}", read.documents);
    assert!(read.documents[1].claims.is_empty(), "{:?}", read.documents);
    assert!(
        read.documents.iter().all(|d| d.refused.is_empty()),
        "a dropped element is lost, not refused: {:?}",
        read.documents
    );
}

/// An answer with no array to read element by element fails the window as
/// before, with the parse error.
#[test]
fn an_answer_with_no_readable_array_still_fails() {
    assert!(super::answer::parse(r#"{"particulars": "none" "claims"}"#, "sec_1").is_err());
}

/// Failing input, named: replies whose quoted history named the deal they
/// answer ("redline", "the Waha piece") were shown with that history hidden
/// and nothing in its place, so the reader could not tell which deal they
/// answer and split or merged them (world-s4 faircroft-d2, world-s3
/// velica-d3; arm c123-verify, 2026-10-15). Hidden quoted lines are replaced
/// by a pointer to the document they come from: its label in the window, or
/// its date outside it. The quoted text stays unshown and uncitable, and a
/// claim's sentence never takes in the pointer.
#[tokio::test]
async fn a_reply_points_to_the_document_it_quotes() {
    let mut policies = policies();
    policies.change.document = Some(crate::enrichment::ontology::DocumentFieldsDecl {
        date: Some("date".into()),
        thread: None,
        id: None,
        author: None,
    });
    let both = input_chapter(&[
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
    let classes = super::super::LineClasses::of(&both.source_documents, &policies);
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [{"kind": "reported_status", "about": "p1", "document": "d2",
                    "quote": "Issue 42 is now closed.", "fields": {"status": "Closed"}}]});
    let (infer, seen) = scripted(vec![answer]);
    let raw = read(&both, &policies, &classes, &infer).await.unwrap();
    let user = seen.lock().unwrap()[0].user.clone();
    let d2 = &user[user.find("=== d2 ===").unwrap()..];
    assert!(
        d2.contains("Issue 42 is now closed.\n[quoted from d1]"),
        "{d2}"
    );
    assert!(!d2.contains("in uv is open"), "{d2}");
    assert_eq!(
        d2.matches("[quoted from").count(),
        1,
        "one pointer per run: {d2}"
    );
    assert!(
        !user.contains("answers d") && !user.contains("[quotes d3]"),
        "the pointer says what it is; no prompt line explains it or assumes a reply"
    );
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&both, &policies, &mut extraction).unwrap();
    let read_b = &extraction.document_read.as_ref().unwrap().documents[1];
    assert_eq!(read_b.claims[0].evidence, "Issue 42 is now closed.");

    let alone = input_chapter(&[row(
        2,
        "doc-b",
        "Issue 42 is now closed.\n> Issue 42 in uv is open.\n> It fails on start.",
        r#"{"date":"Tue, 15 May 2001 09:00:00 -0700"}"#,
    )]);
    let (infer, seen) = scripted(vec![json!({"particulars": [], "claims": []})]);
    read(&alone, &policies, &classes, &infer).await.unwrap();
    let user = seen.lock().unwrap()[0].user.clone();
    assert!(
        user.contains("[quoted from a document of 2001-05-14T16:00:00Z, outside this window]"),
        "{user}"
    );
}

/// Failing input, named: a reply's quoted history is hidden from the reader,
/// but a claim citing it passed through `envelope` unchanged and validation
/// found the quote in the raw body, so a January reply could still be read
/// as saying October's "gone with another supplier" (note 32e3ff3d). A claim
/// whose quote is in the lines a document quotes or embeds, and not in its
/// own, is refused by name; one in its own lines is read; one found nowhere
/// is left for validation to refuse.
#[tokio::test]
async fn a_claim_citing_lines_its_document_quotes_is_refused() {
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
    let classes = super::super::LineClasses::of(&chapter.source_documents, &policies);
    let answer = json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 42", "fields": {}}],
        "claims": [
            {"kind": "reported_status", "about": "p1", "document": "d2",
             "quote": "Issue 42 in uv is open.", "fields": {"status": "Open"}},
            {"kind": "reported_status", "about": "p1", "document": "d2",
             "quote": "Issue 42 is now closed.", "fields": {"status": "Closed"}},
            {"kind": "reported_status", "about": "p1", "document": "d2",
             "quote": "Issue 42 was reopened.", "fields": {"status": "Open"}}]});
    let (infer, _) = scripted(vec![answer]);
    let raw = read(&chapter, &policies, &classes, &infer).await.unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();
    let d2 = &extraction.document_read.as_ref().unwrap().documents[1];
    let read: Vec<&str> = d2.claims.iter().map(|c| c.evidence.as_str()).collect();
    assert_eq!(read, ["Issue 42 is now closed."], "{:?}", d2.refused);
    let reasons: Vec<&str> = d2.refused.iter().map(|r| r.reason.as_str()).collect();
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("quotes or embeds") && r.contains("not its own")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.contains("not an exact passage")),
        "a quote found nowhere is validation's to refuse: {reasons:?}"
    );
}
