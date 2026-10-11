// SPDX-License-Identifier: AGPL-3.0-or-later
//! A record's value of a field its statements read (`AttrDecl::by`): for each
//! record of a type RESOLVE decides, the declared fold over what each
//! statement about it read of the field (`SUBJECT_FIELDS_ATTRIBUTE`), each
//! reading counted through the statement's own document. Only this writer
//! gives a record such a value; RESOLVE gives none, and a statement keeps its
//! own reading either way.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tracing::{debug, info};

use super::{
    decide, DeriveStage, DerivedOutcome, DerivedReport, DerivedTally, DerivedValue, Graph,
    Participants, SectionDocuments,
};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_records::{decides, BuildAtoms};
use crate::enrichment::ontology::derived::FoldBy;
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::document_read::{
    field_readings, DocumentReadField, SUBJECT_FIELDS_ATTRIBUTE,
};

/// Every `(type, attribute, by)` a declaration folds onto its records.
pub(super) fn declared(policies: &OntologyPolicies) -> Vec<(&str, &str, FoldBy)> {
    let index = TypeIndex::from_policies(policies);
    policies
        .shape
        .types
        .iter()
        .filter(|t| decides(&index, &t.name))
        .flat_map(|t| {
            t.attributes
                .iter()
                .filter_map(move |a| Some((t.name.as_str(), a.name.as_str(), a.by?)))
        })
        .collect()
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
    for (type_name, attr, by) in declared(policies) {
        let is_it = |t: &str| t == type_name || index.is_a(t, type_name);
        let mut decided: Vec<(String, DerivedOutcome, Option<Value>)> = Vec::new();
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
                        let key = match &value {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
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
        info!(r#type = type_name, attribute = attr, by = by.label(), atoms = tally.atoms, outcomes = ?tally.outcomes, "atlas/derive: statements' readings folded");
        report.insert(format!("{type_name}.{attr}"), tally);
    }
    Ok(())
}

#[cfg(test)]
#[path = "read_fields_tests.rs"]
mod tests;
