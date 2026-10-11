use super::*;
use crate::enrichment::atlas::ann_store::AtlasSeeding;
use crate::enrichment::atlas::AtomEnvelope;
use crate::enrichment::ontology::{AttrDecl, AttrFamily, DocumentFieldsDecl, Force};
use crate::enrichment::pipeline::document_read::{
    LOCAL_REF_ATTRIBUTE, SOURCE_DOCUMENT_ATTRIBUTE, SUBJECT_FIELDS_ATTRIBUTE,
};
use serde_json::json;

/// A subject field the reader read, as the projection carries it.
fn read(value: &str) -> Value {
    json!({"status": "supported", "value": value, "evidence": value})
}

#[tokio::test]
async fn state_free_membership_resolves_and_survives_the_production_atlas_writer() {
    let body = "Issue 842 is closed; its spin-off issue 159 is also closed.";
    let rows = [
        corpus_index::index::EnrichmentChunkRow {
            id: 41,
            content: body.into(),
            title: Some("Same title".into()),
            url: Some("https://example.test/a".into()),
            metadata_raw: Some(json!({"thread":"t1","id":"doc-a"}).to_string()),
            source_doc_id: Some("doc-a".into()),
        },
        corpus_index::index::EnrichmentChunkRow {
            id: 42,
            content: body.into(),
            title: Some("Same title".into()),
            url: Some("https://example.test/b".into()),
            metadata_raw: Some(json!({"thread":"t1","id":"doc-b"}).to_string()),
            source_doc_id: Some("doc-b".into()),
        },
    ];
    let documents = SectionDocuments::from_chunk_rows([("sec_1", &[41u64, 42][..])], &rows);
    let mut policies = OntologyPolicies::default();
    policies.shape.types = vec![
        OntologyTypeDecl {
            name: "case".into(),
            kind: TypeKind::Entity,
            description: "A support case".into(),
            identity_criterion: Some("the same issue identifier".into()),
            identity_necessary: vec!["project".into()],
            attributes: vec![
                AttrDecl {
                    name: "number".into(),
                    family: AttrFamily::Text { values: Vec::new() },
                    description: "The issue number".into(),
                    derived: None,
                },
                AttrDecl {
                    name: "project".into(),
                    family: AttrFamily::Text {
                        values: vec!["uv".into(), "pip".into()],
                    },
                    description: "The owning project".into(),
                    derived: None,
                },
            ],
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "membership".into(),
            kind: TypeKind::Claim,
            force: Some(Force::Assertive),
            subject: Some("case".into()),
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "reported_status".into(),
            kind: TypeKind::Claim,
            force: Some(Force::Assertive),
            subject: Some("case".into()),
            attributes: vec![AttrDecl {
                name: "status".into(),
                family: AttrFamily::Text {
                    values: vec!["open".into(), "closed".into()],
                },
                description: "The reported case status".into(),
                derived: None,
            }],
            ..Default::default()
        },
    ];
    policies
        .identity
        .identity
        .insert("case".into(), vec!["number".into()]);
    policies.change.document = Some(DocumentFieldsDecl {
        date: None,
        thread: Some("thread".into()),
        id: Some("id".into()),
        author: None,
    });

    let claim = |id: &str, kind: &str, document: &str, local: &str, number: &str, project: &str| {
        let mut attributes = Map::new();
        if kind == "reported_status" {
            attributes.insert("status".into(), Value::String("closed".into()));
        }
        attributes.insert(LOCAL_REF_ATTRIBUTE.into(), Value::String(local.into()));
        attributes.insert(
            SOURCE_DOCUMENT_ATTRIBUTE.into(),
            Value::String(document.into()),
        );
        attributes.insert(
            SUBJECT_FIELDS_ATTRIBUTE.into(),
            json!({"number": read(number), "project": read(project)}),
        );
        serde_json::from_value(json!({
            "id": id,
            "content": "This document concerns the named support case.",
            "discourse_act": "assert",
            "epistemic_status": "attributed",
            "scope": "universal",
            "evidence": [{"chunk_id":"sec_1","passage_preview":body}],
            "subject": null,
            "claim_kind": kind,
            "anchor": body,
            "attributes": attributes,
            "enrichment_depth": "extracted"
        }))
        .unwrap()
    };
    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = vec![
        claim("claim-1", "membership", "doc-a", "case-main", "842", "uv"),
        claim(
            "claim-2",
            "membership",
            "doc-a",
            "case-spin-off",
            "159",
            "pip",
        ),
        claim("claim-3", "membership", "doc-b", "case-main", "842", "uv"),
        claim(
            "claim-4",
            "reported_status",
            "doc-b",
            "case-main",
            "842",
            "uv",
        ),
    ];
    let mut argument_reconstructions = Vec::new();
    let mut positions = Vec::new();
    let mut oppositions = Vec::new();
    let mut edges = Vec::new();
    let mut trajectories = BTreeMap::new();
    let (reports, failures) = {
        let mut atoms = BuildAtoms {
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
        let mut on_document = |_: &str, _: &DocumentResolution| {};
        resolve_declared_types(
            &mut atoms,
            &documents,
            &policies,
            "cases",
            Answerer::Proposed,
            &mut on_document,
        )
        .await
    };

    assert_eq!(
        reports[0].records, 1,
        "the evidence-keyed case is retained; the conflicting spin-off is not made novel"
    );
    assert_eq!(
        reports[0].statements, 3,
        "same-anchor local refs remain separate statements"
    );
    assert_eq!(reports[0].outcomes.get("refused:contradiction"), Some(&1));
    assert_eq!(
        failures.len(),
        1,
        "the spin-off remains explicitly unresolved"
    );
    let cases: Vec<_> = entities
        .iter()
        .filter(|entity| entity.entity_type.as_str_repr() == "case")
        .collect();
    assert_eq!(cases.len(), 1);
    let memberships: Vec<_> = claims
        .iter()
        .filter(|claim| claim.claim_kind.as_deref() == Some("membership"))
        .collect();
    assert_eq!(memberships.len(), 3);
    assert_eq!(memberships[0].subject, memberships[2].subject);
    assert!(memberships[1].subject.is_none());
    for claim in &memberships {
        assert_eq!(
            claim.attributes.keys().collect::<Vec<_>>(),
            [SUBJECT_FIELDS_ATTRIBUTE],
            "membership is state-free; it keeps only its subject's readings"
        );
        assert_eq!(
            claim.evidence[0].source_doc_id.as_deref(),
            Some(if claim.id.as_str() == "claim-2" {
                "doc-a"
            } else if claim.id.as_str() == "claim-3" {
                "doc-b"
            } else {
                "doc-a"
            })
        );
    }

    let directory = tempfile::tempdir().unwrap();
    crate::enrichment::atlas::writer::write_atlas_full(
        directory.path(),
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &[],
        &[],
        &argument_reconstructions,
        &positions,
        &oppositions,
        &edges,
        &trajectories,
        &AtlasSeeding::Deferred("document-read adapter test has no embedder"),
    )
    .unwrap();
    let written = understanding_vocab::read::read_atlas_atoms(directory.path()).unwrap();
    let written_claims: Vec<_> = written
        .atoms()
        .iter()
        .filter_map(|atom| match atom {
            AtomEnvelope::Claim(claim) if claim.claim_kind.as_deref() == Some("membership") => {
                Some(claim)
            }
            _ => None,
        })
        .collect();
    assert_eq!(written_claims.len(), 3);
    // The writer keeps each one's subject readings, cited.
    for claim in &written_claims {
        assert_eq!(
            claim.attributes.keys().collect::<Vec<_>>(),
            [SUBJECT_FIELDS_ATTRIBUTE]
        );
        let number = &claim.attributes[SUBJECT_FIELDS_ATTRIBUTE]["number"];
        assert_eq!(number["status"], "supported");
        assert_eq!(number["evidence"], number["value"]);
    }
    assert_eq!(
        written_claims
            .iter()
            .map(|claim| claim.evidence[0].source_doc_id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["doc-a", "doc-a", "doc-b"]
    );
    assert_eq!(
        written_claims
            .iter()
            .filter_map(|claim| claim.subject.as_ref().map(|subject| subject.as_str()))
            .collect::<BTreeSet<_>>()
            .len(),
        1,
        "the confirmed case is written; the spin-off has no fabricated subject"
    );
    let status = written
        .atoms()
        .iter()
        .find_map(|atom| match atom {
            AtomEnvelope::Claim(claim)
                if claim.claim_kind.as_deref() == Some("reported_status") =>
            {
                Some(claim)
            }
            _ => None,
        })
        .expect("declared claim qualifier survives the production writer");
    assert_eq!(status.attributes["status"], "closed");
    assert_eq!(status.evidence[0].source_doc_id.as_deref(), Some("doc-b"));
}

async fn run_open_writer_case(
    particulars: serde_json::Value,
) -> (
    usize,
    usize,
    usize,
    usize,
    Vec<Option<String>>,
    usize,
    Vec<Option<String>>,
) {
    use std::sync::Arc;

    let body = "Two local references appear in this one passage.";
    let rows = [corpus_index::index::EnrichmentChunkRow {
        id: 41,
        content: body.into(),
        title: Some("One source".into()),
        url: Some("https://example.test/doc-a".into()),
        metadata_raw: Some(json!({"id":"doc-a"}).to_string()),
        source_doc_id: Some("doc-a".into()),
    }];
    let documents = SectionDocuments::from_chunk_rows([("sec_1", &[41u64][..])], &rows);
    let mut policies = OntologyPolicies::default();
    policies.shape.types = vec![
        OntologyTypeDecl {
            name: "case".into(),
            kind: TypeKind::Entity,
            description: "A support case".into(),
            identity_criterion: Some("the same issue".into()),
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "membership".into(),
            kind: TypeKind::Claim,
            force: Some(Force::Assertive),
            subject: Some("case".into()),
            ..Default::default()
        },
    ];
    let claim = |id: &str, local_ref: &str| {
        let mut attributes = Map::new();
        attributes.insert(LOCAL_REF_ATTRIBUTE.into(), Value::String(local_ref.into()));
        attributes.insert(
            SOURCE_DOCUMENT_ATTRIBUTE.into(),
            Value::String("doc-a".into()),
        );
        attributes.insert(SUBJECT_FIELDS_ATTRIBUTE.into(), json!({}));
        serde_json::from_value(json!({
            "id": id,
            "content": "This source passage concerns a case.",
            "discourse_act": "assert",
            "epistemic_status": "attributed",
            "scope": "universal",
            "evidence": [{"chunk_id":"sec_1","passage_preview":body}],
            "subject": null,
            "claim_kind": "membership",
            "anchor": body,
            "attributes": attributes,
            "enrichment_depth": "extracted"
        }))
        .unwrap()
    };
    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = vec![claim("claim-a", "local-a"), claim("claim-b", "local-b")];
    let mut argument_reconstructions = Vec::new();
    let mut positions = Vec::new();
    let mut oppositions = Vec::new();
    let mut edges = Vec::new();
    let mut trajectories = BTreeMap::new();
    let (reports, failures) = {
        let mut atoms = BuildAtoms {
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
        let answer = particulars.to_string();
        let infer: crate::types::InferenceFn = Arc::new(move |_, _| {
            let answer = answer.clone();
            Box::pin(async move { Ok(answer) })
        });
        let mut on_document = |_: &str, _: &DocumentResolution| {};
        resolve_declared_types(
            &mut atoms,
            &documents,
            &policies,
            "same-anchor",
            Answerer::Model(&infer),
            &mut on_document,
        )
        .await
    };
    let record_count = reports[0].records;
    let statement_count = reports[0].statements;
    let opened = *reports[0].outcomes.get("opened").unwrap_or(&0);
    let membership_subjects: Vec<Option<String>> = claims
        .iter()
        .filter(|claim| claim.claim_kind.as_deref() == Some("membership"))
        .map(|claim| {
            claim
                .subject
                .as_ref()
                .map(|subject| subject.as_str().to_string())
        })
        .collect();

    let directory = tempfile::tempdir().unwrap();
    crate::enrichment::atlas::writer::write_atlas_full(
        directory.path(),
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &[],
        &[],
        &argument_reconstructions,
        &positions,
        &oppositions,
        &edges,
        &trajectories,
        &AtlasSeeding::Deferred("same-anchor identity test has no embedder"),
    )
    .unwrap();
    let written = understanding_vocab::read::read_atlas_atoms(directory.path()).unwrap();
    let written_cases = written
        .atoms()
        .iter()
        .filter(|atom| {
            matches!(atom, AtomEnvelope::Entity(entity) if entity.entity_type.as_str_repr() == "case")
        })
        .count();
    let written_subjects = written
        .atoms()
        .iter()
        .filter_map(|atom| match atom {
            AtomEnvelope::Claim(claim) if claim.claim_kind.as_deref() == Some("membership") => {
                Some(
                    claim
                        .subject
                        .as_ref()
                        .map(|subject| subject.as_str().to_string()),
                )
            }
            _ => None,
        })
        .collect();
    (
        record_count,
        statement_count,
        opened,
        failures.len(),
        membership_subjects,
        written_cases,
        written_subjects,
    )
}

#[tokio::test]
async fn same_anchor_local_references_follow_explicit_open_and_link_decisions_in_writer() {
    let body = "Two local references appear in this one passage.";
    let independent = json!({
        "particulars": [
            {"same_as":"none", "mentions":[{"statement":"s0", "cite":body}]},
            {"same_as":"none", "mentions":[{"statement":"s1", "cite":body}]}
        ]
    });
    let linked = json!({
        "particulars": [
            {"same_as":"none", "mentions":[
                {"statement":"s0", "cite":body},
                {"statement":"s1", "cite":body}
            ]}
        ]
    });

    let (records, statements, opened, failures, subjects, written_cases, written_subjects) =
        run_open_writer_case(independent).await;
    assert_eq!(
        (records, statements, opened, failures, written_cases),
        (2, 2, 2, 0, 2)
    );
    assert_ne!(subjects[0], subjects[1]);
    let written_set: BTreeSet<_> = written_subjects.into_iter().flatten().collect();
    assert_eq!(written_set.len(), 2);

    let (records, statements, opened, failures, subjects, written_cases, written_subjects) =
        run_open_writer_case(linked).await;
    assert_eq!(
        (records, statements, opened, failures, written_cases),
        (1, 2, 2, 0, 1)
    );
    assert_eq!(subjects[0], subjects[1]);
    assert!(written_subjects.iter().all(Option::is_some));
    assert_eq!(written_subjects[0], written_subjects[1]);
}

/// One support case read from issue text and a label event: `case` with a
/// declared `number` key, a state-free membership and a reported state.
fn local_subject_fixture() -> (
    Vec<corpus_index::index::EnrichmentChunkRow>,
    OntologyPolicies,
) {
    let row = |id: u64, doc: &str, body: &str| corpus_index::index::EnrichmentChunkRow {
        id,
        content: body.into(),
        title: None,
        url: Some(format!("https://example.test/{doc}")),
        metadata_raw: Some(json!({ "id": doc }).to_string()),
        source_doc_id: Some(doc.into()),
    };
    let rows = vec![
        row(51, "issue", ISSUE),
        row(52, "label-event", "Label compatibility added to #1373."),
    ];
    let mut policies = OntologyPolicies::default();
    policies.shape.types = vec![
        OntologyTypeDecl {
            name: "case".into(),
            kind: TypeKind::Entity,
            description: "A support case".into(),
            identity_criterion: Some("the same issue".into()),
            identity: vec!["number".into()],
            attributes: vec![AttrDecl {
                name: "number".into(),
                family: AttrFamily::Text { values: Vec::new() },
                description: "The issue number".into(),
                derived: None,
            }],
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "case_membership".into(),
            kind: TypeKind::Claim,
            force: Some(Force::Assertive),
            subject: Some("case".into()),
            ..Default::default()
        },
        OntologyTypeDecl {
            name: "case_state".into(),
            kind: TypeKind::Claim,
            force: Some(Force::Assertive),
            subject: Some("case".into()),
            attributes: vec![AttrDecl {
                name: "state".into(),
                family: AttrFamily::Text {
                    values: vec!["reported".into(), "fixed".into()],
                },
                description: "The reported case state".into(),
                derived: None,
            }],
            ..Default::default()
        },
    ];
    (rows, policies)
}

const ISSUE: &str = "Support --no-cache-dir as an alias. Running it fails with error: unexpected argument '--no-cache-dir' found. pip accepts the flag.";
const ISSUE_STATE: &str = "error: unexpected argument '--no-cache-dir' found.";

fn local_subject_claim(id: &str, kind: &str, document: &str, anchor: &str, number: &str) -> Claim {
    let mut attributes = Map::new();
    if kind == "case_state" {
        attributes.insert("state".into(), Value::String("reported".into()));
    }
    attributes.insert(LOCAL_REF_ATTRIBUTE.into(), Value::String("#1373".into()));
    attributes.insert(
        SOURCE_DOCUMENT_ATTRIBUTE.into(),
        Value::String(document.into()),
    );
    attributes.insert(
        SUBJECT_FIELDS_ATTRIBUTE.into(),
        json!({ "number": read(number) }),
    );
    serde_json::from_value(json!({
        "id": id,
        "content": "The source reports this about the case.",
        "discourse_act": "assert",
        "epistemic_status": "attributed",
        "scope": "universal",
        "evidence": [{"chunk_id":"sec_1","passage_preview":anchor}],
        "subject": null,
        "claim_kind": kind,
        "anchor": anchor,
        "attributes": attributes,
        "enrichment_depth": "extracted"
    }))
    .unwrap()
}

async fn resolve_local_subjects(
    claims: &mut Vec<Claim>,
    entities: &mut Vec<Entity>,
) -> (Vec<RecordsReport>, Vec<PhaseFailure>) {
    let (rows, policies) = local_subject_fixture();
    let documents = SectionDocuments::from_chunk_rows([("sec_1", &[51u64, 52][..])], &rows);
    let (mut events, mut states, mut relations) = (Vec::new(), Vec::new(), Vec::new());
    let (mut argument_reconstructions, mut positions, mut oppositions) =
        (Vec::new(), Vec::new(), Vec::new());
    let (mut edges, mut trajectories) = (Vec::new(), BTreeMap::new());
    let mut atoms = BuildAtoms {
        entities,
        events: &mut events,
        states: &mut states,
        relations: &mut relations,
        claims,
        argument_reconstructions: &mut argument_reconstructions,
        positions: &mut positions,
        oppositions: &mut oppositions,
        edges: &mut edges,
        trajectories: &mut trajectories,
    };
    let mut on_document = |_: &str, _: &DocumentResolution| {};
    resolve_declared_types(
        &mut atoms,
        &documents,
        &policies,
        "local-subjects",
        Answerer::Proposed,
        &mut on_document,
    )
    .await
}

#[tokio::test]
async fn one_local_subject_at_two_spans_is_one_statement_and_survives_the_writer() {
    let mut claims = vec![
        local_subject_claim("membership", "case_membership", "issue", ISSUE, "1373"),
        local_subject_claim("state", "case_state", "issue", ISSUE_STATE, "1373"),
        local_subject_claim(
            "label",
            "case_membership",
            "label-event",
            "Label compatibility added to #1373.",
            "1373",
        ),
    ];
    let mut entities = Vec::new();
    let (reports, failures) = resolve_local_subjects(&mut claims, &mut entities).await;

    assert_eq!(
        reports[0].statements, 2,
        "the issue's two spans are one statement; the label event's same ref string is its own"
    );
    assert!(failures.is_empty(), "{failures:?}");
    assert!(claims[0].subject.is_some());
    assert_eq!(
        claims[0].subject, claims[1].subject,
        "membership and state share one case"
    );
    assert_eq!(claims[0].anchor.as_deref(), Some(ISSUE));
    assert_eq!(
        claims[1].anchor.as_deref(),
        Some(ISSUE_STATE),
        "each claim keeps its own citation"
    );

    let directory = tempfile::tempdir().unwrap();
    crate::enrichment::atlas::writer::write_atlas_full(
        directory.path(),
        &entities,
        &[],
        &[],
        &[],
        &claims,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &BTreeMap::new(),
        &AtlasSeeding::Deferred("local-subject test has no embedder"),
    )
    .unwrap();
    let written = understanding_vocab::read::read_atlas_atoms(directory.path()).unwrap();
    let written_claim = |id: &str| {
        written
            .atoms()
            .iter()
            .find_map(|atom| match atom {
                AtomEnvelope::Claim(claim) if claim.id.as_str() == id => Some(claim),
                _ => None,
            })
            .unwrap()
    };
    let (membership, state) = (written_claim("membership"), written_claim("state"));
    assert!(membership.subject.is_some());
    assert_eq!(membership.subject, state.subject);
    assert_eq!(state.attributes["state"], "reported");
    assert_eq!(state.anchor.as_deref(), Some(ISSUE_STATE));
}

#[tokio::test]
async fn one_local_subject_whose_spans_disagree_on_an_identity_value_is_not_joined() {
    let mut claims = vec![
        local_subject_claim("membership", "case_membership", "issue", ISSUE, "1373"),
        local_subject_claim("state", "case_state", "issue", ISSUE_STATE, "1374"),
    ];
    let mut entities = Vec::new();
    let (reports, failures) = resolve_local_subjects(&mut claims, &mut entities).await;

    assert_eq!(
        reports[0].statements, 2,
        "a disagreement keeps each span its own statement"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.reason.contains("kept as separate statements")),
        "the disagreement is reported: {failures:?}"
    );
}
