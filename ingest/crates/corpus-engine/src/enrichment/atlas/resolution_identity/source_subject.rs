use std::collections::HashMap;

use serde_json::{Map, Value};
use tracing::debug;

use super::super::atoms::{AtomId, Entity};
use super::super::resolution_ontology::ResolutionPolicy;
use super::super::resolution_sources::SOURCE_EXTRACTOR_ID;
use super::{BoundClaimSubject, TypedSubjectPools};
use crate::enrichment::ontology::SourceDecl;
use crate::enrichment::pipeline::atlas::ClaimSketch;
use crate::enrichment::pipeline::document_read::{
    field_readings, supported_values, SUBJECT_FIELDS_ATTRIBUTE,
};
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind, PipelinePhase};
use crate::enrichment::reconciliation::identity_signals::identity_value_of;

#[allow(clippy::too_many_arguments)]
pub fn bind_claim_subject<'a>(
    sketch: &'a ClaimSketch,
    section_id: &str,
    sketch_index: usize,
    policy: &ResolutionPolicy<'_>,
    entities: &[Entity],
    name_index: &HashMap<String, AtomId>,
    token_index: &HashMap<String, Vec<AtomId>>,
    typed_pools: &mut TypedSubjectPools,
    failures: &mut Vec<PhaseFailure>,
) -> BoundClaimSubject<'a> {
    let declared_subject = super::declared_subject_type(policy, sketch.claim_kind.as_deref());
    let left = declared_subject
        .is_some_and(|ty| super::super::resolution_records::decides(policy.index(), ty));
    let document_read = sketch
        .attributes
        .contains_key(crate::enrichment::pipeline::document_read::LOCAL_REF_ATTRIBUTE);
    let metadata_subject = declared_subject
        .and_then(|type_name| policy.index().get(type_name))
        .is_some_and(|decl| decl.source.is_some());
    let source_document = sketch
        .attributes
        .get(crate::enrichment::pipeline::document_read::SOURCE_DOCUMENT_ATTRIBUTE)
        .and_then(Value::as_str);
    let mut source_identity_failure = None;
    let subject = if document_read && metadata_subject {
        let source_document_for_binding = source_document.filter(|document| !document.is_empty());
        let subject_fields = sketch
            .attributes
            .get(SUBJECT_FIELDS_ATTRIBUTE)
            .map(|readings| field_readings(readings).map(|readings| supported_values(&readings)));
        match (
            declared_subject,
            source_document_for_binding,
            subject_fields,
        ) {
            (Some(type_name), Some(_), Some(Err(why))) => {
                debug!(subject_type = type_name, claim = %sketch.content, %why, "atlas/resolution 3b: subject readings unreadable; claim stays unbound");
                source_identity_failure = Some(format!(
                    "document-read claim for metadata-backed `{type_name}`: its subject's {why}"
                ));
                None
            }
            (Some(type_name), Some(_), Some(Ok(fields))) => {
                match resolve_metadata_source_subject(type_name, &fields, policy, entities) {
                    Ok(id) => Some(id),
                    Err(reason) => {
                        debug!(
                            subject_type = type_name,
                            claim = %sketch.content,
                            %reason,
                            "atlas/resolution 3b: metadata-backed claim subject remains unbound"
                        );
                        source_identity_failure = Some(reason);
                        None
                    }
                }
            }
            (Some(type_name), _, _) => {
                source_identity_failure = Some(format!(
                    "document-read claim for metadata-backed `{type_name}` lacks its source document or identity carrier"
                ));
                None
            }
            (None, _, _) => None,
        }
    } else {
        sketch.subject.as_ref().filter(|_| !left).and_then(|name| {
            let resolved = super::resolve_within_declared_type(
                name,
                declared_subject,
                policy,
                entities,
                name_index,
                token_index,
                typed_pools,
            );
            if resolved.is_none() {
                failures.push(PhaseFailure {
                    phase: PipelinePhase::Questions,
                    subject: format!("sketch:claim:{section_id}#{sketch_index}"),
                    kind: PhaseFailureKind::UnresolvedClaimSubject,
                    reason: match declared_subject {
                        Some(ty) => format!(
                            "claim subject `{}` did not resolve to a `{ty}` — `{}` \
                             declares subject = `{ty}` (claim content: `{}`)",
                            name,
                            sketch.claim_kind.as_deref().unwrap_or("?"),
                            sketch.content.trim()
                        ),
                        None => format!(
                            "claim subject `{}` did not resolve (claim content: `{}`)",
                            name,
                            sketch.content.trim()
                        ),
                    },
                    raw_response_head: None,
                });
            }
            resolved
        })
    };
    if let Some(reason) = source_identity_failure {
        failures.push(PhaseFailure {
            phase: PipelinePhase::Questions,
            subject: format!("sketch:claim:{section_id}#{sketch_index}"),
            kind: PhaseFailureKind::UnresolvedClaimSubject,
            reason,
            raw_response_head: None,
        });
    }
    // The subject's readings stay: they cite the identity this claim was
    // bound by.
    let mut attributes = sketch.attributes.clone();
    if document_read && metadata_subject {
        attributes.remove(crate::enrichment::pipeline::document_read::LOCAL_REF_ATTRIBUTE);
        attributes.remove(crate::enrichment::pipeline::document_read::SOURCE_DOCUMENT_ATTRIBUTE);
    }
    BoundClaimSubject {
        subject,
        source_document,
        document_read,
        attributes,
    }
}

