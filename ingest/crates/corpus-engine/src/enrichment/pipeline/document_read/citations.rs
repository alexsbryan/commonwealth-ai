use std::collections::{BTreeMap, BTreeSet};

use kernel_types::ContentHash;
use serde_json::{json, Value};

use crate::enrichment::atlas::SourceDocument;

use super::{DocumentReadClaim, DocumentReadField};

const PREFIX: &str = "q:";

pub(super) fn choices(document: &SourceDocument) -> BTreeMap<String, String> {
    let body = document.raw_body();
    let mut quotes: BTreeSet<String> = body
        .lines()
        .chain(std::iter::once(body.as_str()))
        .map(str::trim)
        .filter(|quote| !quote.is_empty())
        .map(str::to_owned)
        .collect();
    for value in document.metadata().values() {
        quotes.extend(super::validation::metadata_strings(value));
    }
    quotes.extend(
        [
            Some(document.key()),
            document.source_doc_id(),
            document.title(),
            document.url(),
        ]
        .into_iter()
        .flatten()
        .filter(|quote| !quote.is_empty())
        .map(str::to_owned),
    );
    let mut choices = BTreeMap::new();
    for quote in quotes {
        let content = serde_json::to_string(&(document.key(), &quote))
            .expect("source citation strings are serializable");
        let hash = ContentHash::of_str(&content);
        // A short handle is a request-local address, never persisted identity.
        // A collision cannot bind another quote: use the full digest instead.
        let mut handle = format!("{PREFIX}{}", hash.short());
        if choices
            .get(&handle)
            .is_some_and(|existing| existing != &quote)
        {
            tracing::warn!(document = document.key(), %handle, "phase1.source_citation_short_collision");
            handle = format!("{PREFIX}{}", hash.to_hex());
        }
        choices.insert(handle, quote);
    }
    choices
}

pub(super) fn constrain(outcome: &mut Value, document: &SourceDocument) {
    let choices = choices(document);
    let claim_handles: Vec<&str> = choices
        .iter()
        .filter(|(_, quote)| super::validation::claim_evidence_exists(document, quote))
        .map(|(handle, _)| handle.as_str())
        .collect();
    let field_handles: Vec<&str> = choices
        .iter()
        .filter(|(_, quote)| super::validation::field_evidence_exists(document, quote))
        .map(|(handle, _)| handle.as_str())
        .collect();
    if let Some(branches) = outcome
        .get_mut("properties")
        .and_then(|value| value.get_mut("claims"))
        .and_then(|value| value.get_mut("items"))
        .and_then(|value| value.get_mut("oneOf"))
        .and_then(Value::as_array_mut)
    {
        for claim in branches {
            claim["properties"]["evidence"] = json!({"type":"string", "enum":claim_handles});
            for bag in ["fields", "subject_fields"] {
                if let Some(fields) = claim["properties"][bag]["properties"].as_object_mut() {
                    for field in fields.values_mut() {
                        field["oneOf"][0]["properties"]["evidence"] =
                            json!({"type":"string", "enum":field_handles});
                    }
                }
            }
        }
    }
}

pub(super) fn expand(
    claim: &mut DocumentReadClaim,
    document: &SourceDocument,
) -> Result<bool, String> {
    let choices = choices(document);
    let mut changed = false;
    let mut bind = |evidence: &mut String| -> Result<(), String> {
        if !evidence.starts_with(PREFIX) {
            return Ok(());
        }
        let quote = choices.get(evidence).ok_or_else(|| {
            format!(
                "citation handle {evidence:?} is not supplied for document `{}`",
                document.key()
            )
        })?;
        tracing::debug!(document = document.key(), handle = %evidence, "phase1.source_citation_bound");
        *evidence = quote.clone();
        changed = true;
        Ok(())
    };
    bind(&mut claim.evidence)?;
    for fields in [&mut claim.fields, &mut claim.subject_fields] {
        for field in fields.values_mut() {
            if let DocumentReadField::Supported { evidence, .. } = field {
                bind(evidence)?;
            }
        }
    }
    Ok(changed)
}
