use std::collections::HashMap;

use serde_json::{json, Value};

use super::*;
use crate::enrichment::atlas::SectionDocuments;
use crate::enrichment::ontology::{OntologyPolicies, OntologyV1};
use crate::enrichment::pipeline::atlas::SectionExtraction;
use crate::enrichment::pipeline::runner::Phase1CheckpointEntry;
use crate::enrichment::pipeline::types::ChapterInput;

#[path = "document_read/claim_check_tests.rs"]
mod claim_check_tests;

#[path = "document_read/citation_tests.rs"]
mod citation_tests;

pub(super) fn policies() -> OntologyPolicies {
    let ontology: OntologyV1 = toml::from_str(
        r#"
document_reading = true
guidance = "Read issue and case assertions."

[[types]]
name = "case"
kind = "entity"
description = "A support case."
identity_criterion = "same issue number"
identity = ["number"]
identity_necessary = ["project"]

[[types.attributes]]
name = "number"
type = "text"

[[types.attributes]]
name = "project"
type = "text"
values = ["uv", "pip"]

[[types]]
name = "author"
kind = "entity"
source = { metadata = ["author"] }

[[types]]
name = "membership"
kind = "claim"
force = "assertive"
subject = "case"

[[types]]
name = "reported_status"
kind = "claim"
force = "assertive"
subject = "case"

[[types.attributes]]
name = "status"
type = "text"
values = ["open", "closed"]
"#,
    )
    .unwrap();
    ontology.into_policies()
}

pub(super) fn row(
    id: u64,
    source_doc_id: &str,
    body: &str,
    metadata: &str,
) -> corpus_index::index::EnrichmentChunkRow {
    corpus_index::index::EnrichmentChunkRow {
        id,
        content: body.to_string(),
        title: Some("Issue thread".into()),
        url: Some(format!("https://example.test/{source_doc_id}")),
        metadata_raw: Some(metadata.to_string()),
        source_doc_id: Some(source_doc_id.to_string()),
    }
}

