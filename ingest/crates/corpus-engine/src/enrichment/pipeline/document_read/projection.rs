use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::atlas::{
    ClaimScope, ClaimSketch, EnrichmentDepth, EntitySketch, EntityType, EpistemicStatus,
    SectionExtraction,
};
use crate::enrichment::pipeline::pipelines::literary::prepare_phase_json;
use crate::enrichment::pipeline::types::Phase1ChapterResult;
use crate::error::{Error, Result};

use super::schema;
use super::validation;
use super::{
    DocumentRead, DocumentReadOutcome, CLAIM_FIELDS_ATTRIBUTE, LOCAL_REF_ATTRIBUTE,
    SOURCE_DOCUMENT_ATTRIBUTE, SUBJECT_FIELDS_ATTRIBUTE,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRead {
    documents: Vec<DocumentReadOutcome>,
}

/// Parse only the dedicated document envelope; no generic facet or question is required.
pub fn parse_response(response: &str, policies: &OntologyPolicies) -> Result<Phase1ChapterResult> {
    let cleaned = prepare_phase_json(response, "phase 1 (document read)")?;
    let raw: RawRead = serde_json::from_str(&cleaned).map_err(|error| {
        Error::Serialization(format!(
            "phase 1 (document read) response is not valid JSON: {error}"
        ))
    })?;
    let mut extraction = SectionExtraction {
        enrichment_depth: EnrichmentDepth::Extracted,
        document_read: Some(DocumentRead {
            contract_fingerprint: super::cache::contract_fingerprint(policies),
            context_fingerprint: String::new(),
            documents: raw.documents,
        }),
        ..Default::default()
    };
    project_compatibility_sketches(&mut extraction, policies)?;
    Ok(Phase1ChapterResult {
        questions: Vec::new(),
        reveals: None,
        thematic_carriers: Vec::new(),
        setting: None,
        plot: None,
        section_extraction: Some(extraction),
    })
}

pub(super) fn project_compatibility_sketches(
    extraction: &mut SectionExtraction,
    policies: &OntologyPolicies,
) -> Result<()> {
    let index = TypeIndex::from_policies(policies);
    let claims = policies
        .shape
        .types
        .iter()
        .filter(|ty| schema::eligible_claim(ty, policies));
    let claim_decls: HashMap<&str, _> = claims.map(|ty| (ty.name.as_str(), ty)).collect();
    let mut subjects: BTreeMap<(String, String, String), EntitySketch> = BTreeMap::new();
    let mut sketches = Vec::new();
    let read = extraction.document_read.as_ref().ok_or_else(|| {
        Error::Serialization("document-read projection has no source carrier".into())
    })?;
    for outcome in &read.documents {
        for claim in &outcome.claims {
            let decl = claim_decls.get(claim.kind.as_str()).ok_or_else(|| {
                Error::Serialization(format!(
                    "undeclared or ineligible claim kind `{}`",
                    claim.kind
                ))
            })?;
            let subject_type = decl.subject.as_deref().ok_or_else(|| {
                Error::Serialization(format!(
                    "claim type `{}` has no subject declaration",
                    decl.name
                ))
            })?;
            if subject_type != claim.subject_type {
                return Err(Error::Serialization(format!(
                    "claim `{}` declares subject `{subject_type}`, not `{}`",
                    claim.kind, claim.subject_type
                )));
            }
            validation::validate_fields(
                &claim.fields,
                index.extracted_attributes(&decl.name),
                &format!("claim `{}` fields", decl.name),
            )?;
            let subject_decl = policies.type_decl(subject_type).ok_or_else(|| {
                Error::Serialization(format!("claim subject type `{subject_type}` is undeclared"))
            })?;
            let subject_attributes =
                schema::subject_read_attributes(policies, &index, subject_type);
            validation::validate_fields(
                &claim.subject_fields,
                subject_attributes,
                &format!("subject `{subject_type}` fields"),
            )?;
            let mut attributes = supported_values(&claim.fields);
            attributes.insert(
                CLAIM_FIELDS_ATTRIBUTE.into(),
                serde_json::to_value(&claim.fields).map_err(|error| {
                    Error::Serialization(format!(
                        "document-read field evidence cannot be serialized: {error}"
                    ))
                })?,
            );
            attributes.insert(
                LOCAL_REF_ATTRIBUTE.into(),
                Value::String(claim.subject_local_ref.clone()),
            );
            attributes.insert(
                SOURCE_DOCUMENT_ATTRIBUTE.into(),
                Value::String(outcome.document_id.clone()),
            );
            attributes.insert(
                SUBJECT_FIELDS_ATTRIBUTE.into(),
                Value::Object(supported_values(&claim.subject_fields)),
            );
            if subject_decl.source.is_none() {
                let key = (
                    outcome.document_id.clone(),
                    subject_type.to_string(),
                    claim.subject_local_ref.clone(),
                );
                let entity = EntitySketch {
                    canonical_name: claim.subject_name.clone(),
                    aliases: Vec::new(),
                    entity_type: EntityType::Other(subject_type.to_string()),
                    description: String::new(),
                    defining_quote: None,
                    anchor: claim.evidence.clone(),
                    attributes: supported_values(&claim.subject_fields),
                };
                // A disagreeing repeat of a local reference is refused by
                // validation, not here: the FIRST occurrence stands, and one
                // inconsistent claim must not kill its whole chapter. The
                // runner validates before this extraction is ever persisted.
                subjects.entry(key).or_insert(entity);
            }
            let force = decl.force.ok_or_else(|| {
                Error::Serialization(format!(
                    "claim type `{}` has no declared force for document reading",
                    decl.name
                ))
            })?;
            let discourse_act =
                crate::enrichment::pipeline::pipelines::parse_policy::discourse_act_for(force);
            let scope = decl
                .scope
                .map(crate::enrichment::pipeline::pipelines::parse_policy::claim_scope_for)
                .unwrap_or(ClaimScope::Universal);
            sketches.push(ClaimSketch {
                content: claim.content.trim().to_string(),
                discourse_act,
                epistemic_status: EpistemicStatus::Attributed,
                attributed_to: claim.speaker.clone(),
                quotable_excerpt: Some(claim.evidence.clone()),
                anchor: claim.evidence.clone(),
                claim_kind: Some(claim.kind.clone()),
                subject: Some(claim.subject_name.clone()),
                scope: Some(scope),
                attributes,
            });
        }
    }
    extraction
        .entities_introduced
        .extend(subjects.into_values());
    extraction.claims.extend(sketches);
    Ok(())
}

fn supported_values(fields: &BTreeMap<String, super::DocumentReadField>) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|(name, field)| match field {
            super::DocumentReadField::Supported { value, .. } => {
                Some((name.clone(), value.clone()))
            }
            super::DocumentReadField::Unknown { .. } => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
