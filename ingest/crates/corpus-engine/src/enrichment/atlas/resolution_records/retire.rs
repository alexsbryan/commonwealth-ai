// SPDX-License-Identifier: AGPL-3.0-or-later
//! Retiring the Phase-1 atoms of a type RESOLVE decides, and every reference
//! to them (split from `resolution_records.rs`, arch-gate's 800-line band).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use tracing::debug;

use super::super::atoms::AtomId;
use super::super::resolution_documents::failure;
use super::BuildAtoms;
use crate::enrichment::pipeline::types::{PhaseFailure, PhaseFailureKind};

/// Remove the `retired` entities and every reference to them. An atom that
/// cannot stand without one goes with it: a state of a retired atom, a
/// relation left with fewer than two participants, and in turn their states,
/// edges and trajectories. Returns what was dropped, by what held it.
pub(super) fn retire(
    atoms: &mut BuildAtoms<'_>,
    retired: &BTreeSet<AtomId>,
    type_name: &str,
    failures: &mut Vec<PhaseFailure>,
) -> BTreeMap<&'static str, usize> {
    let mut dropped: BTreeMap<&'static str, usize> = BTreeMap::new();
    if retired.is_empty() {
        return dropped;
    }
    let mut add = |what: &'static str, n: usize| {
        if n > 0 {
            *dropped.entry(what).or_default() += n;
        }
    };
    let reason = |id: &AtomId| {
        format!(
            "names `{type_name}` atom {}, retired: RESOLVE decides `{type_name}` from its \
             statements, and this mention is not one",
            id.as_str()
        )
    };
    let mut gone: BTreeSet<AtomId> = retired.clone();
    atoms.entities.retain(|e| !retired.contains(&e.id));
    atoms.events.retain(|e| !retired.contains(&e.id));
    for e in atoms.entities.iter_mut() {
        add("entity_participant", drop_ids(&mut e.participants, retired));
        add(
            "entity_attribute",
            drop_attr_refs(&mut e.attributes, retired),
        );
    }
    for ev in atoms.events.iter_mut() {
        add("event_participant", drop_ids(&mut ev.participants, retired));
        add(
            "event_attribute",
            drop_attr_refs(&mut ev.attributes, retired),
        );
    }
    atoms.relations.retain_mut(|r| {
        let before: Vec<AtomId> = r.participants.clone();
        let n = drop_ids(&mut r.participants, retired);
        add(
            "relation_attribute",
            drop_attr_refs(&mut r.attributes, retired),
        );
        if n == 0 || r.participants.len() >= 2 {
            add("relation_participant", n);
            return true;
        }
        let named = before
            .iter()
            .find(|p| retired.contains(*p))
            .cloned()
            .unwrap_or_else(|| r.id.clone());
        failures.push(failure(
            format!("atom:{}", r.id.as_str()),
            PhaseFailureKind::UnresolvedRelationParticipant,
            reason(&named),
        ));
        gone.insert(r.id.clone());
        add("relation", 1);
        false
    });
    atoms.states.retain(|s| {
        if !gone.contains(&s.entity_id) {
            return true;
        }
        failures.push(failure(
            format!("atom:{}", s.id.as_str()),
            PhaseFailureKind::UnresolvedEntityName,
            reason(&s.entity_id),
        ));
        gone.insert(s.id.clone());
        add("state", 1);
        false
    });
    for c in atoms.claims.iter_mut() {
        if let Some(s) = c.subject.take_if(|s| gone.contains(s)) {
            failures.push(failure(
                format!("atom:{}", c.id.as_str()),
                PhaseFailureKind::UnresolvedClaimSubject,
                reason(&s),
            ));
            add("claim_subject", 1);
        }
        if let Some(a) = c.attributed_to.take_if(|a| gone.contains(a)) {
            failures.push(failure(
                format!("atom:{}", c.id.as_str()),
                PhaseFailureKind::UnresolvedClaimAttribution,
                reason(&a),
            ));
            add("claim_attribution", 1);
        }
        add("claim_attribute", drop_attr_refs(&mut c.attributes, &gone));
    }
    for a in atoms.argument_reconstructions.iter_mut() {
        add(
            "proponent",
            usize::from(a.proponent.take_if(|p| gone.contains(p)).is_some()),
        );
    }
    for p in atoms.positions.iter_mut() {
        add(
            "proponent",
            usize::from(p.proponent_id.take_if(|x| gone.contains(x)).is_some()),
        );
        add("position_evidence", drop_ids(&mut p.evidence_ids, &gone));
    }
    for o in atoms.oppositions.iter_mut() {
        for side in [&mut o.left_atom_id, &mut o.right_atom_id] {
            add(
                "opposition_side",
                usize::from(side.take_if(|x| gone.contains(x)).is_some()),
            );
        }
    }
    let edges = atoms.edges.len();
    atoms
        .edges
        .retain(|e| !gone.contains(&e.source) && !gone.contains(&e.target));
    add("edge", edges - atoms.edges.len());
    let gone_ids: BTreeSet<&str> = gone.iter().map(AtomId::as_str).collect();
    let chains = atoms.trajectories.len();
    atoms
        .trajectories
        .retain(|k, _| !gone_ids.contains(k.as_str()));
    add("trajectory", chains - atoms.trajectories.len());
    for tr in atoms.trajectories.values_mut() {
        tr.states
            .retain(|s| !gone_ids.contains(s.state_id.as_str()));
        tr.transitions
            .retain(|x| !gone_ids.contains(x.from.as_str()) && !gone_ids.contains(x.to.as_str()));
    }
    debug!(
        r#type = type_name,
        retired = retired.len(),
        ?dropped,
        "atlas/resolve: Phase-1 atoms retired"
    );
    dropped
}

/// Drop every id in `gone` from `ids`; how many went.
fn drop_ids(ids: &mut Vec<AtomId>, gone: &BTreeSet<AtomId>) -> usize {
    let before = ids.len();
    ids.retain(|id| !gone.contains(id));
    before - ids.len()
}

/// Drop every attribute value that is the id of an atom in `gone` (a `ref`
/// 3b snapped to it), and an attribute left with no value; how many went.
fn drop_attr_refs(attributes: &mut Map<String, Value>, gone: &BTreeSet<AtomId>) -> usize {
    let names = |v: &Value| {
        v.as_str()
            .is_some_and(|s| gone.iter().any(|g| g.as_str() == s))
    };
    let mut n = 0;
    attributes.retain(|_, v| match v {
        Value::Array(xs) => {
            let before = xs.len();
            xs.retain(|x| !names(x));
            n += before - xs.len();
            !(xs.is_empty() && before > 0)
        }
        other if names(other) => {
            n += 1;
            false
        }
        _ => true,
    });
    n
}
