// SPDX-License-Identifier: AGPL-3.0-or-later
//! A record's value of a field its statements read (`AttrDecl::by`): for each
//! record of a type RESOLVE decides, the declared fold over what each
//! statement about it read of the field (`SUBJECT_FIELDS_ATTRIBUTE`), each
//! reading counted through the statement's own document. Only this writer
//! gives a record such a value; RESOLVE gives none, and a statement keeps its
//! own reading either way. A reference holds a record or nothing: a reading
//! of a `ref` counts as the one record of the referenced type its statement's
//! document names in its fields (`Participants`) whose identity values,
//! joined as Pick shows them, it folds equal to, and is dropped when that
//! record is in an exclusion set over the type (`exclusions_of`). A reading
//! that names no such record is a mention's words: it counts as the record
//! RESOLVE put the same document's mention of those words on
//! (`decides_mentions`). One that names neither is never held and never
//! minted into a record: it stays on the derivation line, cited
//! (`UnresolvedReading`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tracing::{debug, info};

use super::outcome::write_fold;
use super::{
    decide, fold_identity_value, DeriveStage, DerivedOutcome, DerivedReport, DerivedTally,
    DerivedValue, Graph, Node, Participants, SectionDocuments, UnresolvedReading,
};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_records::{decides, BuildAtoms};
use crate::enrichment::atlas::resolution_sources::key_name;
use crate::enrichment::ontology::derived::FoldBy;
use crate::enrichment::ontology::{AttrDecl, AttrFamily, OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::document_read::{
    field_readings, mention_words, DocumentReadField, SUBJECT_FIELDS_ATTRIBUTE,
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

/// One record's fold of one field, before it is written.
struct Folded {
    record: String,
    outcome: DerivedOutcome,
    value: Option<Value>,
    excluded: BTreeSet<String>,
    unjudged: BTreeSet<String>,
    unresolved: Vec<UnresolvedReading>,
    /// Each reading folded, cited (`write_fold`).
    readings: Vec<Value>,
}

/// The one record of type `of` that document `doc`'s fields name and whose
/// identity values, joined by a space as a Pick of it gives them, fold equal
/// to `value`, by its place among the entities; failing that, the one record
/// `doc`'s mention whose words fold equal to `value` is on; or why there is
/// none. Keyed by the document, never by words across documents.
fn record_read(
    graph: &Graph<'_>,
    doc: Option<&str>,
    of: &str,
    value: &Value,
) -> Result<usize, String> {
    let doc = doc.ok_or("the statement is in no one document")?;
    let want = value
        .as_str()
        .and_then(fold_identity_value)
        .ok_or("the reading is no name")?;
    let keys = graph.index.effective_identity(of);
    let named: BTreeSet<usize> = graph
        .participants
        .get(doc)
        .into_iter()
        .flatten()
        .filter_map(|(_, id)| graph.entity_at.get(id.as_str()).copied())
        .filter(|&i| {
            let e = &graph.entities[i];
            let t = e.entity_type.as_str_repr();
            (t == of || graph.index.is_a(t, of))
                && fold_identity_value(&key_name(&e.attributes, keys.iter().map(String::as_str)))
                    .is_some_and(|k| k == want)
        })
        .collect();
    match named.len() {
        1 => return Ok(*named.first().unwrap()),
        0 => {}
        n => {
            return Err(format!(
                "{n} `{of}` records the document's fields name are {want:?}"
            ))
        }
    }
    // A reading that names no listed record is a mention's words: it holds
    // the record RESOLVE put that document's mention of them on.
    let mentioned: BTreeSet<usize> = graph
        .claims
        .iter()
        .zip(&graph.claim_doc)
        .filter(|(c, d)| c.claim_kind.is_none() && **d == Some(doc))
        .filter(|(c, _)| {
            mention_words(&c.attributes)
                .and_then(fold_identity_value)
                .is_some_and(|w| w == want)
        })
        .filter_map(|(c, _)| graph.entity_at.get(c.subject.as_ref()?.as_str()).copied())
        .filter(|&i| {
            let t = graph.entities[i].entity_type.as_str_repr();
            t == of || graph.index.is_a(t, of)
        })
        .collect();
    match mentioned.len() {
        1 => Ok(*mentioned.first().unwrap()),
        0 => Err(format!(
            "no `{of}` the document's fields name or its text mentions is {want:?}"
        )),
        n => Err(format!(
            "the document's mentions of {want:?} are on {n} `{of}` records"
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
        let exclusions = of
            .map(|of| policies.derivation.derived.exclusions_of(of))
            .unwrap_or_default();
        let is_it = |t: &str| t == type_name || index.is_a(t, type_name);
        let mut decided: Vec<Folded> = Vec::new();
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
                let mut excluded: BTreeSet<String> = BTreeSet::new();
                let mut unjudged: BTreeSet<String> = BTreeSet::new();
                let mut unresolved: Vec<UnresolvedReading> = Vec::new();
                let mut readings: Vec<Value> = Vec::new();
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
                    let Some(DocumentReadField::Supported {
                        value, evidence, ..
                    }) = reading
                    else {
                        continue;
                    };
                    let doc = graph.claim_doc[j];
                    let cited = evidence.clone();
                    let (key, value) = match of.map(|of| record_read(&graph, doc, of, &value)) {
                        None => (words(&value), value),
                        Some(Ok(i)) => {
                            let id = graph.entities[i].id.as_str().to_string();
                            let node = Node::Entity(i);
                            if let Some(set) = exclusions
                                .iter()
                                .find(|s| graph.member(s, &node) == Some(true))
                            {
                                debug!(claim = %claim.id.as_str(), attribute = attr, atom = %id, set = %set.id, "atlas/derive: a reference read names a record in an exclusion set; the record does not hold it");
                                excluded.insert(id);
                                continue;
                            }
                            // D3 (operator, 2026-10-12): a record no set can
                            // judge is held, and the line says it is unjudged.
                            if let Some(set) =
                                exclusions.iter().find(|s| graph.member(s, &node).is_none())
                            {
                                debug!(claim = %claim.id.as_str(), attribute = attr, atom = %id, set = %set.id, "atlas/derive: a reference read names a record no exclusion set can judge; the record holds it, marked unjudged");
                                unjudged.insert(id.clone());
                            }
                            (id.clone(), Value::String(id))
                        }
                        Some(Err(why)) => {
                            debug!(claim = %claim.id.as_str(), attribute = attr, %why, "atlas/derive: a reference read names no record; it stays unresolved on the derivation line");
                            unresolved.push(UnresolvedReading {
                                claim: claim.id.as_str().to_string(),
                                document: doc.map(str::to_string),
                                words: value,
                                evidence,
                                why,
                            });
                            continue;
                        }
                    };
                    read.entry(key.clone())
                        .or_default()
                        .insert(doc.map(str::to_string));
                    readings.push(serde_json::json!({
                        "value": value,
                        "claim": claim.id.as_str(),
                        "document": doc,
                        "evidence": cited,
                    }));
                    values.insert(key, value);
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
                decided.push(Folded {
                    record: record.to_string(),
                    outcome,
                    value,
                    excluded,
                    unjudged,
                    unresolved,
                    readings,
                });
            }
        }
        let mut tally = DerivedTally::default();
        for Folded {
            record,
            outcome,
            value,
            excluded,
            unjudged,
            unresolved,
            readings,
        } in decided
        {
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
            write_fold(attributes, attr, &outcome, Some(readings), &unresolved);
            let replaced = previous.filter(|p| attributes.get(attr) != Some(p));
            tally.atoms += 1;
            *tally.outcomes.entry(outcome.label()).or_default() += 1;
            tally.excluded += excluded.len();
            tally.unjudged += unjudged.len();
            tally.unresolved += unresolved.len();
            debug!(r#type = type_name, attribute = attr, atom = %record, outcome = outcome.label(), by = by.label(), excluded = excluded.len(), unresolved = unresolved.len(), "atlas/derive: a record folds its statements' readings");
            sink(&DerivedValue {
                stage: DeriveStage::AfterResolve,
                type_name: type_name.to_string(),
                attribute: attr.to_string(),
                atom: record,
                derived: format!("by {}", by.label()),
                outcome,
                excluded: excluded.into_iter().collect(),
                unresolved,
                unjudged: unjudged.into_iter().collect(),
                replaced,
                protocol: None,
                by: SourcePrecision::new(
                    format!("derived:by {}", by.label()),
                    Precision::Unmeasured,
                ),
            });
        }
        info!(r#type = type_name, attribute = attr, by = by.label(), atoms = tally.atoms, outcomes = ?tally.outcomes, excluded = tally.excluded, unjudged = tally.unjudged, unresolved = tally.unresolved, "atlas/derive: statements' readings folded");
        report.insert(format!("{type_name}.{attr}"), tally);
    }
    Ok(())
}

#[cfg(test)]
#[path = "read_fields_tests.rs"]
mod tests;