/// Resolve a supplied identity to an entity already made by metadata source
/// projection. Names and fuzzy matches are deliberately not consulted here.
pub fn resolve_metadata_source_subject(
    subject_type: &str,
    supplied_fields: &Map<String, Value>,
    policy: &ResolutionPolicy<'_>,
    entities: &[Entity],
) -> Result<AtomId, String> {
    let declaration = policy
        .index()
        .get(subject_type)
        .ok_or_else(|| format!("subject type `{subject_type}` is undeclared"))?;
    if !matches!(declaration.source.as_ref(), Some(SourceDecl::Metadata(_))) {
        return Err(format!(
            "subject type `{subject_type}` is not backed by a metadata source"
        ));
    }
    let keys = policy.index().effective_identity(subject_type);
    if keys.is_empty() {
        debug!(
            subject_type,
            "atlas/resolution 3b: source subject has no identity keys"
        );
        return Err(format!(
            "metadata-backed subject type `{subject_type}` declares no effective identity field"
        ));
    }
    let mut supplied_identity = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(value) = supplied_fields.get(key).and_then(identity_value_of) else {
            debug!(
                subject_type,
                key, "atlas/resolution 3b: source subject identity field is absent or unknown"
            );
            return Err(format!(
                "document-read subject is missing supported identity field `{key}` for `{subject_type}`"
            ));
        };
        supplied_identity.push((key.as_str(), value));
    }

    let mut matches = Vec::<AtomId>::new();
    for entity in entities {
        if entity.provenance.extractor_id != SOURCE_EXTRACTOR_ID
            || !policy.accepts(subject_type, entity.entity_type.as_str_repr())
        {
            continue;
        }
        let agrees = supplied_identity.iter().all(|(key, expected)| {
            entity
                .attributes
                .get(*key)
                .and_then(identity_value_of)
                .as_ref()
                == Some(expected)
        });
        if agrees && !matches.iter().any(|candidate| candidate == &entity.id) {
            matches.push(entity.id.clone());
        }
    }
    debug!(
        subject_type,
        identity_fields = ?supplied_identity.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        projected_matches = matches.len(),
        "atlas/resolution 3b: exact metadata-source subject lookup"
    );
    match matches.as_slice() {
        [entity] => Ok(entity.clone()),
        [] => Err(format!(
            "no projected metadata-backed `{subject_type}` matches the supplied identity fields"
        )),
        _ => Err(format!(
            "multiple projected metadata-backed `{subject_type}` entities match the supplied identity fields"
        )),
    }
}

#[cfg(test)]
#[path = "source_subject_tests.rs"]
mod tests;
