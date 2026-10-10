// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pick (ONTOLOGY_METHOD §Reading): a read reference field is one forced
//! choice among the candidates code proposes, and "none". The candidates of a
//! referenced type, per document:
//!
//! - the records its declared metadata source names in this document's own
//!   fields, read by the projection's readers (`resolution_sources::
//!   field_records`), so a candidate is a record the build makes;
//! - this document's mentions of the type (`mention.rs`), less a mention
//!   whose words are a listed record's identity value or one of its read
//!   values: that mention names the record and is not offered twice;
//! - less any record in a declared exclusion set, a set over the type that a
//!   declared path or fold drops by (`[!set]`).
//!
//! A chosen record answers with its identity value, cited by the field it was
//! read from; a chosen mention with its words, cited where the line holds
//! them. Neither is a record by itself (C1: nothing is minted here).

use std::collections::BTreeMap;

use oicp_types::forced_choice;
use serde_json::{json, Map, Value};
use tracing::debug;

use super::ask::Ask;
use super::mention::Mention;
use super::passes::{argmax, render};
use super::DocumentReadField;
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_derived::conditions_hold;
use crate::enrichment::atlas::resolution_sources::field_records;
use crate::enrichment::atlas::resolve_records::{decision_call, marked_context, LABELS, NONE};
use crate::enrichment::atlas::SourceDocument;
use crate::enrichment::ontology::{
    AttrDecl, OntologyPolicies, OntologyTypeDecl, SourceDecl, TypeIndex,
};
use crate::enrichment::pipeline::types::ChatPrompt;
use crate::enrichment::reconciliation::identity_signals::fold_identity_value;

const PICK_SYSTEM: &str = include_str!("passes_pick_prompt.md");
pub(super) const PICK_PHASE: &str = "document_passes_pick";

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
    /// Words of a line that name one of the type.
    Mention { line: usize, text: String },
}

