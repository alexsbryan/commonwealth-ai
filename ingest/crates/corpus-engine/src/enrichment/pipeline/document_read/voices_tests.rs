// SPDX-License-Identifier: AGPL-3.0-or-later
//! A declared voice is never a particular or a subject under the document
//! reader, as it is never an entity under the general extractor.

use super::*;

fn particular(local_ref: &str, name: &str, words: Option<&str>) -> DocumentReadParticular {
    DocumentReadParticular {
        subject_type: "case".into(),
        subject_local_ref: local_ref.into(),
        subject_name: name.into(),
        evidence: "The narrator writes: Issue 42 in uv is open.".into(),
        subject_fields: ["number", "project"]
            .map(|f| {
                let reason = "not stated".to_string();
                (f.to_string(), DocumentReadField::Unknown { reason })
            })
            .into(),
        words: words.map(str::to_string),
        other_names: Default::default(),
    }
}

#[tokio::test]
async fn a_declared_voice_is_never_a_particular_or_a_subject() {
    let mut policies = policies();
    policies.assertion.voices.not_entities = vec!["The Narrator".into()];
    let chapter = input_chapter(&[
        row(
            1,
            "doc-a",
            "The narrator writes: Issue 42 in uv is open.",
            r#"{"author":"ana"}"#,
        ),
        row(2, "doc-b", "Issue 42 is now closed.", r#"{"author":"bo"}"#),
    ]);
    let answer = json!({
        "particulars": [
            {"ref": "p1", "type": "case", "name": "issue 42", "fields": {}},
            {"ref": "p2", "type": "case", "name": "the narrator", "fields": {}}],
        "claims": [
            {"kind": "membership", "about": "p1", "document": "d1", "quote": "Issue 42 in uv is open.", "fields": {}},
            {"kind": "reported_status", "about": "p2", "document": "d2", "quote": "Issue 42 is now closed.",
             "fields": {"status": "closed"}}]});
    let (infer, _) = scripted(vec![answer]);
    let raw = super::super::window::read(&chapter, &policies, &Default::default(), &infer)
        .await
        .unwrap();
    let mut extraction = parse_response(&raw, &policies)
        .unwrap()
        .section_extraction
        .unwrap();
    // Particulars in each form validation reads a voice in: an `l{n}:`
    // reference, a mention (its words), and one that is no voice.
    extraction.document_read.as_mut().unwrap().documents[0]
        .particulars
        .extend([
            particular("l1:The narrator", "the one telling it", None),
            particular("m1", "the one telling it", Some("The narrator")),
            particular("l1:Issue 42", "issue 42", None),
        ]);
    // A read cached before voices were enforced holds all of it.
    let cached = extraction.clone();
    validate_and_stamp(&chapter, &policies, &mut extraction).unwrap();

    let read = extraction.document_read.as_ref().unwrap();
    let [a, b] = read.documents.as_slice() else {
        panic!("{:?}", read.documents)
    };
    let names: Vec<&str> = a
        .claims
        .iter()
        .chain(&b.claims)
        .map(|c| c.subject_name.as_str())
        .chain(a.particulars.iter().map(|p| p.subject_name.as_str()))
        .collect();
    assert_eq!(names, ["issue 42", "issue 42"], "{read:#?}");
    let voiced = |o: &DocumentReadOutcome| {
        o.refused
            .iter()
            .filter(|r| {
                r.reason
                    .contains("is a declared voice (voices.not_entities)")
            })
            .count()
    };
    assert_eq!((voiced(a), voiced(b)), (2, 1), "{read:#?}");
    assert_eq!(b.status, DocumentReadStatus::CouldNotJudge);
    let reason = b.reason.as_deref().unwrap();
    assert!(!reason.contains("citation did not verify"), "{reason}");
    let subjects: Vec<Option<&str>> = extraction
        .claims
        .iter()
        .map(|c| c.subject.as_deref())
        .collect();
    assert_eq!(
        subjects,
        [Some("issue 42"); 2],
        "the claim and the kept particular"
    );
    assert!(extraction
        .entities_introduced
        .iter()
        .all(|e| !e.canonical_name.to_lowercase().contains("narrator")));

    let mut revalidated = cached;
    validate_and_stamp(&chapter, &policies, &mut revalidated).unwrap();
    assert_eq!(revalidated.document_read, extraction.document_read);
    assert!(cache_matches_chapter(&chapter, &extraction, &policies).is_ok());
}
