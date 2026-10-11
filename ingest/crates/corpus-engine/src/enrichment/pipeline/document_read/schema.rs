use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::enrichment::ontology::{AttrDecl, OntologyPolicies, SourceDecl, TypeIndex, TypeKind};
use crate::enrichment::pipeline::types::{ChapterInput, ChatPrompt};

/// v4: one reader remains (passes); the one-shot prompt, its decoder schema and
/// its citation handles are gone, so reads cached under v3 are not reused.
/// v5: prefill, line classes, Point, Mention and Pick; a v4 read left open
/// fields and references unknown, so it is not reused.
/// v6: a claim carries its subject's readings whole and the projection
/// sketches no subject; a v5 section holds bare values and subject sketches,
/// so it is not reused.
const CONTRACT_VERSION: u32 = 6;

/// The Phase-1 prompt of a declared reading: the plan this chapter will be read
/// by, never dispatched. The passes reader asks its own small questions
/// (`passes::read`); this is what `--dry-run` shows and part of what keys the
/// section cache (`runner_support::cache_text`).
pub fn compose(chapter: &ChapterInput, policies: &OntologyPolicies, phase_id: &str) -> ChatPrompt {
    let documents: Vec<&str> = chapter.source_documents.iter().map(|d| d.key()).collect();
    tracing::debug!(
        chapter = %chapter.chapter_id,
        documents = documents.len(),
        claim_types = policies
            .shape
            .types
            .iter()
            .filter(|claim| eligible_claim(claim, policies))
            .count(),
        "phase1.document_read_plan_composed"
    );
    let user = serde_json::to_string_pretty(&json!({
        "chapter_id": chapter.chapter_id,
        "documents": documents,
        "declared_read_contract": contract_value(policies),
    }))
    .expect("serialising the document-read plan is infallible");
    ChatPrompt::new("", user).with_phase_id(phase_id)
}

pub(super) fn contract_value(policies: &OntologyPolicies) -> Value {
    let index = TypeIndex::from_policies(policies);
    let eligible_claims: Vec<Value> = policies
        .shape
        .types
        .iter()
        .filter(|claim| eligible_claim(claim, policies))
        .map(|claim| {
            json!({
                "name": claim.name,
                "description": claim.description,
                "force": claim.force,
                "subject": claim.subject,
                "scope": claim.scope,
                "attributes": attrs_value(index.extracted_attributes(&claim.name)),
            })
        })
        .collect();
    let subject_names: BTreeSet<&str> = eligible_claims
        .iter()
        .filter_map(|claim| claim.get("subject").and_then(Value::as_str))
        .collect();
    let subjects: Vec<Value> = policies
        .shape
        .types
        .iter()
        .filter(|ty| {
            matches!(ty.kind, TypeKind::Entity | TypeKind::Event)
                && subject_names.contains(ty.name.as_str())
        })
        .map(|ty| {
            json!({
                "name": ty.name,
                "description": ty.description,
                "source": ty.source,
                "attributes": attrs_value(subject_read_attributes(policies, &index, &ty.name)),
            })
        })
        .collect();
    let metadata_sources: Vec<Value> = policies
        .shape
        .types
        .iter()
        .filter_map(|ty| match &ty.source {
            Some(SourceDecl::Metadata(source)) => Some(json!({
                "type": ty.name,
                "metadata": source.metadata,
                "attributes": source.attributes,
                "refs": source.refs,
            })),
            _ => None,
        })
        .collect();
    json!({
        "contract_version": CONTRACT_VERSION,
        "guidance": policies.prose.guidance,
        "voices": policies.assertion.voices,
        "claim_types": eligible_claims,
        "subject_types": subjects,
        "metadata_sources": metadata_sources,
        "passes": super::passes::contract_value(policies),
    })
}

fn attrs_value(attrs: Vec<&AttrDecl>) -> Vec<Value> {
    attrs
        .into_iter()
        .filter(|attr| attr.derived.is_none())
        .map(|attr| serde_json::to_value(attr).expect("attribute declarations are serializable"))
        .collect()
}

/// Fields a read may use for one declared subject: all extracted fields for a
/// source-free record, only declared identity keys for a metadata projection.
pub(super) fn subject_read_attributes<'a>(
    policies: &'a OntologyPolicies,
    index: &TypeIndex<'a>,
    subject_name: &str,
) -> Vec<&'a AttrDecl> {
    let Some(subject) = policies.type_decl(subject_name) else {
        return Vec::new();
    };
    let attributes = index.extracted_attributes(subject_name);
    match subject.source.as_ref() {
        None => attributes,
        Some(SourceDecl::Metadata(_)) => {
            let identity = index.effective_identity(subject_name);
            attributes
                .into_iter()
                .filter(|attribute| {
                    attribute.derived.is_none() && identity.iter().any(|key| key == &attribute.name)
                })
                .collect()
        }
        Some(SourceDecl::Table(_)) => Vec::new(),
    }
}

pub(super) fn eligible_claim(
    claim: &crate::enrichment::ontology::OntologyTypeDecl,
    policies: &OntologyPolicies,
) -> bool {
    claim.is_document_reading_eligible(&policies.shape.types)
}