pub(super) fn input_chapter(rows: &[corpus_index::index::EnrichmentChunkRow]) -> ChapterInput {
    let ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
    let sections = [("sec_1", ids.as_slice())];
    let documents = SectionDocuments::from_chunk_rows(sections, rows);
    let body = rows
        .iter()
        .map(|row| row.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    ChapterInput {
        chapter_id: "sec_1".into(),
        title: "Issue thread".into(),
        approx_tokens: body.len() / 4,
        text: body,
        metadata: HashMap::new(),
        source_documents: documents.documents_for_section("sec_1").to_vec(),
    }
}

fn claim(kind: &str, local_ref: &str, name: &str, evidence: &str, status: &str) -> Value {
    let fields = if kind == "reported_status" {
        json!({
            "status": {
                "status": status,
                "value": "closed",
                "evidence": evidence,
            }
        })
    } else {
        json!({})
    };
    json!({
        "kind": kind,
        "content": format!("{name} is discussed in this document."),
        "subject_type": "case",
        "subject_local_ref": local_ref,
        "subject_name": name,
        "speaker": null,
        "evidence": evidence,
        "fields": fields,
        "subject_fields": {
            "number": {"status":"supported", "value":"842", "evidence":"842"},
            "project": {"status":"unknown", "reason":"the document does not name a project"}
        }
    })
}

fn response(documents: Value) -> String {
    json!({"documents": documents}).to_string()
}

fn one_document_read() -> String {
    response(json!([{
        "document_id":"doc-a",
        "status":"read",
        "claims":[claim(
            "membership",
            "case-842",
            "Case 842",
            "Issue 842 was closed by pull request #1380.",
            "unknown"
        )]
    }]))
}

fn reader_runner(
    root: &std::path::Path,
    policies: &OntologyPolicies,
    chat: crate::types::InferenceFn,
    use_section_cache: bool,
) -> super::super::runner::PhaseRunner {
    use super::super::pipelines::configurable_atlas::CustomAtlasSpec;
    use super::super::pipelines::literary_atlas::LiteraryAtlasPipeline;
    use super::super::runner::PhaseRunner;
    use super::super::{PhaseCache, RunOutputWriter};
    use std::sync::Arc;

    let spec = CustomAtlasSpec {
        name: "test cases".into(),
        guidance: policies.prose.guidance.clone(),
        vocabulary: None,
        ontology_version: 1,
        policies: Some(policies.clone()),
    };
    let pipeline = Arc::new(LiteraryAtlasPipeline::with_custom_ontology(&spec));
    let embed: crate::types::EmbedFn =
        Arc::new(|_| panic!("declared document reading must not embed exemplars"));
    let runner = PhaseRunner::new(
        pipeline,
        embed,
        chat,
        PhaseCache::new(root.join("cache")),
        RunOutputWriter::new(root.join("runs")),
        root.join("exemplars"),
    )
    .with_min_body_words(0);
    if use_section_cache {
        runner.with_section_cache(root.join("atlas"), "mock-model", "read-test-v1")
    } else {
        runner
    }
}

#[tokio::test]
async fn production_phase1_uses_the_mock_provider_with_actual_document_context() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        r#"{"author":"Alice","role":"member","date":"2024-02-03","kind":"comment","id":"doc-a"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let calls = Arc::new(AtomicUsize::new(0));
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let called = calls.clone();
    let captured = prompts.clone();
    let answer = one_document_read();
    let chat: crate::types::InferenceFn = Arc::new(move |prompt, _| {
        called.fetch_add(1, Ordering::SeqCst);
        captured.lock().unwrap().push(prompt.clone());
        let answer = answer.clone();
        Box::pin(async move { Ok(answer) })
    });
    let temp = tempfile::tempdir().unwrap();
    let runner = reader_runner(temp.path(), &policies, chat, false);
    let result = runner
        .phase_1_extract_questions(
            &[chapter.clone()],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one chapter batch is one provider call"
    );
    assert!(result.output.questions_by_chapter[0].questions.is_empty());
    let read = result.output.questions_by_chapter[0]
        .section_extraction
        .as_ref()
        .unwrap()
        .document_read
        .as_ref()
        .unwrap();
    assert_eq!(read.documents.len(), 1);
    assert!(
        result.output.questions_by_chapter[0]
            .section_extraction
            .as_ref()
            .unwrap()
            .claims[0]
            .attributed_to
            .is_none(),
        "metadata.author is not substituted for quoted speaker"
    );
    let prompts = prompts.lock().unwrap();
    let prompt = &prompts[0];
    assert_eq!(
        prompt.response_schema_name.as_deref(),
        Some("declared_document_read")
    );
    assert_eq!(
        prompt.response_schema.as_ref().unwrap()["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["documents"],
        "dedicated schema carries no generic atlas facets"
    );
    assert!(
        !prompt
            .response_schema
            .as_ref()
            .unwrap()
            .to_string()
            .contains("author"),
        "metadata-source entities are reference context only"
    );
    assert!(prompt.user.contains("\"author\": \"Alice\""));
    assert!(prompt.user.contains("metadata_source_references"));
    assert!(prompt
        .system
        .contains("Metadata-sourced entities are reference context"));
    assert!(phase1_cache_matches(&[chapter.clone()], &result.output, &policies).is_ok());
    let default_policy = OntologyPolicies::default();
    assert!(phase1_cache_matches(&[], &result.output, &default_policy).is_err());
    let checkpoint = Phase1CheckpointEntry::Success {
        chapter_id: result.output.questions_by_chapter[0].chapter_id.clone(),
        extracted: result.output.questions_by_chapter[0].clone(),
    };
    let checkpoint: Phase1CheckpointEntry =
        serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert_eq!(
        checkpoint_processed_ids(&[checkpoint], &[chapter.clone()], &policies),
        [chapter.chapter_id.clone()].into_iter().collect()
    );
    let checkpoint = Phase1CheckpointEntry::Success {
        chapter_id: result.output.questions_by_chapter[0].chapter_id.clone(),
        extracted: result.output.questions_by_chapter[0].clone(),
    };
    assert!(
        checkpoint_processed_ids(&[checkpoint.clone()], &[chapter.clone()], &default_policy)
            .is_empty()
    );
    assert!(validate_checkpoint(&[checkpoint], &[], &default_policy).is_err());
    let changed_author_rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        r#"{"author":"Bob"}"#,
    )];
    assert!(phase1_cache_matches(
        &[input_chapter(&changed_author_rows)],
        &result.output,
        &policies
    )
    .is_err());
    let mut changed_contract = policies.clone();
    changed_contract
        .prose
        .guidance
        .push_str(" Read every membership carefully.");
    assert!(phase1_cache_matches(&[chapter.clone()], &result.output, &changed_contract).is_err());

    let mut old_result = result.output.questions_by_chapter[0].clone();
    old_result
        .section_extraction
        .as_mut()
        .unwrap()
        .document_read = None;
    let old_checkpoint = Phase1CheckpointEntry::Success {
        chapter_id: old_result.chapter_id.clone(),
        extracted: old_result,
    };
    assert!(
        checkpoint_processed_ids(&[old_checkpoint.clone()], &[chapter.clone()], &policies)
            .is_empty()
    );
    assert!(validate_checkpoint(&[old_checkpoint], &[chapter], &policies).is_err());
}

