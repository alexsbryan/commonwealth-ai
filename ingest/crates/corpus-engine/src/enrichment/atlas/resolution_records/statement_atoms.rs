// SPDX-License-Identifier: AGPL-3.0-or-later
//! A read state or relation statement (`OntologyTypeDecl::is_read_statement`)
//! is served as a claim of its type, cited, with its fields; after RESOLVE
//! and the reference derive (`resolution_derived::statement_refs`) it is
//! also a State or Relation atom beside that claim, the shape an extracted
//! one has (`resolution_ontology::emit_role_states` is the precedent), so
//! what reads those atoms (trajectories, configurations, patterns) reads the
//! document reader's too. One atom per statement, its id hashed from the
//! statement's own: a holder's "grief" stated in two chapters is two states
//! in its trajectory, never one. The trajectory of every holder given a
//! state is chained again over all its states (`resolution::trajectories`).
//! A statement whose holder or other endpoint holds no record, or a state
//! with no value read, makes no atom and is counted.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tracing::{debug, info};

use super::BuildAtoms;
use crate::enrichment::atlas::atoms::{AtomId, Relation, SectionRange, State};
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use crate::enrichment::atlas::resolution::trajectories;
use crate::enrichment::ontology::{OntologyPolicies, TypeKind};
use crate::enrichment::pipeline::atlas::{RelationType, SectionExtraction, StateType};
use understanding_vocab::ontology::decl::STATEMENT_TO;

/// What the projection made, and why it made nothing of a statement.
#[derive(Debug, Default)]
pub struct StatementAtoms {
    pub states: usize,
    pub relations: usize,
    pub skipped: BTreeMap<&'static str, usize>,
}

