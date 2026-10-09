use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::super::super::ann_store::AtlasSeeding;
use super::super::super::atoms::{AtomEnvelope, AtomId};
use super::super::super::resolution::{resolve_entities_and_events_with, resolve_step_3b_with};
use super::super::super::resolution_documents::SectionDocuments;
use super::super::super::resolution_ontology::ResolutionPolicy;
use super::super::super::resolution_sources::project_source_atoms;
use super::super::super::writer::write_atlas_full;
use crate::enrichment::ontology::OntologyV1;
use crate::enrichment::pipeline::document_read::{
    parse_response, validate_and_stamp, DocumentReadField,
};
use crate::enrichment::pipeline::types::{ChapterInput, PhaseFailureKind};
use crate::types::EmbedFn;

const BODY: &str = "Alice Hart writes: “I will send the report Friday.”";
const ALICE_EMAIL: &str = "alice@example.test";
const CARRIER_EMAIL: &str = "review@example.test";

fn policies() -> crate::enrichment::ontology::OntologyPolicies {
    let ontology: OntologyV1 = toml::from_str(
        r#"

[[types]]
name = "person"
kind = "entity"
attributes = [
  { name = "email", type = "text" },
  { name = "name", type = "text" },
]
identity = ["email"]
source = { metadata = ["from", "to", "cc"], attributes = { email = "address", name = "display_name" } }

[[types]]
name = "commitment"
kind = "claim"
force = "commissive"
subject = "person"
attributes = [{ name = "due", type = "time" }]
"#,
    )
    .unwrap();
    ontology.into_policies()
}

fn chapter() -> (SectionDocuments, ChapterInput) {
    let rows = [corpus_index::index::EnrichmentChunkRow {
        id: 41,
        content: BODY.into(),
        title: Some("Forwarded commitment".into()),
        url: Some("https://example.test/message/41".into()),
        metadata_raw: Some(
            json!({
                "id":"doc-1",
                "from":"Review Bot <review@example.test>",
                "to":"Alice Hart <alice@example.test>",
                "cc":"",
                "author":"Review Bot"
            })
            .to_string(),
        ),
        source_doc_id: Some("doc-1".into()),
    }];
    let documents = SectionDocuments::from_chunk_rows([("sec_1", &[41u64][..])], &rows);
    let chapter = ChapterInput {
        chapter_id: "sec_1".into(),
        title: "Forwarded commitment".into(),
        approx_tokens: BODY.len() / 4,
        text: BODY.into(),
        metadata: HashMap::new(),
        source_documents: documents.documents_for_section("sec_1").to_vec(),
    };
    (documents, chapter)
}

fn read_claim(
    local_ref: &str,
    subject_name: &str,
    speaker: Option<&str>,
    evidence: &str,
    email: Value,
) -> Value {
    json!({
        "kind":"commitment",
        "content":"The quoted author commits to send the report Friday.",
        "subject_type":"person",
        "subject_local_ref":local_ref,
        "subject_name":subject_name,
        "speaker":speaker,
        "evidence":evidence,
        "fields":{
            "due":{"status":"unknown","reason":"No due date is stated in the source passage."}
        },
        "subject_fields":{"email":email}
    })
}

fn section(
    chapter: &ChapterInput,
    policies: &crate::enrichment::ontology::OntologyPolicies,
) -> crate::enrichment::pipeline::atlas::SectionExtraction {
    let raw = json!({
        "documents":[{
            "document_id":"doc-1",
            "status":"read",
            "claims":[
                read_claim(
                    "alice-ref",
                    "quoted author",
                    Some("Alice Hart"),
                    BODY,
                    json!({"status":"supported","value":ALICE_EMAIL,"evidence":ALICE_EMAIL})
                ),
                read_claim(
                    "quoted-i-without-identity",
                    "Review Bot",
                    None,
                    "I will send the report Friday.",
                    json!({"status":"unknown","reason":"The quoted first-person voice has no supplied email identity."})
                )
            ]
        }]
    })
    .to_string();
    let mut parsed = parse_response(&raw, policies).unwrap();
    let mut section = parsed.section_extraction.take().unwrap();
    section.section_id = chapter.chapter_id.clone();
    validate_and_stamp(chapter, policies, &mut section).unwrap();
    section
}

async fn resolve(
    section: &crate::enrichment::pipeline::atlas::SectionExtraction,
    sources: Vec<crate::enrichment::atlas::atoms::Entity>,
    policies: &crate::enrichment::ontology::OntologyPolicies,
) -> (
    crate::enrichment::atlas::resolution::ResolutionOutput,
    crate::enrichment::atlas::resolution::Step3bOutput,
) {
    let embed: EmbedFn = Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0, 1.0, 0.0]) }));
    let policy = ResolutionPolicy::new(policies);
    let resolution =
        resolve_entities_and_events_with(std::slice::from_ref(section), &embed, &policy, sources)
            .await
            .unwrap();
    let step3b = resolve_step_3b_with(
        std::slice::from_ref(section),
        &resolution.entities,
        &resolution.events,
        &policy,
    )
    .unwrap();
    (resolution, step3b)
}

