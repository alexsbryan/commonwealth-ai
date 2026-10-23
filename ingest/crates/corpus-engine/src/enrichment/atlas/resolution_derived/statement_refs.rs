// SPDX-License-Identifier: AGPL-3.0-or-later
//! A read statement's own reference fields (a claim kind's `ref` attribute, a
//! relation's `to`: `document_read::statement_attributes`): after RESOLVE each
//! holds the record its reading names, by the rule a record's folded
//! reference follows (`read_fields::held`), or nothing. The reading stays on
//! the statement, cited; one that names no record is kept on the derivation
//! line, never served as words in a reference's place. Failing input, named:
//! `Tabb trusted Maren` served `to = "Maren"`, which the record view showed as
//! an absent record.

use serde_json::Value;
use tracing::{debug, info};

use super::read_fields::{held, Held};
use super::{
    DeriveStage, DerivedOutcome, DerivedReport, DerivedTally, DerivedValue, Graph, Participants,
    SectionDocuments, UnresolvedReading,
};
use crate::enrichment::atlas::precision::{Precision, SourcePrecision};
use crate::enrichment::atlas::resolution_records::BuildAtoms;
use crate::enrichment::ontology::{AttrFamily, OntologyPolicies, TypeIndex};
use crate::enrichment::pipeline::document_read::{
    field_readings, other_names, statement_attributes, DocumentReadField, CLAIM_FIELDS_ATTRIBUTE,
};

/// Every `(statement kind, field, referenced type)` the document reader reads.
pub(super) fn declared(policies: &OntologyPolicies) -> Vec<(String, String, String)> {
    let index = TypeIndex::from_policies(policies);
    let types = &policies.shape.types;
    types
        .iter()
        .filter(|t| t.is_read_statement(types))
        .flat_map(|t| {
            statement_attributes(&index, t)
                .into_iter()
                .filter_map(|a| match &a.family {
                    AttrFamily::Ref { of } => Some((t.name.clone(), a.name.clone(), of.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// One statement's reference, decided before it is written.
struct Decided {
    claim: usize,
    outcome: DerivedOutcome,
    record: Option<String>,
    excluded: Vec<String>,
    unjudged: Vec<String>,
    unresolved: Vec<UnresolvedReading>,
}

/// Write each read statement's reference fields as the records their readings
/// name, telling `sink` every statement's outcome.
pub(super) fn resolve(
    atoms: &mut BuildAtoms<'_>,
    documents: &SectionDocuments,
    participants: &Participants,
    policies: &OntologyPolicies,
    index: &TypeIndex<'_>,
    report: &mut DerivedReport,
    sink: &mut (dyn FnMut(&DerivedValue) + Send),
) -> Result<(), String> {
    for (kind, attr, of) in declared(policies) {
        let exclusions = policies.derivation.derived.exclusions_of(&of);
        let mut decided = Vec::new();
        {
            let graph = Graph::new(atoms, documents, participants, policies, index)?;
            for (j, claim) in graph.claims.iter().enumerate() {
                if claim.claim_kind.as_deref() != Some(kind.as_str()) {
                    continue;
                }
                // Only what the document reader read: its readings ride here.
                let Some(carrier) = claim.attributes.get(CLAIM_FIELDS_ATTRIBUTE) else {
                    continue;
                };
                let reading = match field_readings(carrier) {
                    Ok(mut readings) => readings.remove(&attr),
                    Err(why) => {
                        debug!(claim = %claim.id.as_str(), %why, "atlas/derive: a statement's own readings are unreadable; its reference holds nothing");
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
                let mut d = Decided {
                    claim: j,
                    outcome: DerivedOutcome::Nothing,
                    record: None,
                    excluded: Vec::new(),
                    unjudged: Vec::new(),
                    unresolved: Vec::new(),
                };
                let also = value
                    .as_str()
                    .map(|w| other_names(&claim.attributes, w))
                    .unwrap_or_default();
                match held(&graph, &exclusions, doc, &of, &value, &also) {
                    Held::Record { id, unjudged } => {
                        debug!(claim = %claim.id.as_str(), attribute = %attr, atom = %id, unjudged = ?unjudged, "atlas/derive: a statement's reference holds the record its reading names");
                        d.unjudged.extend(unjudged.map(|_| id.clone()));
                        d.outcome = DerivedOutcome::Decided {
                            values: vec![id.clone()],
                            input: None,
                            documents: doc.map(str::to_string).into_iter().collect(),
                            superseded: Vec::new(),
                        };
                        d.record = Some(id);
                    }
                    Held::Excluded { id, set } => {
                        debug!(claim = %claim.id.as_str(), attribute = %attr, atom = %id, %set, "atlas/derive: a statement's reference names a record in an exclusion set; it holds nothing");
                        d.excluded.push(id);
                    }
                    Held::Unresolved(why) => {
                        debug!(claim = %claim.id.as_str(), attribute = %attr, %why, "atlas/derive: a statement's reference names no record; it holds nothing and stays on the derivation line");
                        d.unresolved.push(UnresolvedReading {
                            claim: claim.id.as_str().to_string(),
                            document: doc.map(str::to_string),
                            words: value,
                            evidence,
                            why,
                        });
                    }
                }
                decided.push(d);
            }
        }
        let mut tally = DerivedTally::default();
        for d in decided {
            let claim = &mut atoms.claims[d.claim];
            let previous = claim.attributes.remove(&attr);
            if let Some(id) = &d.record {
                claim
                    .attributes
                    .insert(attr.clone(), Value::String(id.clone()));
            }
            let replaced = previous.filter(|p| claim.attributes.get(&attr) != Some(p));
            tally.atoms += 1;
            *tally.outcomes.entry(d.outcome.label()).or_default() += 1;
            tally.excluded += d.excluded.len();
            tally.unjudged += d.unjudged.len();
            tally.unresolved += d.unresolved.len();
            sink(&DerivedValue {
                stage: DeriveStage::AfterResolve,
                type_name: kind.clone(),
                attribute: attr.clone(),
                atom: claim.id.as_str().to_string(),
                derived: "reference".into(),
                outcome: d.outcome,
                excluded: d.excluded,
                unresolved: d.unresolved,
                unjudged: d.unjudged,
                replaced,
                protocol: None,
                by: SourcePrecision::new("derived:reference", Precision::Unmeasured),
            });
        }
        info!(r#type = %kind, attribute = %attr, atoms = tally.atoms, outcomes = ?tally.outcomes, excluded = tally.excluded, unjudged = tally.unjudged, unresolved = tally.unresolved, "atlas/derive: statements' references resolved");
        report.insert(format!("{kind}.{attr}"), tally);
    }
    Ok(())
}