impl StatementAtoms {
    /// One line for the resolve step's output.
    pub fn summary(&self) -> String {
        let skipped = self
            .skipped
            .iter()
            .map(|(why, n)| format!("{why} {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "read statements as atoms: {} state(s), {} relation(s); none made of [{skipped}]",
            self.states, self.relations
        )
    }
}

/// Add a State or Relation atom for every read state or relation statement.
pub fn project(
    atoms: &mut BuildAtoms<'_>,
    policies: &OntologyPolicies,
    sections: &[SectionExtraction],
    corpus_id: &str,
) -> StatementAtoms {
    let types = &policies.shape.types;
    let kinds: BTreeMap<&str, _> = types
        .iter()
        .filter(|t| matches!(t.kind, TypeKind::State | TypeKind::Relation))
        .filter(|t| t.is_read_statement(types))
        .map(|t| (t.name.as_str(), t))
        .collect();
    let mut out = StatementAtoms::default();
    if kinds.is_empty() {
        return out;
    }
    let skip = |why: &'static str, claim: &AtomId, out: &mut StatementAtoms| {
        debug!(claim = %claim.as_str(), why, "atlas/statement_atoms: a read statement makes no atom");
        *out.skipped.entry(why).or_default() += 1;
    };
    let mut held: BTreeSet<AtomId> = BTreeSet::new();
    let (mut states, mut relations, mut edges) = (Vec::new(), Vec::new(), Vec::new());
    for claim in atoms.claims.iter() {
        let Some(decl) = claim.claim_kind.as_deref().and_then(|k| kinds.get(k)) else {
            continue;
        };
        let Some(holder) = claim.subject.clone() else {
            skip("no holder record", &claim.id, &mut out);
            continue;
        };
        let Some(section) = claim.evidence.first().map(|e| e.chunk_id.clone()) else {
            skip("no evidence", &claim.id, &mut out);
            continue;
        };
        let text = |name: &str| match claim.attributes.get(name) {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        };
        if decl.kind == TypeKind::State {
            let Some(value) = text(&decl.name) else {
                skip("no state value read", &claim.id, &mut out);
                continue;
            };
            let state_type = StateType::Other(decl.name.clone());
            // The statement's id is its essence; the value rides beside it.
            let key = format!("{value}|{}", claim.id.as_str());
            let id = AtomId::state_content_hash(&holder, &state_type, &key, corpus_id);
            edges.push((EdgeType::Involves, id.clone(), holder.clone(), Vec::new()));
            for e in &claim.evidence {
                let target = AtomId::from_raw(e.chunk_id.clone());
                edges.push((EdgeType::Grounds, id.clone(), target, vec![e.clone()]));
            }
            held.insert(holder.clone());
            states.push(State {
                id,
                entity_id: holder,
                label: value,
                state_type,
                evidence: claim.evidence.clone(),
                section_range: SectionRange::point(section),
                confidence: None,
                enrichment_depth: claim.enrichment_depth,
            });
        } else {
            let Some(to) = text(STATEMENT_TO).map(AtomId::from_raw) else {
                skip("no `to` record", &claim.id, &mut out);
                continue;
            };
            let relation_type = RelationType::Other(decl.name.clone());
            let participants = vec![holder, to];
            // The hash sorts participants; the statement's id keeps one
            // atom per statement, and `participants` keeps the direction.
            let id = AtomId::relation_content_hash(
                &participants,
                &relation_type,
                claim.id.as_str(),
                corpus_id,
            );
            for p in &participants {
                edges.push((EdgeType::Involves, id.clone(), p.clone(), Vec::new()));
            }
            let declared: BTreeSet<&str> =
                decl.attributes.iter().map(|a| a.name.as_str()).collect();
            relations.push(Relation {
                id,
                label: claim.content.clone(),
                participants,
                relation_type,
                evidence: claim.evidence.clone(),
                section_range: SectionRange::point(section),
                attributes: claim
                    .attributes
                    .iter()
                    .filter(|(k, _)| declared.contains(k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                enrichment_depth: claim.enrichment_depth,
            });
        }
    }
    out.states = states.len();
    out.relations = relations.len();
    atoms.states.extend(states);
    atoms.relations.extend(relations);
    let first = trajectories::next_edge_index(atoms.edges);
    for (i, (edge_type, source, target, evidence)) in edges.into_iter().enumerate() {
        atoms.edges.push(Edge {
            id: EdgeId::new(first + i),
            edge_type,
            source,
            target,
            evidence,
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        });
    }
    rechain(atoms, &held, sections);
    info!(states = out.states, relations = out.relations, skipped = ?out.skipped, holders = held.len(), "atlas/statement_atoms: read statements projected");
    out
}

/// Chain again the trajectory of every holder in `held`, over all its states:
/// its old Transition and Causes edges go, and `trajectories::chain` makes
/// them anew with the rest, as 3b does.
fn rechain(atoms: &mut BuildAtoms<'_>, held: &BTreeSet<AtomId>, sections: &[SectionExtraction]) {
    if held.is_empty() {
        return;
    }
    let owned: Vec<State> = atoms
        .states
        .iter()
        .filter(|s| held.contains(&s.entity_id))
        .cloned()
        .collect();
    let ids: BTreeSet<&AtomId> = owned.iter().map(|s| &s.id).collect();
    let before = atoms.edges.len();
    atoms.edges.retain(|e| match e.edge_type {
        EdgeType::Transition => !ids.contains(&e.source),
        EdgeType::Causes => !ids.contains(&e.target),
        _ => true,
    });
    debug!(
        holders = held.len(),
        states = owned.len(),
        dropped = before - atoms.edges.len(),
        "atlas/statement_atoms: holders' trajectories chained again"
    );
    let chained = trajectories::chain(
        &owned,
        atoms.entities.as_slice(),
        atoms.relations.as_slice(),
        atoms.events.as_slice(),
        sections,
        &mut *atoms.edges,
    );
    atoms.trajectories.extend(chained);
}

#[cfg(test)]
#[path = "statement_atoms_tests.rs"]
mod tests;
