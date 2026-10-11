// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeMap;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::resolution_records::BuildAtoms;
use corpus_engine::enrichment::atlas::SectionDocuments;
use corpus_engine::enrichment::ontology::{validate_block, OntologyPolicies};
use corpus_engine::enrichment::pipeline::document_read::{
    CLAIM_FIELDS_ATTRIBUTE, LOCAL_REF_ATTRIBUTE, SOURCE_DOCUMENT_ATTRIBUTE,
    SUBJECT_FIELDS_ATTRIBUTE,
};
use corpus_engine::index::EnrichmentChunkRow;
use corpus_engine::Recipe;
use corpus_engine::{enrichment::atlas::atoms::Claim, InferenceFn};
use serde_json::{json, Value};

use super::{apply, DocumentInputs, DECISIONS_FILE, DERIVED_FILE};

const DECLARATION: &str = r#"
[corpus]
id = "protocol-test"
name = "protocol-test"
[acquire]
type = "local_file"
path = "/tmp/protocol-test.jsonl"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
domain = "declared records"
[enrichment.ontology]
version = 1

[[enrichment.ontology.types]]
name = "case"
kind = "entity"
identity = ["case_number"]
identity_necessary = ["case_number"]
identity_criterion = "the same declared case number"
attributes = [
  { name = "case_number", type = "text", values = ["17", "18"] },
  { name = "state", type = "text", values = ["open", "resolved"], derived = "case_state" },
]

[[enrichment.ontology.types]]
name = "transition"
kind = "claim"
force = "assertive"
subject = "case"
attributes = [
  { name = "transition_id", type = "text" },
  { name = "action", type = "text", values = ["closed_by_pr", "reopen", "correction", "cross_reference", "closed"] },
  { name = "role", type = "text", values = ["maintainer"] },
  { name = "voice", type = "text", values = ["maintainer", "reporter"] },
  { name = "polarity", type = "text", values = ["affirmative"] },
  { name = "effective_at", type = "time" },
  { name = "corrects", type = "text" },
  { name = "redirect_to", type = "text" },
]

[enrichment.ontology.change]
document = { date = "reported_at" }

[[enrichment.ontology.folds]]
id = "case_state"
by = "protocol"
from = ["^subject"]

[enrichment.ontology.folds.protocol]
identity = "transition_id"
effective_time = "effective_at"

[[enrichment.ontology.folds.protocol.rules]]
id = "close_by_pr"
claim_kind = "transition"
state = "resolved"
when = { action = "closed_by_pr", role = "maintainer", voice = "maintainer", polarity = "affirmative" }

[[enrichment.ontology.folds.protocol.rules]]
id = "reopen"
claim_kind = "transition"
state = "open"
when = { action = "reopen", role = "maintainer", voice = "maintainer", polarity = "affirmative" }

[[enrichment.ontology.folds.protocol.rules]]
id = "correct"
claim_kind = "transition"
state = "open"
corrects = "corrects"
when = { action = "correction", role = "maintainer", voice = "maintainer", polarity = "affirmative" }
"#;

#[derive(Clone)]
struct Input {
    id: &'static str,
    document: &'static str,
    reported_at: Option<&'static str>,
    action: &'static str,
    speaker: Option<&'static str>,
    role: Option<&'static str>,
    polarity: Option<&'static str>,
    transition_id: &'static str,
    effective_at: Option<&'static str>,
    corrects: Option<&'static str>,
    redirect_to: Option<&'static str>,
    case_number: &'static str,
    field_provenance: bool,
    local_ref: &'static str,
}

fn policies(declaration: &str) -> OntologyPolicies {
    Recipe::from_toml(declaration)
        .unwrap()
        .custom_atlas_spec()
        .expect("the recipe has a custom atlas")
        .policies()
}

fn supported(value: &str, evidence: &str) -> Value {
    json!({ "status": "supported", "value": value, "evidence": evidence })
}

fn unknown(reason: &str) -> Value {
    json!({ "status": "unknown", "reason": reason })
}