#[tokio::test]
async fn metadata_subject_identity_binds_after_source_projection_and_writer() {
    let policies = policies();
    let (documents, chapter) = chapter();
    let section = section(&chapter, &policies);
    assert_eq!(
        section.entities_introduced.len(),
        0,
        "a sourced subject is not re-extracted"
    );
    assert_eq!(
        section.claims[0].attributed_to.as_deref(),
        Some("Alice Hart")
    );
    assert!(
        section.claims[1].attributed_to.is_none(),
        "quoted I is not defaulted to the header author"
    );
    assert!(matches!(
        section.document_read.as_ref().unwrap().documents[0].claims[1].subject_fields["email"],
        DocumentReadField::Unknown { .. }
    ));
    assert!(matches!(
        &section.document_read.as_ref().unwrap().documents[0].claims[0].fields["due"],
        DocumentReadField::Unknown { reason }
            if reason == "No due date is stated in the source passage."
    ));
    let replay: crate::enrichment::pipeline::atlas::SectionExtraction =
        serde_json::from_value(serde_json::to_value(&section).unwrap()).unwrap();
    assert!(matches!(
        &replay.document_read.as_ref().unwrap().documents[0].claims[0].fields["due"],
        DocumentReadField::Unknown { reason }
            if reason == "No due date is stated in the source passage."
    ));

    let projection = project_source_atoms(&documents, &policies, "ward-source-subject").unwrap();
    assert_eq!(
        projection.atoms.len(),
        2,
        "From and To project the existing people once each"
    );
    assert!(
        projection.atoms.iter().any(|entity| {
            entity.attributes.get("email").and_then(Value::as_str) == Some(CARRIER_EMAIL)
        }),
        "the From-header author remains separately source-projected"
    );
    let alice = projection
        .atoms
        .iter()
        .find(|entity| entity.attributes.get("email").and_then(Value::as_str) == Some(ALICE_EMAIL))
        .expect("the exact To identity is source-projected");
    let alice_id = alice.id.clone();
    let source_entities = projection.atoms.clone();
    let (resolution, step3b) = resolve(&section, source_entities, &policies).await;
    let unresolved: Vec<_> = step3b
        .failures
        .iter()
        .filter(|failure| failure.kind == PhaseFailureKind::UnresolvedClaimSubject)
        .collect();
    assert_eq!(unresolved.len(), 1);
    assert!(
        unresolved[0].reason.contains("identity field `email`"),
        "{unresolved:?}"
    );
    assert_eq!(step3b.claims[0].subject.as_ref(), Some(&alice_id));
    assert_eq!(step3b.claims[0].attributed_to.as_ref(), Some(&alice_id));
    assert!(step3b.claims[1].subject.is_none());
    assert!(step3b.claims[1].attributed_to.is_none());
    assert_eq!(step3b.claims.len(), 2);

    let mut edges = resolution.edges.clone();
    edges.extend(step3b.edges.iter().cloned());
    let directory = tempfile::tempdir().unwrap();
    write_atlas_full(
        directory.path(),
        &resolution.entities,
        &resolution.events,
        &step3b.states,
        &step3b.relations,
        &step3b.claims,
        &step3b.questions,
        &[],
        &step3b.argument_reconstructions,
        &[],
        &[],
        &edges,
        &step3b.trajectories,
        &AtlasSeeding::Deferred("source subject test does not embed"),
    )
    .unwrap();
    let written = understanding_vocab::read::read_atlas_atoms(directory.path()).unwrap();
    let written_people: Vec<_> = written
        .atoms()
        .iter()
        .filter_map(|atom| match atom {
            AtomEnvelope::Entity(entity) if entity.entity_type.as_str_repr() == "person" => {
                Some(entity)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        written_people.len(),
        2,
        "no duplicate person atoms were made"
    );
    let written_commitments: Vec<_> = written
        .atoms()
        .iter()
        .filter_map(|atom| match atom {
            AtomEnvelope::Claim(claim) if claim.claim_kind.as_deref() == Some("commitment") => {
                Some(claim)
            }
            _ => None,
        })
        .collect();
    assert_eq!(written_commitments.len(), 2);
    assert_eq!(written_commitments[0].subject.as_ref(), Some(&alice_id));
    assert!(written_commitments[1].subject.is_none());
    assert!(written_commitments.iter().all(|claim| {
        !claim.attributes.contains_key("due")
            && !claim
                .attributes
                .contains_key(crate::enrichment::pipeline::document_read::SUBJECT_FIELDS_ATTRIBUTE)
    }));
    assert_eq!(
        written_commitments[0].evidence[0].source_doc_id.as_deref(),
        Some("doc-1")
    );
}

#[tokio::test]
async fn ambiguous_projected_identity_is_refused_and_counted() {
    let policies = policies();
    let (documents, chapter) = chapter();
    let section = section(&chapter, &policies);
    let projection = project_source_atoms(&documents, &policies, "ward-source-subject").unwrap();
    let mut ambiguous_sources = projection.atoms;
    let mut duplicate = ambiguous_sources
        .iter()
        .find(|entity| entity.attributes.get("email").and_then(Value::as_str) == Some(ALICE_EMAIL))
        .expect("the exact To identity is projected")
        .clone();
    duplicate.id = AtomId::exact_entity_content_hash(
        "duplicate-alice-source",
        &duplicate.entity_type,
        "ward-source-subject",
    );
    ambiguous_sources.push(duplicate);

    let (_, step3b) = resolve(&section, ambiguous_sources, &policies).await;
    assert!(step3b.claims[0].subject.is_none());
    assert!(step3b.failures.iter().any(|failure| {
        failure.kind == PhaseFailureKind::UnresolvedClaimSubject
            && failure
                .reason
                .contains("multiple projected metadata-backed")
    }));
}
