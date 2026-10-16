// SPDX-License-Identifier: AGPL-3.0-or-later
//! The candidates a document offers a reference (ONTOLOGY_METHOD §Reading):
//! of a referenced type, per document, the records its declared metadata
//! source names in this document's own fields, read by the projection's
//! readers (`resolution_sources::field_records`), so a candidate is a record
//! the build makes; less any record in a declared exclusion set, a set over
//! the type that a declared path or fold drops by (`[!set]`). The window
//! reader shows them beside each document (`Named here:`) and lands a
//! reference reading on one (`window_refs.rs`). A candidate answers with its
//! identity value, cited by the field it was read from; it is not a record by
//! itself (C1: nothing is minted here).

use serde_json::{Map, Value};
use tracing::debug;

use crate::enrichment::atlas::resolution_derived::conditions_hold;
use crate::enrichment::atlas::resolution_sources::field_records;
use crate::enrichment::atlas::resolve_records::LABELS;
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{OntologyPolicies, OntologyTypeDecl, SourceDecl, TypeIndex};

/// One thing a reference may name.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Candidate {
    /// A record the type's metadata source reads from a field of the document.
    Record {
        /// The identity value(s) as read, in key order.
        identity: Vec<String>,
        /// Its other read values (a display name), for the question.
        names: Vec<String>,
        field: String,
        /// The field's value it was read from: what cites it.
        scalar: String,
    },
}

impl Candidate {
    pub(super) fn shown(&self, of: &str) -> String {
        match self {
            Candidate::Record {
                identity,
                names,
                field,
                ..
            } => {
                let mut s = format!("{of} {}", identity.join(" "));
                if !names.is_empty() {
                    s.push_str(&format!(" ({})", names.join(", ")));
                }
                s.push_str(&format!(", named in the field `{field}`"));
                s
            }
        }
    }

    /// The value a chosen candidate gives the field, and what cites it.
    pub(super) fn answer(&self) -> (String, String) {
        match self {
            Candidate::Record {
                identity, scalar, ..
            } => (identity.join(" "), scalar.clone()),
        }
    }
}

/// The candidates a document offers one referenced type, at most one forced
/// choice's worth, and how many were left off.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Candidates {
    pub(super) shown: Vec<Candidate>,
    pub(super) dropped: usize,
}

/// What `document` offers a reference to `target`, at most one forced
/// choice's worth ([`listed`]).
pub(super) fn candidates(
    document: &SourceDocument,
    policies: &OntologyPolicies,
    target: &OntologyTypeDecl,
) -> Candidates {
    let mut all = listed(document, policies, target);
    let dropped = all.len().saturating_sub(LABELS.len());
    all.truncate(LABELS.len());
    Candidates {
        shown: all,
        dropped,
    }
}

/// Everything `document` offers a reference to `target`: the records its
/// source reads here, less the exclusion sets.
fn listed(
    document: &SourceDocument,
    policies: &OntologyPolicies,
    target: &OntologyTypeDecl,
) -> Vec<Candidate> {
    let excluded = policies.derivation.derived.exclusions_of(&target.name);
    let mut all: Vec<Candidate> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    if let Some(SourceDecl::Metadata(src)) = &target.source {
        let index = TypeIndex::from_policies(policies);
        let identity = index.effective_identity(&target.name);
        for field in &src.metadata {
            let Some(value) = document.metadata().get(field) else {
                continue;
            };
            for record in field_records(src, identity, value) {
                if keys.contains(&record.key) {
                    continue;
                }
                let attributes: Map<String, Value> = record
                    .attributes
                    .iter()
                    .map(|(a, v)| (a.clone(), Value::String(v.clone())))
                    .collect();
                if let Some(set) = excluded
                    .iter()
                    .find(|s| conditions_hold(s, &attributes) == Some(true))
                {
                    debug!(document = %document.key(), of = %target.name, key = %record.key, set = %set.id, "document_read/pick: candidate in an exclusion set");
                    continue;
                }
                keys.push(record.key.clone());
                let names = record
                    .attributes
                    .iter()
                    .filter(|(a, _)| !identity.contains(a))
                    .map(|(_, v)| v.clone())
                    .collect();
                all.push(Candidate::Record {
                    identity: record.identity,
                    names,
                    field: field.clone(),
                    scalar: record.scalar,
                });
            }
        }
    }
    all
}

#[cfg(test)]
#[path = "pick_tests.rs"]
mod tests;