fn atoms_for(inputs: &[Input]) -> (Vec<EnrichmentChunkRow>, SectionDocuments, Vec<Claim>) {
    let rows: Vec<EnrichmentChunkRow> = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let metadata = match input.reported_at {
                Some(date) => json!({ "reported_at": date, "author": "maintainer" }),
                None => json!({ "author": "maintainer" }),
            };
            EnrichmentChunkRow {
                id: index as u64 + 1,
                content: format!("{} evidence", input.id),
                title: None,
                url: None,
                metadata_raw: Some(metadata.to_string()),
                source_doc_id: Some(input.document.into()),
            }
        })
        .collect();
    let chunk_ids: Vec<u64> = rows.iter().map(|row| row.id).collect();
    let docs = SectionDocuments::from_chunk_rows([("section", chunk_ids.as_slice())], &rows);
    let claims = inputs
        .iter()
        .map(|input| {
            let evidence = format!("{} evidence", input.id);
            let mut source_fields = serde_json::Map::new();
            source_fields.insert(
                "transition_id".into(),
                supported(input.transition_id, &evidence),
            );
            source_fields.insert("action".into(), supported(input.action, &evidence));
            source_fields.insert(
                "role".into(),
                input
                    .role
                    .map(|value| supported(value, &evidence))
                    .unwrap_or_else(|| unknown("the source does not state the role")),
            );
            source_fields.insert(
                "polarity".into(),
                input
                    .polarity
                    .map(|value| supported(value, &evidence))
                    .unwrap_or_else(|| unknown("the source does not state the polarity")),
            );
            source_fields.insert(
                "effective_at".into(),
                input
                    .effective_at
                    .map(|value| supported(value, &evidence))
                    .unwrap_or_else(|| unknown("the source does not state an effective time")),
            );
            source_fields.insert(
                "corrects".into(),
                input
                    .corrects
                    .map(|value| supported(value, &evidence))
                    .unwrap_or_else(|| unknown("the source does not identify a correction target")),
            );
            source_fields.insert(
                "redirect_to".into(),
                input
                    .redirect_to
                    .map(|value| supported(value, &evidence))
                    .unwrap_or_else(|| unknown("the source does not name a redirect target")),
            );
            source_fields.insert(
                "voice".into(),
                input
                    .speaker
                    .map(|value| supported(value, &evidence))
                    .unwrap_or_else(|| unknown("the source does not identify a speaker")),
            );
            let mut attributes = serde_json::Map::new();
            for (name, value) in [
                ("transition_id", input.transition_id),
                ("action", input.action),
            ] {
                attributes.insert(name.into(), json!(value));
            }
            if let Some(value) = input.role {
                attributes.insert("role".into(), json!(value));
            }
            if let Some(value) = input.speaker {
                attributes.insert("voice".into(), json!(value));
            }
            if let Some(value) = input.polarity {
                attributes.insert("polarity".into(), json!(value));
            }
            if let Some(value) = input.effective_at {
                attributes.insert("effective_at".into(), json!(value));
            }
            if let Some(value) = input.corrects {
                attributes.insert("corrects".into(), json!(value));
            }
            if let Some(value) = input.redirect_to {
                attributes.insert("redirect_to".into(), json!(value));
            }
            attributes.insert(LOCAL_REF_ATTRIBUTE.into(), json!(input.local_ref));
            attributes.insert(SOURCE_DOCUMENT_ATTRIBUTE.into(), json!(input.document));
            attributes.insert(
                SUBJECT_FIELDS_ATTRIBUTE.into(),
                json!({ "case_number": supported(input.case_number, &evidence) }),
            );
            if input.field_provenance {
                attributes.insert(CLAIM_FIELDS_ATTRIBUTE.into(), Value::Object(source_fields));
            }
            serde_json::from_value(json!({
                "id": input.id,
                "content": evidence,
                "discourse_act": "assert",
                "epistemic_status": "confident",
                "scope": "universal",
                "attributed_to": input.speaker,
                "evidence": [{
                    "chunk_id": "section",
                    "passage_preview": evidence,
                    "source_doc_id": input.document
                }],
                "anchor": evidence,
                "claim_kind": "transition",
                "attributes": attributes,
                "enrichment_depth": "extracted"
            }))
            .unwrap()
        })
        .collect();
    (rows, docs, claims)
}

