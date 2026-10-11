// SPDX-License-Identifier: AGPL-3.0-or-later
//! A record holds its documents (ONTOLOGY_METHOD §The core): those its
//! statements are in, and each document read in the run that holds no
//! statement of the record's type, when every statement of that type in the
//! document's declared thread sits on that one record. A thread whose
//! statements sit on several records, or on none, places nothing; nor is a
//! document placed whose own statements RESOLVE put on no record, or one the
//! reader could not judge. Every document read is counted by how it came to
//! be held or not. Nothing here asks a model: the records are RESOLVE's, the
//! thread is the declaration's. A held document is written by its declared
//! id, the form a claim's `document_id` stamp carries, so a declaration with
//! no id places nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Map, Value};
use tracing::{debug, info};

use super::{DeriveStage, DerivedOutcome, DerivedReport, DerivedTally, DerivedValue};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_documents::{locate, read_stamp, SectionDocuments};
use crate::enrichment::atlas::resolution_records::{decides, BuildAtoms};
use crate::enrichment::ontology::{
    DocumentStamp, OntologyPolicies, TypeIndex, TypeKind, DOCUMENT_THREAD_ATTR,
    THREAD_DOCUMENTS_ATTR,
};
use crate::enrichment::pipeline::document_read::DocumentReadStatus;

/// How a document read came to be held by a record of a type, or not. Closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// Its own statements of the type are on a record.
    ByStatement,
    /// It holds none, and every one in its thread is on one record.
    ByThread,
    /// Its own statements of the type are on no record.
    StatementUnplaced,
    /// The reader could not judge it.
    NotJudged,
    /// The reader was given it and did not read it.
    NotRead,
    NoThread,
    /// No statement of the type in its thread is on a record.
    ThreadOnNoRecord,
    ThreadOnSeveralRecords,
    NoId,
}

impl Placement {
    fn label(self) -> &'static str {
        match self {
            Placement::ByStatement => "by_statement",
            Placement::ByThread => "by_thread",
            Placement::StatementUnplaced => "statement_unplaced",
            Placement::NotJudged => "not_judged",
            Placement::NotRead => "not_read",
            Placement::NoThread => "no_thread",
            Placement::ThreadOnNoRecord => "thread_on_no_record",
            Placement::ThreadOnSeveralRecords => "thread_on_several_records",
            Placement::NoId => "no_id",
        }
    }
}

/// The declared thread and id fields, when both are declared.
fn declared(policies: &OntologyPolicies) -> Option<(&str, &str)> {
    let d = policies.change.document.as_ref()?;
    Some((d.thread.as_deref()?, d.id.as_deref()?))
}

/// Whether a declaration places documents on records by their thread: it
/// declares a thread and an id, and RESOLVE decides some type.
pub fn applies(policies: &OntologyPolicies) -> bool {
    let index = TypeIndex::from_policies(policies);
    declared(policies).is_some()
        && policies
            .shape
            .types
            .iter()
            .any(|t| decides(&index, &t.name))
}

