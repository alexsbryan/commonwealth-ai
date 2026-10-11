// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prefill (ONTOLOGY_METHOD §Reading): a document's declared facts, written
//! once and put first in every question about it. The facts are the metadata
//! fields the declaration names (each metadata source's fields, then
//! `change.document`'s), and the roles derived from them through declared
//! sets: a set over documents this document is in, and, for a set over a
//! sourced type, each record a field names that is in it (the sender's side).
//! A pure function of the declaration and the document's own fields: it holds
//! no other document's text (C2) and says only what is there.

use serde_json::{Map, Value};

use crate::enrichment::atlas::resolution_derived::conditions_hold;
use crate::enrichment::atlas::resolution_sources::field_records;
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::derived::DOCUMENT;
use crate::enrichment::ontology::{OntologyPolicies, SourceDecl, TypeIndex};

/// What opens a document's facts; part of the read contract's fingerprint.
pub(super) const FACTS_HEADER: &str = "Declared facts of this document:\n";

/// The metadata fields the declaration names, in declaration order, each once.
pub(super) fn declared_fields(policies: &OntologyPolicies) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let sourced = policies.shape.types.iter().filter_map(|t| match &t.source {
        Some(SourceDecl::Metadata(s)) => Some(s.metadata.iter().map(String::as_str)),
        _ => None,
    });
    let document = policies.change.document.iter().flat_map(|d| {
        d.declared()
            .map(|(_, f)| f)
            .chain(d.author.as_deref())
            .collect::<Vec<_>>()
    });
    for f in sourced.flatten().chain(document) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// The declared facts of `document`, as the lines that open every question
/// about it; empty when the declaration names nothing the document holds.
pub(super) fn facts(document: &SourceDocument, policies: &OntologyPolicies) -> String {
    let fields = document.metadata();
    let mut lines: Vec<String> = declared_fields(policies)
        .into_iter()
        .filter_map(|f| {
            let value = super::validation::metadata_strings(fields.get(f)?).join(", ");
            (!value.trim().is_empty()).then(|| format!("{f}: {}", value.trim()))
        })
        .collect();
    lines.extend(roles(document, policies));
    if lines.is_empty() {
        return String::new();
    }
    tracing::trace!(document = %document.key(), facts = lines.len(), "document_read/prefill: facts");
    format!("{FACTS_HEADER}{}\n\n", lines.join("\n"))
}

/// The declared sets this document, or a record one of its fields names, is in.
fn roles(document: &SourceDocument, policies: &OntologyPolicies) -> Vec<String> {
    let index = TypeIndex::from_policies(policies);
    let fields = document.metadata();
    let mut out = Vec::new();
    for set in &policies.derivation.derived.sets {
        if set.of == DOCUMENT {
            if conditions_hold(set, fields) == Some(true) {
                out.push(format!("this document is in `{}`", set.id));
            }
            continue;
        }
        let Some(t) = policies.type_decl(&set.of) else {
            continue;
        };
        let Some(SourceDecl::Metadata(src)) = &t.source else {
            continue;
        };
        for field in &src.metadata {
            let Some(value) = fields.get(field) else {
                continue;
            };
            for record in field_records(src, index.effective_identity(&t.name), value) {
                let attributes: Map<String, Value> = record
                    .attributes
                    .iter()
                    .map(|(a, v)| (a.clone(), Value::String(v.clone())))
                    .collect();
                if conditions_hold(set, &attributes) == Some(true) {
                    let line = format!(
                        "{field} names {} {}, which is in `{}`",
                        t.name,
                        record.identity.join(" "),
                        set.id
                    );
                    if !out.contains(&line) {
                        out.push(line);
                    }
                }
            }
        }
    }
    out
}

/// `prompt` with `facts` put first in its user turn.
pub(super) fn carrying(
    mut prompt: crate::enrichment::pipeline::types::ChatPrompt,
    facts: &str,
) -> crate::enrichment::pipeline::types::ChatPrompt {
    if !facts.is_empty() {
        prompt.user = format!("{facts}{}", prompt.user);
    }
    prompt
}

#[cfg(test)]
#[path = "prefill_tests.rs"]
mod tests;