async fn replay(declaration: &str, inputs: &[Input]) -> Vec<Value> {
    let policies = policies(declaration);
    let (_rows, documents, mut claims) = atoms_for(inputs);
    let input = DocumentInputs {
        documents: Some(documents),
        projection: None,
        participants: Default::default(),
    };
    let (mut entities, mut events, mut states, mut relations) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut argument_reconstructions, mut positions, mut oppositions, mut edges) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut trajectories = BTreeMap::new();
    let atoms = BuildAtoms {
        entities: &mut entities,
        events: &mut events,
        states: &mut states,
        relations: &mut relations,
        claims: &mut claims,
        argument_reconstructions: &mut argument_reconstructions,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    let inference: InferenceFn = Arc::new(|_, _| {
        Box::pin(async { panic!("pure protocol replay attempted model inference") })
    });
    let directory = tempfile::tempdir().unwrap();
    let failures = apply(
        atoms,
        &input,
        &policies,
        "protocol-test",
        &inference,
        directory.path(),
    )
    .await
    .expect("RESOLVE and the derived-decision writer complete without inference");
    assert!(
        failures.is_empty()
            || failures
                .iter()
                .all(|failure| failure.reason.contains("document_date")),
        "unexpected resolve failures: {failures:?}"
    );
    assert!(directory.path().join(DECISIONS_FILE).exists());
    let lines = std::fs::read_to_string(directory.path().join(DERIVED_FILE)).unwrap();
    let decisions: Vec<Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for entity in entities
        .iter()
        .filter(|entity| entity.entity_type.as_str_repr() == "case")
    {
        let decision = state_line(&decisions);
        if decision["outcome"] != "decided" {
            assert!(
                !entity.attributes.contains_key("state"),
                "a non-decision must not leave a scalar state on the atom: decision={decision}, attributes={:?}",
                entity.attributes
            );
        }
    }
    decisions
}

fn state_line(lines: &[Value]) -> &Value {
    state_lines(lines)[0]
}

fn state_lines(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["type"] == "case" && line["attribute"] == "state")
        .collect()
}

fn transition(
    id: &'static str,
    document: &'static str,
    reported_at: Option<&'static str>,
    action: &'static str,
    transition_id: &'static str,
) -> Input {
    Input {
        id,
        document,
        reported_at,
        action,
        speaker: Some("maintainer"),
        role: Some("maintainer"),
        polarity: Some("affirmative"),
        transition_id,
        effective_at: Some("2026-10-01"),
        corrects: None,
        redirect_to: None,
        case_number: "17",
        field_provenance: true,
        local_ref: "case-17",
    }
}

#[tokio::test]
async fn a_supported_close_projects_with_report_time_rule_and_field_evidence() {
    let lines = replay(
        DECLARATION,
        &[transition(
            "claim-close",
            "doc-close",
            Some("2026-10-05T12:00:00Z"),
            "closed_by_pr",
            "transition-close",
        )],
    )
    .await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "decided");
    assert_eq!(line["values"], json!(["resolved"]));
    assert_eq!(
        line["protocol"]["as_of_report_time"],
        "2026-10-05T12:00:00Z"
    );
    assert_eq!(line["protocol"]["effective_times"], json!(["2026-10-01"]));
    assert_eq!(line["protocol"]["history"][0]["rule"], "close_by_pr");
    assert_eq!(line["protocol"]["basis_claims"], json!(["claim-close"]));
    assert_eq!(line["protocol"]["history"][0]["assigned_to"], line["atom"]);
    assert_eq!(
        line["protocol"]["history"][0]["citation"][0]["source_doc_id"],
        "doc-close"
    );
    assert!(
        line["protocol"]["history"][0]["field_evidence"]["action"]["evidence"]
            .as_str()
            .is_some()
    );
}

#[tokio::test]
async fn a_cross_reference_or_bare_closed_value_has_no_state_effect() {
    let cross_reference = transition(
        "claim-ref",
        "doc-ref",
        Some("2026-10-05T12:00:00Z"),
        "cross_reference",
        "transition-ref",
    );
    let lines = replay(DECLARATION, &[cross_reference]).await;
    assert_eq!(state_line(&lines)["outcome"], "nothing");
    assert!(state_line(&lines)["protocol"]["history"]
        .as_array()
        .unwrap()[0]["rule"]
        .is_null());

    let bare_closed = transition(
        "claim-bare",
        "doc-bare",
        Some("2026-10-05T12:00:00Z"),
        "closed",
        "transition-bare",
    );
    let lines = replay(DECLARATION, &[bare_closed]).await;
    assert_eq!(state_line(&lines)["outcome"], "nothing");
}

