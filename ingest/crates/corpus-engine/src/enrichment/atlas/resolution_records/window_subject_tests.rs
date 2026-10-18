//! A window read's chapter subject reaching RESOLVE across two documents
//! (`declared_document_read_tests` fixtures).

use super::*;

/// The issue and the label event, read as one window, each saying something of
/// one case with no declared key: the case is one record only when the
/// window's chapter subject reaches RESOLVE.
async fn resolve_window(chapter: bool, answerer: Answerer<'_>) -> (Vec<Claim>, Vec<RecordsReport>) {
    let (_, mut policies) = local_subject_fixture();
    policies.shape.types[0].identity.clear();
    let tag = |mut claim: Claim| {
        if chapter {
            claim
                .attributes
                .insert(CHAPTER_SUBJECT_ATTRIBUTE.into(), json!("sec_1/p1"));
        }
        claim
    };
    let mut claims = vec![
        tag(local_subject_claim(
            "membership",
            "case_membership",
            "issue",
            ISSUE,
            "1373",
        )),
        tag(local_subject_claim(
            "label",
            "case_membership",
            "label-event",
            "Label compatibility added to #1373.",
            "1373",
        )),
    ];
    let mut entities = Vec::new();
    let (reports, _) = resolve_fixture_by(&mut claims, &mut entities, &policies, answerer).await;
    (claims, reports)
}

/// Failing input, named: without the chapter subject the label event's
/// statement is RESOLVE's to decide, and with no key and no model it does not
/// land on the issue's record. With it, the window reader's reading of the two
/// documents as one case reaches RESOLVE as evidence (lean D, 2026-10-17): the
/// label event is asked, the issue's record offered though no proposer offered
/// it, marked as the reader's reading, and the model's pick joins them.
#[tokio::test]
async fn a_window_reads_one_subject_across_two_documents_and_resolve_is_asked() {
    use std::sync::{Arc, Mutex};
    let (claims, reports) = resolve_window(false, Answerer::Proposed).await;
    assert!(
        claims[1].subject.is_none() || claims[0].subject != claims[1].subject,
        "control: no key, no model, no link"
    );
    assert_eq!(reports[0].statements, 2);

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let kept = seen.clone();
    let infer: crate::types::InferenceFn = Arc::new(move |p, _| {
        if p.phase_id.as_deref() == Some("resolve_select") {
            kept.lock().unwrap().push(p.user.clone());
        }
        Box::pin(async move { Ok(json!({"A": 0.9, "0": 0.1}).to_string()) })
    });
    let (claims, reports) = resolve_window(true, Answerer::Select(&infer)).await;
    assert!(claims[0].subject.is_some());
    assert_eq!(
        claims[0].subject, claims[1].subject,
        "one case across the window"
    );
    assert_eq!(reports[0].records, 1);
    let asked = seen.lock().unwrap();
    assert_eq!(asked.len(), 1, "the label event is asked: {asked:?}");
    assert!(
        asked[0].contains("read this record's particular as this statement's"),
        "{}",
        asked[0]
    );
    assert!(
        claims
            .iter()
            .all(|c| !c.attributes.contains_key(CHAPTER_SUBJECT_ATTRIBUTE)),
        "the handoff attribute does not outlive RESOLVE"
    );
}
