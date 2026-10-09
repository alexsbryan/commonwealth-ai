use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{
    AttrDecl, DocumentReader, OntologyPolicies, SourceDecl, TypeIndex, TypeKind,
};
use crate::enrichment::pipeline::pipelines::ontology_schema::attribute_schema;
use crate::enrichment::pipeline::types::{ChapterInput, ChatPrompt};

/// v3 binds decoder citations to the actual supplied source, before projection.
const CONTRACT_VERSION: u32 = 3;
const SYSTEM: &str = include_str!("../document_read_system.md");

/// Compose the small, declared-only prompt over the already-batched documents.
pub fn compose(chapter: &ChapterInput, policies: &OntologyPolicies, phase_id: &str) -> ChatPrompt {
    tracing::debug!(
        chapter = %chapter.chapter_id,
        documents = chapter.source_documents.len(),
        claim_types = policies
            .shape
            .types
            .iter()
            .filter(|claim| eligible_claim(claim, policies))
            .count(),
        "phase1.document_read_prompt_composed"
    );
    let documents: Vec<Value> = chapter
        .source_documents
        .iter()
        .map(|document| {
            json!({
                "document_id": document.key(),
                "source_doc_id": document.source_doc_id(),
                "title": document.title(),
                "url": document.url(),
                "metadata": document.metadata(),
                "metadata_source_references": metadata_references(document, policies),
                "citation_choices": super::citations::choices(document),
                "body": document.raw_body(),
            })
        })
        .collect();
    let user = format!(
        "Read the supplied documents independently.\n\n{}",
        serde_json::to_string_pretty(&json!({
            "chapter_id": chapter.chapter_id,
            "declared_read_contract": contract_value(policies),
            "documents": documents,
        }))
        .expect("serialising JSON document-read input is infallible")
    );
    ChatPrompt::new(SYSTEM, user)
        .with_response_schema("declared_document_read", schema_for(chapter, policies))
        .with_phase_id(phase_id)
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
        .filter(|ty| ty.kind == TypeKind::Entity && subject_names.contains(ty.name.as_str()))
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
    let mut contract = json!({
        "contract_version": CONTRACT_VERSION,
        "system_prompt": SYSTEM,
        "response_schema": schema_for_ids(&[], policies),
        "document_reading": policies.document_reading,
        "guidance": policies.prose.guidance,
        "voices": policies.assertion.voices,
        "claim_types": eligible_claims,
        "subject_types": subjects,
        "metadata_sources": metadata_sources,
    });
    // Reads from the two readers never answer for each other in a cache; the
    // one-shot contract keeps its bytes, so its cached reads stay valid.
    if policies.document_reader == DocumentReader::Passes {
        contract["passes"] = super::passes::contract_value(policies);
    }
    contract
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

fn schema_for(chapter: &ChapterInput, policies: &OntologyPolicies) -> Value {
    let document_ids: Vec<&str> = chapter
        .source_documents
        .iter()
        .map(SourceDocument::key)
        .collect();
    let mut schema = schema_for_ids(&document_ids, policies);
    let templates = schema["properties"]["documents"]["items"]["oneOf"]
        .as_array()
        .expect("document outcomes have decoder branches")
        .clone();
    let branches: Vec<Value> = chapter
        .source_documents
        .iter()
        .flat_map(|document| {
            templates.iter().cloned().map(move |mut branch| {
                branch["properties"]["document_id"] = json!({"const":document.key()});
                super::citations::constrain(&mut branch, document);
                branch
            })
        })
        .collect();
    if !branches.is_empty() {
        schema["properties"]["documents"]["items"]["oneOf"] = json!(branches);
    }
    schema
}

fn schema_for_ids(document_ids: &[&str], policies: &OntologyPolicies) -> Value {
    let index = TypeIndex::from_policies(policies);
    let claims: Vec<_> = policies
        .shape
        .types
        .iter()
        .filter(|claim| eligible_claim(claim, policies))
        .collect();
    let claim_kinds: Vec<&str> = claims.iter().map(|claim| claim.name.as_str()).collect();
    tracing::debug!(
        target: "corpus_engine::document_read",
        claim_kinds = ?claim_kinds,
        documents = document_ids.len(),
        "declared claim types selected for accountable document reading"
    );
    let claim_branches: Vec<Value> = claims
        .iter()
        .map(|claim| {
            let subject = claim.subject.as_deref().unwrap_or_default();
            json!({
                "type": "object",
                "properties": {
                    "kind": {"const": claim.name},
                    "content": {"type": "string", "minLength": 1},
                    "subject_type": {"const": subject},
                    "subject_local_ref": {"type": "string", "minLength": 1, "maxLength": 120},
                    "subject_name": {"type": "string", "minLength": 1},
                    "speaker": {"type": ["string", "null"]},
                    "evidence": {"type": "string", "minLength": 1},
                    "fields": field_object(index.extracted_attributes(&claim.name)),
                    "subject_fields": field_object(subject_read_attributes(policies, &index, subject)),
                },
                "required": ["kind", "content", "subject_type", "subject_local_ref", "subject_name", "evidence", "fields", "subject_fields"],
                "additionalProperties": false,
            })
        })
        .collect();
    let mut branches = outcome_branches(&claim_branches);
    if !document_ids.is_empty() {
        for branch in &mut branches {
            branch["properties"]["document_id"] = json!({"type": "string", "enum": document_ids});
        }
    }
    let mut documents = json!({
        "type": "array",
        "items": {"oneOf": branches},
    });
    if !document_ids.is_empty() {
        documents["minItems"] = json!(document_ids.len());
        documents["maxItems"] = json!(document_ids.len());
    }
    json!({
        "type": "object",
        "properties": {"documents": documents},
        "required": ["documents"],
        "additionalProperties": false,
    })
}

/// One `supported`/`unknown` contract per declared attribute, with the
/// attribute set REQUIRED and no others allowed — a kind's bag cannot borrow
/// a sibling kind's field, which is what a union lets the model do.
fn field_object(attrs: Vec<&AttrDecl>) -> Value {
    let names: Vec<&str> = attrs.iter().map(|attr| attr.name.as_str()).collect();
    let properties: Map<String, Value> = attrs
        .iter()
        .map(|attr| (attr.name.clone(), field_value(attr)))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": names,
        "additionalProperties": false,
    })
}