#[tokio::test]
async fn reopen_keeps_the_resolved_report_in_history() {
    let close = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let reopen = transition(
        "claim-reopen",
        "doc-reopen",
        Some("2026-10-06T12:00:00Z"),
        "reopen",
        "transition-reopen",
    );
    let lines = replay(DECLARATION, &[close, reopen]).await;
    let line = state_line(&lines);
    assert_eq!(line["values"], json!(["open"]));
    assert_eq!(line["protocol"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(
        line["protocol"]["basis_claims"].as_array().unwrap().len(),
        2
    );
    assert!(line["protocol"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["state"] == "resolved"));
}

#[tokio::test]
async fn a_directed_redirect_does_not_assign_or_decline_another_record() {
    let mut redirects_from = transition(
        "claim-redirect-from",
        "doc-redirect-from",
        Some("2026-10-05T12:00:00Z"),
        "cross_reference",
        "transition-redirect-from",
    );
    redirects_from.redirect_to = Some("18");
    let mut redirects_to = transition(
        "claim-redirect-to",
        "doc-redirect-to",
        Some("2026-10-06T12:00:00Z"),
        "cross_reference",
        "transition-redirect-to",
    );
    redirects_to.case_number = "18";
    redirects_to.local_ref = "case-18";

    let lines = replay(DECLARATION, &[redirects_from, redirects_to]).await;
    let states = state_lines(&lines);
    assert_eq!(states.len(), 2);
    assert!(states.iter().all(|line| line["outcome"] == "nothing"));
    assert_ne!(states[0]["atom"], states[1]["atom"]);
}

#[tokio::test]
async fn duplicate_closes_are_excluded_from_selection_but_retained_as_evidence() {
    let first = transition(
        "claim-close-a",
        "doc-close-a",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let duplicate = transition(
        "claim-close-b",
        "doc-close-b",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let lines = replay(DECLARATION, &[first, duplicate]).await;
    let line = state_line(&lines);
    assert_eq!(line["values"], json!(["resolved"]));
    assert_eq!(
        line["protocol"]["basis_claims"].as_array().unwrap().len(),
        2
    );
    assert_eq!(line["protocol"]["history"].as_array().unwrap().len(), 2);
    assert!(line["protocol"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["disposition"] == "duplicate"));
}

#[tokio::test]
async fn missing_role_voice_or_polarity_is_pending_not_a_default() {
    let mut missing_role = transition(
        "claim-role",
        "doc-role",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-role",
    );
    missing_role.role = None;
    let mut missing_voice = transition(
        "claim-voice",
        "doc-voice",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-voice",
    );
    missing_voice.speaker = None;
    let mut missing_polarity = transition(
        "claim-polarity",
        "doc-polarity",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-polarity",
    );
    missing_polarity.polarity = None;
    let lines = replay(
        DECLARATION,
        &[missing_role, missing_voice, missing_polarity],
    )
    .await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "pending");
    assert_eq!(line["values"], json!(["resolved"]));
    assert!(line["protocol"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["disposition"] == "pending"));
}

#[tokio::test]
async fn an_explicit_quote_speaker_does_not_inherit_the_carrier_author() {
    let mut quoted = transition(
        "claim-quote",
        "doc-quote",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-quote",
    );
    quoted.speaker = Some("reporter");
    let lines = replay(DECLARATION, &[quoted]).await;
    assert_eq!(state_line(&lines)["outcome"], "nothing");
}

#[tokio::test]
async fn a_dated_and_undated_transition_remain_alternatives() {
    let close = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let reopen = transition(
        "claim-reopen",
        "doc-reopen",
        None,
        "reopen",
        "transition-reopen",
    );
    let lines = replay(DECLARATION, &[close, reopen]).await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "pending");
    assert_eq!(
        line["protocol"]["alternatives"],
        json!(["open", "resolved"])
    );
    assert_eq!(line["values"], json!(["open", "resolved"]));
}

#[tokio::test]
async fn incompatible_states_at_the_same_report_time_conflict() {
    let close = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let reopen = transition(
        "claim-reopen",
        "doc-reopen",
        Some("2026-10-05T12:00:00Z"),
        "reopen",
        "transition-reopen",
    );
    let lines = replay(DECLARATION, &[close, reopen]).await;
    let line = state_line(&lines);
    assert_eq!(line["outcome"], "conflict");
    assert_eq!(line["values"], json!(["open", "resolved"]));
    assert_eq!(
        line["protocol"]["as_of_report_time"],
        "2026-10-05T12:00:00Z"
    );
}

#[tokio::test]
async fn correction_needs_a_target_identity_and_replay_reacts_to_rule_data() {
    let original = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let mut correction = transition(
        "claim-correction",
        "doc-correction",
        Some("2026-10-06T12:00:00Z"),
        "correction",
        "transition-correction",
    );
    correction.corrects = Some("transition-close");
    let corrected = replay(DECLARATION, &[original.clone(), correction.clone()]).await;
    assert_eq!(state_line(&corrected)["values"], json!(["open"]));
    assert!(state_line(&corrected)["protocol"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["disposition"] == "corrected"));

    correction.corrects = Some("not-present");
    let unresolved = replay(DECLARATION, &[original.clone(), correction]).await;
    assert_eq!(state_line(&unresolved)["outcome"], "pending");
    assert_eq!(
        state_line(&unresolved)["values"],
        json!(["open", "resolved"])
    );

    let mut earlier_correction = transition(
        "claim-earlier-correction",
        "doc-earlier-correction",
        Some("2026-10-04T12:00:00Z"),
        "correction",
        "transition-earlier-correction",
    );
    earlier_correction.corrects = Some("transition-close");
    let unordered = replay(DECLARATION, &[original.clone(), earlier_correction]).await;
    assert_eq!(state_line(&unordered)["outcome"], "pending");
    assert!(state_line(&unordered)["protocol"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("reported before target")));

    let changed_rule = DECLARATION.replace(
        "state = \"resolved\"\nwhen = { action = \"closed_by_pr\"",
        "state = \"open\"\nwhen = { action = \"closed_by_pr\"",
    );
    let revised = replay(&changed_rule, &[original]).await;
    assert_eq!(state_line(&revised)["values"], json!(["open"]));
    assert_ne!(
        state_line(&corrected)["protocol"]["rule_fingerprint"],
        state_line(&revised)["protocol"]["rule_fingerprint"]
    );
}

#[tokio::test]
async fn assignment_replay_is_deterministic_and_does_not_call_inference() {
    let mut first = transition(
        "claim-close",
        "doc-close",
        Some("2026-10-05T12:00:00Z"),
        "closed_by_pr",
        "transition-close",
    );
    let first_lines = replay(DECLARATION, &[first.clone()]).await;
    let same_lines = replay(DECLARATION, &[first.clone()]).await;
    assert_eq!(first_lines, same_lines);

    first.local_ref = "case-reassigned";
    let reassigned = replay(DECLARATION, &[first]).await;
    assert_eq!(state_line(&reassigned)["values"], json!(["resolved"]));
    assert_ne!(
        state_line(&first_lines)["atom"],
        state_line(&reassigned)["atom"]
    );
    assert_ne!(
        state_line(&first_lines)["protocol"]["assignment_dependencies"],
        state_line(&reassigned)["protocol"]["assignment_dependencies"]
    );
}

#[test]
fn protocol_declaration_roundtrips_and_rejects_unknown_claim_fields_states_and_paths() {
    let original = policies(DECLARATION);
    let encoded = serde_json::to_value(&original).unwrap();
    let decoded: OntologyPolicies = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, original);

    let invalid = [
        (
            DECLARATION.replace("state = \"resolved\"", "state = \"unlisted\""),
            "closed values",
        ),
        (
            DECLARATION.replace("identity = \"transition_id\"", "identity = \"missing_id\""),
            "does not declare",
        ),
        (
            DECLARATION.replace(
                "claim_kind = \"transition\"",
                "claim_kind = \"missing_kind\"",
            ),
            "undeclared claim type",
        ),
        (
            DECLARATION.replace(
                "from = [\"^subject\"]",
                "from = [\"^subject / missing_step\"]",
            ),
            "missing_step",
        ),
    ];
    for (recipe, expected) in invalid {
        let parsed = Recipe::from_toml(&recipe).unwrap();
        let block = parsed
            .enrichment
            .and_then(|enrichment| enrichment.ontology)
            .expect("the recipe retains its declaration block");
        let errors = validate_block(&block).errors;
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "expected {expected:?} in {errors:?}"
        );
    }
}

#[path = "atlas_resolve_documents_replay_tests.rs"]
mod replay_tests;

#[test]
fn decision_writer_creates_parent_on_a_fresh_atlas() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fresh/atlas/derived_decisions.jsonl");
    assert!(!path.parent().unwrap().exists());
    let mut writer = super::Jsonl::create(path.clone()).unwrap();
    writer.line(&json!({"outcome":"pending"}));
    assert_eq!(writer.finish().unwrap(), path);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"outcome\":\"pending\"}\n"
    );
}
