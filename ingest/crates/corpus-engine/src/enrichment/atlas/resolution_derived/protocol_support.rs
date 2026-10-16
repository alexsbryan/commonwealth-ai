// SPDX-License-Identifier: AGPL-3.0-or-later
//! The protocol fold's reading of one claim (its declared qualifications and
//! fields, as its source supports them) and the audit history it keeps.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::enrichment::ontology::derived::ProtocolRuleDecl;
use crate::enrichment::pipeline::document_read::{
    field_evidence_exists, DocumentReadField, CLAIM_FIELDS_ATTRIBUTE,
};

pub(super) fn matches_rule(
    claim: &crate::enrichment::atlas::atoms::Claim,
    source_document: Option<&crate::enrichment::atlas::SourceDocument>,
    rule: &ProtocolRuleDecl,
) -> Result<bool, Vec<String>> {
    let mut missing = BTreeSet::new();
    let mut mismatch = false;
    for (field, expected) in &rule.when {
        match source_field_value(claim, source_document, field) {
            Ok(actual) if actual == *expected => {}
            Ok(_) => mismatch = true,
            Err(reason) => {
                missing.insert(format!("`{field}`: {reason}"));
            }
        }
    }
    if mismatch {
        Ok(false)
    } else if missing.is_empty() {
        Ok(true)
    } else {
        Err(missing.into_iter().collect())
    }
}

pub(super) fn source_field_value(
    claim: &crate::enrichment::atlas::atoms::Claim,
    source_document: Option<&crate::enrichment::atlas::SourceDocument>,
    field: &str,
) -> Result<String, String> {
    let carrier = claim
        .attributes
        .get(CLAIM_FIELDS_ATTRIBUTE)
        .ok_or_else(|| "DocumentRead field provenance is unavailable in this cache".to_string())?;
    let fields = carrier
        .as_object()
        .ok_or_else(|| "DocumentRead field provenance is not an object".to_string())?;
    let wire = fields
        .get(field)
        .cloned()
        .ok_or_else(|| "DocumentRead did not provide this qualification".to_string())?;
    let field: DocumentReadField = serde_json::from_value(wire)
        .map_err(|error| format!("DocumentRead qualification is malformed: {error}"))?;
    match field {
        DocumentReadField::Unknown { reason } => Err(format!("source field is unknown: {reason}")),
        DocumentReadField::Supported {
            value, evidence, ..
        } => {
            if evidence.trim().is_empty() {
                return Err("supported field has no field evidence".into());
            }
            let Some(document) = source_document else {
                return Err("claim has no unique cited source document".into());
            };
            if !field_evidence_exists(document, &evidence) {
                return Err("field evidence no longer resolves in the cited document".into());
            }
            match value {
                Value::String(value) => Ok(value),
                Value::Number(value) => Ok(value.to_string()),
                _ => Err("supported qualification is not a scalar string or number".into()),
            }
        }
    }
}

pub(super) fn history_entry(
    input: usize,
    claim: &crate::enrichment::atlas::atoms::Claim,
    document: Option<&str>,
    report_time: Option<&str>,
    rule: Option<&str>,
    state: Option<&str>,
    identity: Option<&str>,
    effective_time: Option<&str>,
    disposition: &str,
    reason: Option<&str>,
    field_evidence: Value,
) -> Value {
    let mut entry = json!({
        "input": input,
        "claim": claim.id.as_str(),
        "claim_kind": claim.claim_kind,
        "assigned_to": claim.subject.as_ref().map(|subject| subject.as_str()),
        "document": document,
        "report_time": report_time,
        "effective_time": effective_time,
        "identity": identity,
        "rule": rule,
        "state": state,
        "citation": serde_json::to_value(&claim.evidence).unwrap_or(Value::Null),
        "field_evidence": field_evidence,
        "disposition": disposition,
    });
    if let Some(reason) = reason {
        entry["reason"] = Value::String(reason.to_string());
    }
    entry
}

pub(super) fn set_disposition(history: &mut [Value], index: usize, disposition: &str) {
    if let Some(entry) = history.get_mut(index).and_then(Value::as_object_mut) {
        entry.insert("disposition".into(), Value::String(disposition.to_string()));
    }
}

pub(super) fn report_time_class(time: &str) -> Option<&'static str> {
    if time.len() == 10
        && time.as_bytes().get(4) == Some(&b'-')
        && time.as_bytes().get(7) == Some(&b'-')
    {
        Some("date")
    } else if time.contains('T') && time.ends_with('Z') {
        Some("utc")
    } else {
        None
    }
}