#[tokio::test]
async fn cached_document_read_replays_without_calling_inference() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        r#"{"author":"Alice"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    let answer = one_document_read();
    let first_chat: crate::types::InferenceFn = Arc::new(move |_, _| {
        called.fetch_add(1, Ordering::SeqCst);
        let answer = answer.clone();
        Box::pin(async move { Ok(answer) })
    });
    reader_runner(temp.path(), &policies, first_chat, true)
        .phase_1_extract_questions(
            &[chapter.clone()],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("identical accountable read must use the section cache"));
    let replay = reader_runner(temp.path(), &policies, panic_chat, true)
        .phase_1_extract_questions(&[chapter], &super::super::ChapterSelection::Full, |_| {})
        .await
        .unwrap();
    assert_eq!(replay.output.questions_by_chapter.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

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
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(replay.output.questions_by_chapter.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[should_panic(expected = "changed author must reject cached accountable read")]
async fn changed_author_does_not_reuse_a_cached_document_read() {
    use std::sync::Arc;

    let body = "Issue 842 was closed by pull request #1380.";
    let original = [row(1, "doc-a", body, r#"{"author":"Alice"}"#)];
    let changed = [row(1, "doc-a", body, r#"{"author":"Bob"}"#)];
    let policies = policies();
    let temp = tempfile::tempdir().unwrap();
    let answer = one_document_read();
    let first: crate::types::InferenceFn = Arc::new(move |_, _| {
        let answer = answer.clone();
        Box::pin(async move { Ok(answer) })
    });
    reader_runner(temp.path(), &policies, first, true)
        .phase_1_extract_questions(
            &[input_chapter(&original)],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();

    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("changed author must reject cached accountable read"));
    let _ = reader_runner(temp.path(), &policies, panic_chat, true)
        .phase_1_extract_questions(
            &[input_chapter(&changed)],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await;
}

#[tokio::test]
#[should_panic(expected = "changed body must reject cached accountable read")]
async fn changed_body_does_not_reuse_a_cached_document_read() {
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
    let answer = one_document_read();
    let first: crate::types::InferenceFn = Arc::new(move |_, _| {
        let answer = answer.clone();
        Box::pin(async move { Ok(answer) })
    });
    reader_runner(temp.path(), &policies, first, true)
        .phase_1_extract_questions(
            &[input_chapter(&original)],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();

    let panic_chat: crate::types::InferenceFn =
        Arc::new(|_, _| panic!("changed body must reject cached accountable read"));
    let _ = reader_runner(temp.path(), &policies, panic_chat, true)
        .phase_1_extract_questions(
            &[input_chapter(&changed)],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await;
}

#[tokio::test]
#[should_panic(expected = "changed READ contract must reject cached accountable read")]
async fn changed_read_contract_does_not_reuse_a_cached_document_read() {
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
    let answer = one_document_read();
    let first: crate::types::InferenceFn = Arc::new(move |_, _| {
        let answer = answer.clone();
        Box::pin(async move { Ok(answer) })
    });
    reader_runner(temp.path(), &policies, first, true)
        .phase_1_extract_questions(
            &[chapter.clone()],
            &super::super::ChapterSelection::Full,
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
        .phase_1_extract_questions(&[chapter], &super::super::ChapterSelection::Full, |_| {})
        .await;
}

#[test]
fn document_read_projects_local_subjects_and_keeps_unknown_fields_out_of_attributes() {
    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed by pull request #1380.",
        r#"{"author":"Alice","role":"member","date":"2024-02-03","kind":"comment","id":"doc-a"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let raw = response(json!([
        {
            "document_id":"doc-a",
            "status":"read",
            "claims":[
                claim("membership", "case-main", "Case 842", "Issue 842 was closed by pull request #1380.", "unknown"),
                claim("membership", "case-spin-off", "Spin-off 842", "Issue 842 was closed by pull request #1380.", "unknown")
            ]
        }
    ]));

    let mut result = parse_response(&raw, &policies).unwrap();
    assert!(
        result.questions.is_empty(),
        "document reads do not ask generic questions"
    );
    let extraction = result.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();
    let read = extraction.document_read.as_ref().unwrap();
    assert_eq!(read.documents[0].claims.len(), 2);
    assert_eq!(
        read.documents[0]
            .claims
            .iter()
            .map(|claim| claim.subject_local_ref.as_str())
            .collect::<Vec<_>>(),
        ["case-main", "case-spin-off"]
    );
    let cached: SectionExtraction =
        serde_json::from_value(serde_json::to_value(&*extraction).unwrap()).unwrap();
    assert_eq!(
        cached.document_read.as_ref().unwrap().documents[0]
            .claims
            .iter()
            .map(|claim| claim.subject_local_ref.as_str())
            .collect::<Vec<_>>(),
        ["case-main", "case-spin-off"],
        "the qualified cache carrier retains both local references"
    );
    assert_eq!(extraction.entities_introduced.len(), 2);
    assert!(extraction.questions_raised.is_empty());
    assert!(
        extraction.claims[0].attributes[SUBJECT_FIELDS_ATTRIBUTE]
            .get("project")
            .is_none(),
        "explicit unknown remains in the read carrier, not a compatibility attribute"
    );
    assert!(
        !extraction.claims[0].attributes.contains_key("project"),
        "unknown is not projected as an ordinary claim value"
    );
    assert!(!extraction.claims[0].attributes.contains_key("status"));
}

#[test]
fn no_applicable_documents_are_a_valid_zero_question_read() {
    let rows = [
        row(1, "doc-a", "A package release note.", r#"{"author":"Ada"}"#),
        row(2, "doc-b", "A build log.", r#"{"author":"Lin"}"#),
    ];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let raw = response(json!([
        {"document_id":"doc-a", "status":"nothing_applicable", "reason":"No case assertion.", "claims":[]},
        {"document_id":"doc-b", "status":"nothing_applicable", "reason":"No case assertion.", "claims":[]}
    ]));
    let mut result = parse_response(&raw, &policies).unwrap();
    assert!(result.questions.is_empty());
    let extraction = result.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();
    assert_eq!(
        extraction.document_read.as_ref().unwrap().documents.len(),
        2
    );
}

#[test]
fn outcomes_and_evidence_are_checked_against_actual_documents() {
    let rows = [
        row(1, "doc-a", "Issue 842 names pip.", r#"{"author":"Ada"}"#),
        row(2, "doc-b", "Issue 159 names uv.", r#"{"author":"Lin"}"#),
    ];
    let chapter = input_chapter(&rows);
    let policies = policies();

    let duplicate = response(json!([
        {"document_id":"doc-a", "status":"nothing_applicable", "reason":"none", "claims":[]},
        {"document_id":"doc-a", "status":"nothing_applicable", "reason":"none", "claims":[]}
    ]));
    let mut parsed = parse_response(&duplicate, &policies).unwrap();
    assert!(validate_and_stamp(
        &chapter,
        &policies,
        parsed.section_extraction.as_mut().unwrap()
    )
    .is_err());

    let missing = response(json!([
        {"document_id":"doc-a", "status":"nothing_applicable", "reason":"none", "claims":[]}
    ]));
    let mut parsed = parse_response(&missing, &policies).unwrap();
    assert!(validate_and_stamp(
        &chapter,
        &policies,
        parsed.section_extraction.as_mut().unwrap()
    )
    .is_err());

    let foreign_document = response(json!([
        {"document_id":"doc-a", "status":"nothing_applicable", "reason":"none", "claims":[]},
        {"document_id":"not-supplied", "status":"not_read", "reason":"no body", "claims":[]}
    ]));
    let mut parsed = parse_response(&foreign_document, &policies).unwrap();
    assert!(validate_and_stamp(
        &chapter,
        &policies,
        parsed.section_extraction.as_mut().unwrap()
    )
    .is_err());

    let wrong_document_evidence = response(json!([
        {"document_id":"doc-a", "status":"read", "claims":[claim("membership", "case-a", "Case 842", "Issue 159 names uv.", "unknown")]},
        {"document_id":"doc-b", "status":"nothing_applicable", "reason":"none", "claims":[]}
    ]));
    let mut parsed = parse_response(&wrong_document_evidence, &policies).unwrap();
    let extraction = parsed.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();
    let read = extraction.document_read.as_ref().unwrap();
    let a = read
        .documents
        .iter()
        .find(|outcome| outcome.document_id == "doc-a")
        .unwrap();
    assert_eq!(a.status, DocumentReadStatus::CouldNotJudge);
    assert_eq!(a.refused.len(), 1);
    assert_eq!(a.refused[0].kind, "membership");
    assert!(
        extraction.claims.is_empty(),
        "a refused claim is never projected"
    );
}

#[test]
fn a_repeated_local_reference_with_a_changed_label_is_refused_not_fatal() {
    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed.",
        r#"{"author":"Ada"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let first = claim(
        "membership",
        "case-842",
        "Case 842",
        "Issue 842 was closed.",
        "unknown",
    );
    let second = claim(
        "membership",
        "case-842",
        "Case 843",
        "Issue 842 was closed.",
        "unknown",
    );
    let raw = response(json!([{
        "document_id":"doc-a",
        "status":"read",
        "claims":[first, second]
    }]));
    let mut parsed = parse_response(&raw, &policies).unwrap();
    let extraction = parsed.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();
    assert_eq!(extraction.claims.len(), 1, "the first occurrence stands");
    let read = extraction.document_read.as_ref().unwrap();
    assert_eq!(read.documents[0].refused.len(), 1);
    assert!(read.documents[0].refused[0]
        .reason
        .contains("changes label"));
}

#[test]
fn undeclared_claim_kinds_are_refused_and_identity_changes_reuse_read_contract() {
    let rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed.",
        r#"{"author":"Ada"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let undeclared = response(json!([{
        "document_id":"doc-a",
        "status":"read",
        "claims":[claim("invented_transition", "case-a", "Case 842", "Issue 842 was closed.", "unknown")]
    }]));
    assert!(parse_response(&undeclared, &policies).is_err());

    let first = contract_fingerprint(&policies);
    let mut changed_identity = policies.clone();
    let record = changed_identity
        .shape
        .types
        .iter_mut()
        .find(|ty| ty.name == "case")
        .unwrap();
    record.identity_criterion = Some("a revised resolution rule".into());
    record.identity = vec!["number".into(), "external_key".into()];
    changed_identity
        .identity
        .identity
        .insert("case".into(), vec!["external_key".into()]);
    assert_eq!(first, contract_fingerprint(&changed_identity));

    let read = response(json!([{
        "document_id":"doc-a",
        "status":"read",
        "claims":[claim("membership", "case-a", "Case 842", "Issue 842 was closed.", "unknown")]
    }]));
    let mut result = parse_response(&read, &policies).unwrap();
    let extraction = result.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();
    assert!(cache_matches_chapter(&chapter, extraction, &changed_identity).is_ok());

    let changed_author_rows = [row(
        1,
        "doc-a",
        "Issue 842 was closed.",
        r#"{"author":"Grace"}"#,
    )];
    let changed_context = input_chapter(&changed_author_rows);
    assert!(cache_matches_chapter(&changed_context, extraction, &policies).is_err());

    let mut changed_read_contract = policies.clone();
    changed_read_contract
        .prose
        .guidance
        .push_str(" Never infer transitions.");
    assert!(cache_matches_chapter(&chapter, extraction, &changed_read_contract).is_err());
}