/// For every type RESOLVE decides, place each document `read` names (with
/// what the reader made of it) that holds no statement of the type on the one
/// record its thread's statements sit on,
/// writing the ids of a record's thread documents as its
/// `THREAD_DOCUMENTS_ATTR`, and tell `sink` every record's outcome. Run after
/// RESOLVE, over the records it made.
pub fn place_thread_documents(
    atoms: &mut BuildAtoms<'_>,
    documents: &SectionDocuments,
    read: &BTreeMap<String, DocumentReadStatus>,
    policies: &OntologyPolicies,
    sink: &mut (dyn FnMut(&DerivedValue) + Send),
) -> DerivedReport {
    let mut report = DerivedReport::new();
    let Some((thread_field, id_field)) = declared(policies) else {
        return report;
    };
    let index = TypeIndex::from_policies(policies);
    let docs = documents.by_key();
    let stamp = |key: &str, stamp: DocumentStamp, field: &str| match docs.get(key) {
        Some(d) => read_stamp(&d.fields, stamp, field),
        None => Err("the document is in no section".to_string()),
    };
    let claim_doc: Vec<Option<&str>> = atoms
        .claims
        .iter()
        .map(|c| match locate(c, documents) {
            Ok(d) => Some(d.key.as_str()),
            Err(why) => {
                debug!(claim = %c.id.as_str(), %why, "atlas/derive: a statement in no one document places no document by its thread");
                None
            }
        })
        .collect();
    for t in policies
        .shape
        .types
        .iter()
        .filter(|t| decides(&index, &t.name))
    {
        let kinds: BTreeSet<&str> = policies
            .shape
            .types
            .iter()
            .filter(|c| c.kind == TypeKind::Claim && c.subject.as_deref() == Some(t.name.as_str()))
            .map(|c| c.name.as_str())
            .collect();
        let is_it = |ty: &str| ty == t.name;
        let records: BTreeSet<String> = atoms
            .entities
            .iter()
            .filter(|e| is_it(e.entity_type.as_str_repr()))
            .map(|e| e.id.as_str().to_string())
            .chain(
                atoms
                    .events
                    .iter()
                    .filter(|e| is_it(e.event_type.as_str_repr()))
                    .map(|e| e.id.as_str().to_string()),
            )
            .collect();
        // Each document's statements of the type, and the records they sit on.
        let mut held: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (c, doc) in atoms.claims.iter().zip(&claim_doc) {
            let (Some(kind), Some(doc)) = (c.claim_kind.as_deref(), *doc) else {
                continue;
            };
            if !kinds.contains(kind) {
                continue;
            }
            let on = held.entry(doc).or_default();
            if let Some(record) = c.subject.as_ref().filter(|s| records.contains(s.as_str())) {
                on.insert(record.as_str());
            }
        }
        // Each thread's records, and the documents whose statements put them there.
        let mut threads: HashMap<String, (BTreeSet<&str>, BTreeSet<&str>)> = HashMap::new();
        for (doc, on) in &held {
            match stamp(doc, DocumentStamp::Thread, thread_field) {
                Ok(thread) => {
                    let (records, through) = threads.entry(thread).or_default();
                    records.extend(on.iter().copied());
                    if !on.is_empty() {
                        through.insert(*doc);
                    }
                }
                Err(why) => {
                    debug!(r#type = %t.name, document = %doc, %why, "atlas/derive: a document's statements name no thread")
                }
            }
        }
        let mut joined: BTreeMap<&str, (BTreeSet<String>, BTreeSet<&str>)> = BTreeMap::new();
        let mut tally = DerivedTally::default();
        for (key, reading) in read {
            let (placement, thread, record) = match held.get(key.as_str()) {
                Some(on) if !on.is_empty() => (Placement::ByStatement, None, None),
                Some(_) => (Placement::StatementUnplaced, None, None),
                None if *reading == DocumentReadStatus::CouldNotJudge => {
                    (Placement::NotJudged, None, None)
                }
                None if *reading == DocumentReadStatus::NotRead => (Placement::NotRead, None, None),
                None => match stamp(key, DocumentStamp::Thread, thread_field) {
                    Err(why) => {
                        debug!(r#type = %t.name, document = %key, %why, "atlas/derive: a document read names no thread");
                        (Placement::NoThread, None, None)
                    }
                    Ok(thread) => match threads.get(&thread) {
                        Some((records, through)) if records.len() == 1 => {
                            let record = records.first().copied();
                            match stamp(key, DocumentStamp::Id, id_field) {
                                Ok(id) => {
                                    let (ids, via) = joined.entry(record.unwrap()).or_default();
                                    ids.insert(id);
                                    via.extend(through.iter().copied());
                                    (Placement::ByThread, Some(thread), record)
                                }
                                Err(why) => {
                                    debug!(r#type = %t.name, document = %key, %why, "atlas/derive: a document its thread places has no id to be held by");
                                    (Placement::NoId, Some(thread), record)
                                }
                            }
                        }
                        Some((records, _)) if records.len() > 1 => {
                            (Placement::ThreadOnSeveralRecords, Some(thread), None)
                        }
                        _ => (Placement::ThreadOnNoRecord, Some(thread), None),
                    },
                },
            };
            debug!(r#type = %t.name, document = %key, placement = placement.label(), thread = thread.as_deref().unwrap_or("-"), record = record.unwrap_or("-"), "atlas/derive: a document read and the record that holds it");
            *tally.documents.entry(placement.label()).or_default() += 1;
        }
        let outcomes: HashMap<String, DerivedOutcome> = records
            .iter()
            .map(|r| {
                let outcome = match joined.get(r.as_str()) {
                    Some((ids, via)) => DerivedOutcome::Decided {
                        values: ids.iter().cloned().collect(),
                        input: None,
                        documents: via.iter().map(|d| d.to_string()).collect(),
                        superseded: Vec::new(),
                    },
                    None => DerivedOutcome::Nothing,
                };
                (r.clone(), outcome)
            })
            .collect();
        let atoms_of_type = atoms
            .entities
            .iter_mut()
            .filter(|e| is_it(e.entity_type.as_str_repr()))
            .map(|e| (e.id.as_str().to_string(), &mut e.attributes))
            .chain(
                atoms
                    .events
                    .iter_mut()
                    .filter(|e| is_it(e.event_type.as_str_repr()))
                    .map(|e| (e.id.as_str().to_string(), &mut e.attributes)),
            );
        for (record, attributes) in atoms_of_type {
            let Some(outcome) = outcomes.get(&record) else {
                continue;
            };
            let replaced = hold(attributes, outcome);
            tally.atoms += 1;
            *tally.outcomes.entry(outcome.label()).or_default() += 1;
            sink(&DerivedValue {
                stage: DeriveStage::AfterResolve,
                type_name: t.name.clone(),
                attribute: THREAD_DOCUMENTS_ATTR.to_string(),
                atom: record,
                derived: DOCUMENT_THREAD_ATTR.to_string(),
                outcome: outcome.clone(),
                excluded: Vec::new(),
                replaced,
                protocol: None,
                by: SourcePrecision::new(
                    format!("derived:{DOCUMENT_THREAD_ATTR}"),
                    Precision::Unmeasured,
                ),
            });
        }
        info!(r#type = %t.name, records = tally.atoms, outcomes = ?tally.outcomes, documents = ?tally.documents, "atlas/derive: documents read placed on records by their thread");
        report.insert(format!("{}.{THREAD_DOCUMENTS_ATTR}", t.name), tally);
    }
    report
}

/// Write a record's thread documents, or clear a stale list; the list it held
/// before, when that changed.
fn hold(attributes: &mut Map<String, Value>, outcome: &DerivedOutcome) -> Option<Value> {
    let previous = attributes.remove(THREAD_DOCUMENTS_ATTR);
    if let DerivedOutcome::Decided { values, .. } = outcome {
        let ids = values.iter().cloned().map(Value::String).collect();
        attributes.insert(THREAD_DOCUMENTS_ATTR.to_string(), Value::Array(ids));
    }
    previous.filter(|p| attributes.get(THREAD_DOCUMENTS_ATTR) != Some(p))
}

#[cfg(test)]
#[path = "membership_tests.rs"]
mod tests;
