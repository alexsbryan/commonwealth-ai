use serde_json::{Map, Value};
use tracing::debug;

use super::super::atoms::{AtomId, Entity};
use super::super::resolution_ontology::ResolutionPolicy;
use super::super::resolution_sources::SOURCE_EXTRACTOR_ID;
use crate::enrichment::ontology::SourceDecl;
use crate::enrichment::reconciliation::identity_signals::identity_value_of;

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
