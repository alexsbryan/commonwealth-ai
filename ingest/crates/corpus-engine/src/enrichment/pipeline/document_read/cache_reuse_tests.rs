// SPDX-License-Identifier: AGPL-3.0-or-later
//! A cached document read is replayed only while author, body and READ
//! contract are unchanged.

use super::*;

#[tokio::test]
async fn cached_document_read_replays_without_calling_inference() {
    let _passes = super::super::pin(super::super::Reader::Passes);
    use std::sync::{atomic::Ordering, Arc};

    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        r#"{"author":"Alice"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let temp = tempfile::tempdir().unwrap();
    let (first_chat, calls) = counted(CLOSED);
    reader_runner(temp.path(), &policies, first_chat, true)
        .phase_1_extract_questions(
            &[chapter.clone()],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 7);

    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("identical accountable read must use the section cache"));
    let replay = reader_runner(temp.path(), &policies, panic_chat, true)
        .phase_1_extract_questions(
            &[chapter],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(replay.output.questions_by_chapter.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 7);

    let mut identity_policy = policies.clone();
    identity_policy
        .identity
        .identity
        .insert("case".into(), vec!["number".into(), "external_key".into()]);
    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("identity-policy changes must replay the retained read"));
    let replay = reader_runner(temp.path(), &identity_policy, panic_chat, true)
        .phase_1_extract_questions(
            &[input_chapter(&rows)],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(replay.output.questions_by_chapter.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 7);
}

#[tokio::test]
#[should_panic(expected = "changed author must reject cached accountable read")]
async fn changed_author_does_not_reuse_a_cached_document_read() {
    let _passes = super::super::pin(super::super::Reader::Passes);
    use std::sync::Arc;

    let body = "Issue 842 was closed by pull request #1380.";
    let original = [row(1, "doc-a", body, r#"{"author":"Alice"}"#)];
    let changed = [row(1, "doc-a", body, r#"{"author":"Bob"}"#)];
    let policies = policies();
    let temp = tempfile::tempdir().unwrap();
    let (first, _) = counted(CLOSED);
    reader_runner(temp.path(), &policies, first, true)
        .phase_1_extract_questions(
            &[input_chapter(&original)],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();

    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("changed author must reject cached accountable read"));
    let _ = reader_runner(temp.path(), &policies, panic_chat, true)
        .phase_1_extract_questions(
            &[input_chapter(&changed)],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await;
}

#[tokio::test]
#[should_panic(expected = "changed body must reject cached accountable read")]
async fn changed_body_does_not_reuse_a_cached_document_read() {
    let _passes = super::super::pin(super::super::Reader::Passes);
    use std::sync::Arc;

    let metadata = r#"{"author":"Alice"}"#;
    let original = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        metadata,
    )];
    let changed = [row(
        1,
        "doc-a",
        "Issue 842 was reopened by pull request #1380.",
        metadata,
    )];
    let policies = policies();
    let temp = tempfile::tempdir().unwrap();
    let (first, _) = counted(CLOSED);
    reader_runner(temp.path(), &policies, first, true)
        .phase_1_extract_questions(
            &[input_chapter(&original)],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();

    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("changed body must reject cached accountable read"));
    let _ = reader_runner(temp.path(), &policies, panic_chat, true)
        .phase_1_extract_questions(
            &[input_chapter(&changed)],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await;
}

#[tokio::test]
#[should_panic(expected = "changed READ contract must reject cached accountable read")]
async fn changed_read_contract_does_not_reuse_a_cached_document_read() {
    let _passes = super::super::pin(super::super::Reader::Passes);
    use std::sync::Arc;

    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        r#"{"author":"Alice"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let temp = tempfile::tempdir().unwrap();
    let (first, _) = counted(CLOSED);
    reader_runner(temp.path(), &policies, first, true)
        .phase_1_extract_questions(
            &[chapter.clone()],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    let mut changed = policies;
    changed
        .prose
        .guidance
        .push_str(" Read every membership carefully.");
    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("changed READ contract must reject cached accountable read"));
    let _ = reader_runner(temp.path(), &changed, panic_chat, true)
        .phase_1_extract_questions(
            &[chapter],
            &crate::enrichment::pipeline::ChapterSelection::Full,
            |_| {},
        )
        .await;
}
