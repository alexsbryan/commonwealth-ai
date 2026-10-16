use std::collections::HashMap;

use serde_json::{json, Value};

use super::*;
use crate::enrichment::atlas::SectionDocuments;
use crate::enrichment::ontology::{OntologyPolicies, OntologyV1};
use crate::enrichment::pipeline::atlas::SectionExtraction;
use crate::enrichment::pipeline::runner::Phase1CheckpointEntry;
use crate::enrichment::pipeline::types::ChapterInput;

#[path = "document_read/cache_reuse_tests.rs"]
mod cache_reuse_tests;
#[path = "document_read/claim_check_tests.rs"]
mod claim_check_tests;
#[path = "document_read/voices_tests.rs"]
mod voices_tests;

pub(super) fn policies() -> OntologyPolicies {
    let ontology: OntologyV1 = toml::from_str(
        r#"
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

/// A model that answers `answers` in order and records every question.
pub(super) fn scripted(
    answers: Vec<Value>,
) -> (
    crate::InferenceFn,
    std::sync::Arc<std::sync::Mutex<Vec<crate::enrichment::pipeline::types::ChatPrompt>>>,
) {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(answers.into_iter()));
    let kept = seen.clone();
    let f: crate::InferenceFn = Arc::new(
        move |p: &crate::enrichment::pipeline::types::ChatPrompt, _| {
            kept.lock().unwrap().push(p.clone());
            let next = queue.lock().unwrap().next().map(|v| v.to_string());
            Box::pin(async move {
                next.ok_or_else(|| crate::Error::Extraction("no scripted answer left".into()))
            })
        },
    );
    (f, seen)
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

/// The one statement the runner tests' document makes.
const CLOSED: &str = "Issue 842 was closed by pull request #1380.";

/// A window model for a one-document window holding [`CLOSED`]: case 842's
/// membership, quoting that line, its number read at "Issue 842". Every
/// prompt is kept, one per window read; a second read errs.
fn counted() -> (
    crate::types::InferenceFn,
    std::sync::Arc<std::sync::Mutex<Vec<crate::enrichment::pipeline::types::ChatPrompt>>>,
) {
    scripted(vec![json!({
        "particulars": [{"ref": "p1", "type": "case", "name": "issue 842",
            "fields": {"number": {"value": "842", "quote": "Issue 842"}}}],
        "claims": [{"kind": "membership", "about": "p1", "document": "d1", "quote": CLOSED, "fields": {}}]})])
}

/// The window reads `counted` was asked.
fn calls(prompts: &std::sync::Mutex<Vec<crate::enrichment::pipeline::types::ChatPrompt>>) -> usize {
    prompts.lock().unwrap().len()
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

/// The body-word floor guards the general extractor against heading-only
/// sections; a declared document read is exempt from it (`runner.rs`,
/// `!document_reading`), so an 8-word document under a floor of 40 is still
/// read (abfe32a14: the floor skipped 12 uv states).
#[tokio::test]
async fn a_document_read_reads_a_section_under_the_body_word_floor() {
    let rows = [row(1, "doc-a", CLOSED, "{}")];
    let (chat, prompts) = counted();
    let temp = tempfile::tempdir().unwrap();
    let runner = reader_runner(temp.path(), &policies(), chat, false).with_min_body_words(40);
    let result = runner
        .phase_1_extract_questions(
            &[input_chapter(&rows)],
            &super::super::ChapterSelection::Full,
            |_| {},
        )
        .await
        .unwrap();
    assert!(result.failures.is_empty(), "{:?}", result.failures);
    assert_eq!(calls(&prompts), 1);
}

#[tokio::test]
async fn production_phase1_uses_the_mock_provider_with_actual_document_context() {
    let rows = [row(
        1,
        "doc-a",
        CLOSED,
        r#"{"author":"Alice","role":"member","date":"2024-02-03","kind":"comment","id":"doc-a"}"#,
    )];
    let chapter = input_chapter(&rows);
    let policies = policies();
    let (chat, prompts) = counted();
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

    // One window read of the chapter.
    assert_eq!(calls(&prompts), 1);
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
    let phases: Vec<&str> = prompts
        .iter()
        .map(|p| p.phase_id.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(phases, [super::window::WINDOW_PHASE]);
    // A metadata-sourced author is a declared fact the document's head
    // carries, and never text the reader reads in its body.
    for p in prompts.iter() {
        let block = &p.user[p.user.find("=== d1 ===\n").unwrap()..];
        let (facts, body) = block.split_once("\n\n").unwrap();
        assert!(facts.contains("author: Alice"), "{}", p.user);
        assert!(body.contains(CLOSED), "{}", p.user);
        assert!(!body.contains("Alice"), "{}", p.user);
    }
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
    // No `case` is sketched (RESOLVE decides it); its readings ride the claim.
    assert!(extraction.entities_introduced.is_empty());
    assert!(extraction.questions_raised.is_empty());
    let subject = &extraction.claims[0].attributes[SUBJECT_FIELDS_ATTRIBUTE];
    assert_eq!(subject["number"]["evidence"], "842");
    assert!(subject["project"]["reason"].is_string());
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

/// The window reader decides sameness inside a window, so it is shown each
/// decided type's identity criterion ("Same case when: ...",
/// `window.rs::ontology_text`): a criterion change is a new read contract and
/// re-reads. The identity keys are RESOLVE's alone and reuse the read.
#[test]
fn undeclared_claim_kinds_are_refused_and_a_criterion_change_rereads_where_a_key_change_does_not() {
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
    let mut changed_keys = policies.clone();
    let record = changed_keys
        .shape
        .types
        .iter_mut()
        .find(|ty| ty.name == "case")
        .unwrap();
    record.identity = vec!["number".into(), "external_key".into()];
    changed_keys
        .identity
        .identity
        .insert("case".into(), vec!["external_key".into()]);
    assert_eq!(first, contract_fingerprint(&changed_keys));
    let mut changed_criterion = policies.clone();
    changed_criterion
        .shape
        .types
        .iter_mut()
        .find(|ty| ty.name == "case")
        .unwrap()
        .identity_criterion = Some("a revised resolution rule".into());
    assert_ne!(first, contract_fingerprint(&changed_criterion));

    let read = response(json!([{
        "document_id":"doc-a",
        "status":"read",
        "claims":[claim("membership", "case-a", "Case 842", "Issue 842 was closed.", "unknown")]
    }]));
    let mut result = parse_response(&read, &policies).unwrap();
    let extraction = result.section_extraction.as_mut().unwrap();
    validate_and_stamp(&chapter, &policies, extraction).unwrap();
    assert!(cache_matches_chapter(&chapter, extraction, &changed_keys).is_ok());
    assert!(cache_matches_chapter(&chapter, extraction, &changed_criterion).is_err());

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

/// Failing input, named: with SOVEREIGN_READER unset the default commands read
/// with the passes reader, though the window reader met its flip condition
/// (DEFAULTS_LEDGER, 2026-10-15). Unset names the window reader; a value naming
/// no reader is warned and the window reader reads.
#[test]
fn the_window_reader_reads_unless_another_is_named() {
    use super::{reader_named, Reader};
    assert_eq!(reader_named(None), Reader::Window);
    assert_eq!(reader_named(Some("window")), Reader::Window);
    // The compiled waves reader and the passes reader were deleted (2026-10-16):
    // each name is warned and the window reader reads.
    assert_eq!(reader_named(Some("passes")), Reader::Window);
    assert_eq!(reader_named(Some("waves")), Reader::Window);
    assert_eq!(reader_named(Some("pases")), Reader::Window);
}
