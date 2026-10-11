// SPDX-License-Identifier: AGPL-3.0-or-later
//! A record's value of a field its statements read (`AttrDecl::by`): for each
//! record of a type RESOLVE decides, the declared fold over what each
//! statement about it read of the field (`SUBJECT_FIELDS_ATTRIBUTE`), each
//! reading counted through the statement's own document. Only this writer
//! gives a record such a value; RESOLVE gives none, and a statement keeps its
//! own reading either way. A reference holds a record: a reading of a `ref`
//! counts as the one record of the referenced type its statement's document
//! names in its fields (`Participants`) whose identity values, joined as Pick
//! shows them, it folds equal to. A reading that names no such record (a
//! mention) is held as its words, a proposal never minted into a record, and
//! counted.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tracing::{debug, info};

use super::{
    decide, fold_identity_value, DeriveStage, DerivedOutcome, DerivedReport, DerivedTally,
    DerivedValue, Graph, Participants, SectionDocuments,
};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_records::{decides, BuildAtoms};
use crate::enrichment::atlas::resolution_sources::key_name;
use crate::enrichment::ontology::derived::FoldBy;
use crate::enrichment::ontology::{AttrDecl, AttrFamily, OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::document_read::{
    field_readings, DocumentReadField, SUBJECT_FIELDS_ATTRIBUTE,
};

/// Every `(type, attribute, by)` a declaration folds onto its records.
pub(super) fn declared(policies: &OntologyPolicies) -> Vec<(&str, &AttrDecl, FoldBy)> {
    let index = TypeIndex::from_policies(policies);
    policies
        .shape
        .types
        .iter()
        .filter(|t| decides(&index, &t.name))
        .flat_map(|t| {
            t.attributes
                .iter()
                .filter_map(move |a| Some((t.name.as_str(), a, a.by?)))
        })
        .collect()
}

/// A reading's fold key: its text, or its JSON.
fn words(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The one record of type `of` that document `doc`'s fields name and whose
/// identity values, joined by a space as a Pick of it gives them, fold equal
/// to `value`; or why there is none.
fn record_read(
    graph: &Graph<'_>,
    doc: Option<&str>,
    of: &str,
    value: &Value,
) -> Result<String, String> {
    let doc = doc.ok_or("the statement is in no one document")?;
    let want = value
        .as_str()
        .and_then(fold_identity_value)
        .ok_or("the reading is no name")?;
    let keys = graph.index.effective_identity(of);
    let named: BTreeSet<&str> = graph
        .participants
        .get(doc)
        .into_iter()
        .flatten()
        .filter_map(|(_, id)| {
            graph
                .entity_at
                .get(id.as_str())
                .map(|&i| &graph.entities[i])
        })
        .filter(|e| {
            let t = e.entity_type.as_str_repr();
            t == of || graph.index.is_a(t, of)
        })
        .filter(|e| {
            fold_identity_value(&key_name(&e.attributes, keys.iter().map(String::as_str)))
                .is_some_and(|k| k == want)
        })
        .map(|e| e.id.as_str())
        .collect();
    match named.len() {
        1 => Ok(named.first().unwrap().to_string()),
        0 => Err(format!("no `{of}` the document's fields name is {want:?}")),
        n => Err(format!(
            "{n} `{of}` records the document's fields name are {want:?}"
        )),
    }
}

/// Fold each declared field onto every record of its type, writing the
/// decided value and telling `sink` every record's outcome.
pub(super) fn derive(
    atoms: &mut BuildAtoms<'_>,
    documents: &SectionDocuments,
    participants: &Participants,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    report: &mut DerivedReport,
    sink: &mut (dyn FnMut(&DerivedValue) + Send),
) -> Result<(), String> {
    for (type_name, decl, by) in declared(policies) {
        let attr = decl.name.as_str();
        let of = match &decl.family {
            AttrFamily::Ref { of } => Some(of.as_str()),
            _ => None,
        };
        let is_it = |t: &str| t == type_name || index.is_a(t, type_name);
        let mut decided: Vec<(String, DerivedOutcome, Option<Value>)> = Vec::new();
        let mut unnamed = 0usize;
        {
            let graph = Graph::new(atoms, documents, participants, policies, index)?;
            let records = atoms
                .entities
                .iter()
                .filter(|e| is_it(e.entity_type.as_str_repr()))
                .map(|e| e.id.as_str())
                .chain(
                    atoms
                        .events
                        .iter()
                        .filter(|e| is_it(e.event_type.as_str_repr()))
                        .map(|e| e.id.as_str()),
                );
            for record in records {
                let mut read: BTreeMap<String, BTreeSet<Option<String>>> = BTreeMap::new();
                let mut values: BTreeMap<String, Value> = BTreeMap::new();
                for &j in graph.about.get(record).into_iter().flatten() {
                    let claim = &graph.claims[j];
                    let Some(carrier) = claim.attributes.get(SUBJECT_FIELDS_ATTRIBUTE) else {
                        continue;
                    };
                    let reading = match field_readings(carrier) {
                        Ok(mut readings) => readings.remove(attr),
                        Err(why) => {
                            debug!(claim = %claim.id.as_str(), %why, "atlas/derive: a statement's readings are unreadable; it gives the record nothing");
                            None
                        }
                    };
                    if let Some(DocumentReadField::Supported { value, .. }) = reading {
                        let named =
                            of.map(|of| record_read(&graph, graph.claim_doc[j], of, &value));
                        let (key, value) = match named {
                            Some(Ok(id)) => (id.clone(), Value::String(id)),
                            Some(Err(why)) => {
                                debug!(claim = %claim.id.as_str(), attribute = attr, %why, "atlas/derive: a reference read names no record; the record holds its words");
                                unnamed += 1;
                                (words(&value), value)
                            }
                            None => (words(&value), value),
                        };
                        let doc = graph.claim_doc[j].map(str::to_string);
                        read.entry(key.clone()).or_default().insert(doc);
                        values.insert(key, value);
                    }
                }
                let outcome = decide(by, &[read], &graph.clock);
                let value = match &outcome {
                    DerivedOutcome::Decided { values: keys, .. } => match keys.as_slice() {
                        [one] => values.get(one).cloned(),
                        many => Some(Value::Array(
                            many.iter().filter_map(|k| values.get(k).cloned()).collect(),
                        )),
                    },
                    _ => None,
                };
                decided.push((record.to_string(), outcome, value));
            }
        }
        let mut tally = DerivedTally::default();
        for (record, outcome, value) in decided {
            let attributes = match atoms.entities.iter_mut().find(|e| e.id.as_str() == record) {
                Some(e) => &mut e.attributes,
                None => match atoms.events.iter_mut().find(|e| e.id.as_str() == record) {
                    Some(e) => &mut e.attributes,
                    None => continue,
                },
            };
            let previous = attributes.remove(attr);
            if let Some(value) = value {
                attributes.insert(attr.to_string(), value);
            }
            let replaced = previous.filter(|p| attributes.get(attr) != Some(p));
            tally.atoms += 1;
            *tally.outcomes.entry(outcome.label()).or_default() += 1;
            debug!(r#type = type_name, attribute = attr, atom = %record, outcome = outcome.label(), by = by.label(), "atlas/derive: a record folds its statements' readings");
            sink(&DerivedValue {
                stage: DeriveStage::AfterResolve,
                type_name: type_name.to_string(),
                attribute: attr.to_string(),
                atom: record,
                derived: format!("by {}", by.label()),
                outcome,
                excluded: Vec::new(),
                replaced,
                protocol: None,
                by: SourcePrecision::new(
                    format!("derived:by {}", by.label()),
                    Precision::Unmeasured,
                ),
            });
        }
        info!(r#type = type_name, attribute = attr, by = by.label(), atoms = tally.atoms, outcomes = ?tally.outcomes, references_naming_no_record = unnamed, "atlas/derive: statements' readings folded");
        report.insert(format!("{type_name}.{attr}"), tally);
    }
    Ok(())
}

#[cfg(test)]
#[path = "read_fields_tests.rs"]
mod tests;