fn field_value(attr: &AttrDecl) -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "status": {"const": "supported"},
                    "value": attribute_schema(attr),
                    "evidence": {"type": "string", "minLength": 1},
                },
                "required": ["status", "value", "evidence"],
                "additionalProperties": false,
            },
            {
                "type": "object",
                "properties": {
                    "status": {"const": "unknown"},
                    "reason": {"type": "string", "minLength": 1},
                },
                "required": ["status", "reason"],
                "additionalProperties": false,
            }
        ]
    })
}

/// The four outcome meanings as four decoder branches: `read` carries at
/// least one claim; an abstention carries none and must name its reason.
fn outcome_branches(claim_branches: &[Value]) -> Vec<Value> {
    let read = json!({
        "type": "object",
        "properties": {
            "document_id": {"type": "string", "minLength": 1},
            "status": {"const": "read"},
            "reason": {"type": ["string", "null"]},
            "claims": {"type": "array", "minItems": 1, "items": {"oneOf": claim_branches}},
        },
        "required": ["document_id", "status", "claims"],
        "additionalProperties": false,
    });
    let abstain = |status: &str| {
        json!({
            "type": "object",
            "properties": {
                "document_id": {"type": "string", "minLength": 1},
                "status": {"const": status},
                "reason": {"type": "string", "minLength": 1},
                "claims": {"type": "array", "maxItems": 0},
            },
            "required": ["document_id", "status", "reason", "claims"],
            "additionalProperties": false,
        })
    };
    vec![
        read,
        abstain("nothing_applicable"),
        abstain("could_not_judge"),
        abstain("not_read"),
    ]
}

fn metadata_references(document: &SourceDocument, policies: &OntologyPolicies) -> Vec<Value> {
    policies
        .shape
        .types
        .iter()
        .filter_map(|ty| {
            let Some(SourceDecl::Metadata(source)) = &ty.source else {
                return None;
            };
            let fields: Map<String, Value> = source
                .metadata
                .iter()
                .filter_map(|field| {
                    document
                        .metadata()
                        .get(field)
                        .cloned()
                        .map(|value| (field.clone(), value))
                })
                .collect();
            (!fields.is_empty()).then(|| json!({"type": ty.name, "fields": fields}))
        })
        .collect()
}