impl Candidate {
    fn shown(&self, of: &str) -> String {
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
            Candidate::Mention { line, text } => format!("{of} \"{text}\", named on line {line}"),
        }
    }

    /// The value a chosen candidate gives the field, and what cites it.
    fn answer(&self) -> (String, String) {
        match self {
            Candidate::Record {
                identity, scalar, ..
            } => (identity.join(" "), scalar.clone()),
            Candidate::Mention { text, .. } => (text.clone(), text.clone()),
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

/// What `document` offers a reference to `target`: the records its source
/// reads here, less the exclusion sets, then the mentions that name no listed
/// record.
pub(super) fn candidates(
    document: &SourceDocument,
    policies: &OntologyPolicies,
    target: &OntologyTypeDecl,
    mentions: &[Mention],
) -> Candidates {
    let derived = &policies.derivation.derived;
    let excluded: Vec<_> = derived
        .exclusion_sets()
        .into_iter()
        .filter_map(|id| derived.sets.iter().find(|s| s.id == id))
        .filter(|s| s.of == target.name)
        .collect();
    let mut all: Vec<Candidate> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    // Folded identity values and read values: what a mention naming a listed
    // record would be.
    let mut named: Vec<String> = Vec::new();
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
                named.extend(
                    record
                        .attributes
                        .values()
                        .filter_map(|v| fold_identity_value(v)),
                );
                named.extend(record.key.split('\u{1f}').map(str::to_string));
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
    for m in mentions.iter().filter(|m| m.of == target.name) {
        let folded = fold_identity_value(&m.text);
        if folded.as_ref().is_some_and(|f| named.contains(f)) {
            debug!(document = %document.key(), of = %target.name, words = %m.text, "document_read/pick: the mention names a listed record");
            continue;
        }
        if let Some(f) = folded {
            named.push(f);
        }
        all.push(Candidate::Mention {
            line: m.line,
            text: m.text.clone(),
        });
    }
    let dropped = all.len().saturating_sub(LABELS.len());
    all.truncate(LABELS.len());
    Candidates {
        shown: all,
        dropped,
    }
}

/// The Pick question: the field, the statement marked in its passage, and
/// the candidates.
fn pick_question(
    facts: &str,
    owner: &OntologyTypeDecl,
    attr: &AttrDecl,
    of: &str,
    passage: &str,
    shown: &[Candidate],
    labels: &[&str],
) -> ChatPrompt {
    let described = |name: &str, d: &str| {
        if d.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({d})")
        }
    };
    let mut u = format!(
        "{facts}Type: {}\nAttribute: {}, one {of}\n\nStatement, its words in [[ ]]: \"…{passage}…\"\n\n\
         Which {of} does the statement's {} refer to?\n",
        described(&owner.name, &owner.description),
        described(&attr.name, &attr.description),
        attr.name
    );
    for (c, label) in shown.iter().zip(labels) {
        u.push_str(&format!("{label} {}\n", c.shown(of)));
    }
    u.push_str(&format!(
        "{NONE} none of them, or the statement does not say\nAnswer with its letter."
    ));
    ChatPrompt::new(PICK_SYSTEM, u)
        .with_response_schema("read", forced_choice::schema(labels))
        .with_phase_id(PICK_PHASE)
        .with_temperature(0.0)
}

/// Pick `attr`, a reference to `of`, for the statement `ask` holds.
pub(super) async fn pick(
    ask: &Ask<'_>,
    owner: &OntologyTypeDecl,
    attr: &AttrDecl,
    of: &str,
    calls: &mut u32,
) -> DocumentReadField {
    let empty = Candidates::default();
    let offered = ask.candidates.get(of).unwrap_or(&empty);
    if offered.shown.is_empty() {
        debug!(document = ask.document, statement = ask.statement, field = %attr.name, of, "document_read/pick: no candidate");
        return DocumentReadField::Unknown {
            reason: format!("not asked: the document offers no `{of}` candidate"),
        };
    }
    let labels: Vec<&str> = LABELS[..offered.shown.len()]
        .iter()
        .copied()
        .chain([NONE])
        .collect();
    let passage = marked_context(ask.body, ask.at.start, ask.at.end);
    let prompt = pick_question(
        ask.facts,
        owner,
        attr,
        of,
        &passage,
        &offered.shown,
        &labels,
    );
    *calls += 1;
    match decision_call(ask.infer, &prompt, &labels, ask.document, ask.statement).await {
        Err(refusal) => DocumentReadField::Unknown {
            reason: format!("refused: {refusal:?}"),
        },
        Ok(dist) => match argmax(&labels, &dist) {
            (best, p) if best < offered.shown.len() => {
                let (value, evidence) = offered.shown[best].answer();
                debug!(document = ask.document, statement = ask.statement, field = %attr.name, %value, p, dist = %render(&dist), dropped = offered.dropped, "document_read/pick: picked");
                DocumentReadField::Supported {
                    value: Value::String(value),
                    evidence,
                    by: Some(SourcePrecision::new("reader_pick", Precision::Unmeasured)),
                }
            }
            (_, p) => {
                debug!(document = ask.document, statement = ask.statement, field = %attr.name, p, dist = %render(&dist), "document_read/pick: none of the candidates");
                DocumentReadField::Unknown {
                    reason: format!(
                        "the reader picked none of the candidates: {}",
                        render(&dist)
                    ),
                }
            }
        },
    }
}

/// The Pick question on a fixed sample, for the read contract's fingerprint.
pub(super) fn contract_sample() -> Value {
    let probe = OntologyTypeDecl {
        name: "type".into(),
        ..Default::default()
    };
    let attr = AttrDecl {
        name: "attribute".into(),
        family: crate::enrichment::ontology::AttrFamily::Ref { of: "other".into() },
        description: String::new(),
        derived: None,
    };
    let shown = [
        Candidate::Record {
            identity: vec!["key".into()],
            names: vec!["name".into()],
            field: "field".into(),
            scalar: "key".into(),
        },
        Candidate::Mention {
            line: 1,
            text: "words".into(),
        },
    ];
    let q = pick_question(
        "",
        &probe,
        &attr,
        "other",
        "[[words]]",
        &shown,
        &["A", "B", NONE],
    );
    json!([q.system, q.user])
}

/// Per referenced type the plan asks of a document, its candidates.
pub(super) fn per_type(
    document: &SourceDocument,
    policies: &OntologyPolicies,
    targets: &[&OntologyTypeDecl],
    mentions: &[Mention],
) -> BTreeMap<String, Candidates> {
    targets
        .iter()
        .map(|t| {
            let c = candidates(document, policies, t, mentions);
            debug!(document = %document.key(), of = %t.name, candidates = c.shown.len(), dropped = c.dropped, "document_read/pick: candidates");
            (t.name.clone(), c)
        })
        .collect()
}

#[cfg(test)]
#[path = "pick_tests.rs"]
mod tests;
