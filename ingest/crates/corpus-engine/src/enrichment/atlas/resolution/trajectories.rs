// SPDX-License-Identifier: AGPL-3.0-or-later
//! Step 3b's trajectory pass: each owner's states chained in section order.
//! Moved out of `resolution.rs` (2026-10-15) so the document reader's states,
//! made after RESOLVE (`resolution_records/statement_atoms.rs`), are chained
//! by the same code as an extracted one.

use std::collections::{BTreeMap, HashMap};

use super::{Trajectory, TrajectoryState, TrajectoryTransition};
use crate::enrichment::atlas::atoms::{AtomId, Entity, Event, Relation, State};
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use crate::enrichment::pipeline::atlas::SectionExtraction;

/// Trajectory index + Transition edges. Group states by their
/// entity_id (or relation_id — same field), sort by section
/// start, then emit one Transition per consecutive pair. Each
/// transition gets a best-effort deterministic `trigger_event`
/// when the evidence is unambiguous: an Event that lives in
/// the section window between `from` and `to` AND has the
/// owning entity as a participant. Ambiguous cases leave
/// `trigger_event = None` — we'd rather say "I don't know"
/// than attach a plausible-but-wrong event. Relation-owned
/// trajectories currently leave triggers None (Events don't
/// reference Relation ids in `participants`; a full match
/// would require participant-set intersection, and Phase 5
/// will handle relation-state triggers with evidence depth).
pub fn chain(
    states: &[State],
    entities: &[Entity],
    relations: &[Relation],
    events: &[Event],
    sections: &[SectionExtraction],
    edges: &mut Vec<Edge>,
) -> BTreeMap<String, Trajectory> {
    let section_ordinal = build_section_ordinal_map(sections);
    let mut next = next_edge_index(edges);
    let mut trajectories: BTreeMap<String, Trajectory> = BTreeMap::new();
    let states_by_owner = group_states_by_owner(states);
    for (owner_id, owner_states) in states_by_owner {
        let (owner_name, owner_atom_type) = owner_display(&owner_id, entities, relations);
        let mut traj_states = Vec::with_capacity(owner_states.len());
        let mut traj_transitions = Vec::with_capacity(owner_states.len().saturating_sub(1));
        for state in &owner_states {
            traj_states.push(TrajectoryState {
                state_id: state.id.as_str().to_string(),
                label: state.label.clone(),
                section_range: state.section_range.clone(),
            });
        }
        for pair in owner_states.windows(2) {
            let (from, to) = (&pair[0], &pair[1]);
            let trigger = find_trigger_event(&owner_id, from, to, events, &section_ordinal);
            next += 1;
            edges.push(Edge {
                id: EdgeId::new(next - 1),
                edge_type: EdgeType::Transition,
                source: from.id.clone(),
                target: to.id.clone(),
                evidence: Vec::new(),
                trigger_event: trigger.clone(),
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
            // The same determination, published where a graph walk can
            // follow it. `trigger_event` is a field ON the transition, and
            // the grounding BFS traverses source → target, so the Event is
            // unreachable from a trajectory walk through that field alone.
            // That is why the pre-registered trajectory row walks
            // `[Transition, Causes]` (EPISTEMIC_INDEX §2.2) while `Causes`
            // had no producer anywhere in the workspace, and the row ground
            // on half the vocabulary it declares. ONE decider —
            // `find_trigger_event`, called once above — and two publications
            // of its answer: the field narrates ("triggered by…"), the edge
            // is walkable. No new inference, and nothing is emitted for an
            // ambiguous trigger, which stays `None` on both.
            if let Some(trigger_id) = &trigger {
                next += 1;
                edges.push(Edge {
                    id: EdgeId::new(next - 1),
                    edge_type: EdgeType::Causes,
                    source: trigger_id.clone(),
                    target: to.id.clone(),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
            traj_transitions.push(TrajectoryTransition {
                from: from.id.as_str().to_string(),
                to: to.id.as_str().to_string(),
                trigger_event: trigger.map(|id| id.as_str().to_string()),
            });
        }
        trajectories.insert(
            owner_id.as_str().to_string(),
            Trajectory {
                canonical_name: owner_name,
                atom_type: owner_atom_type,
                states: traj_states,
                transitions: traj_transitions,
            },
        );
    }
    trajectories
}

/// The next free sequential edge index. `len() + 1` is not it once a step
/// removed edges (RESOLVE's `retire`, a re-chain), and reusing a live id
/// shadows an edge; on a gap-free set the two agree.
pub fn next_edge_index(edges: &[Edge]) -> usize {
    edges
        .iter()
        .filter_map(|e| e.id.as_str().strip_prefix("edge-")?.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1
}

fn group_states_by_owner(states: &[State]) -> Vec<(AtomId, Vec<State>)> {
    let mut map: HashMap<String, Vec<State>> = HashMap::new();
    for s in states {
        map.entry(s.entity_id.as_str().to_string())
            .or_default()
            .push(s.clone());
    }
    // Deterministic order by owner id for stable transition
    // numbering across runs. Within an owner, sort states by section
    // start — the trajectory pass assumes this order.
    let mut entries: Vec<(AtomId, Vec<State>)> = map
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_by(|a, b| a.section_range.start.cmp(&b.section_range.start));
            (AtomId::from_raw(k), v)
        })
        .collect();
    entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    entries
}

/// Map `section_id` → ordinal position in the corpus. Used by the
/// Transition-trigger matcher to decide which section ids fall
/// between a `from` state and a `to` state. Building this from the
/// order `sections` arrive in is deterministic across runs.
fn build_section_ordinal_map(sections: &[SectionExtraction]) -> HashMap<String, usize> {
    sections
        .iter()
        .enumerate()
        .map(|(i, s)| (s.section_id.clone(), i))
        .collect()
}

/// Best-effort trigger-event match for one (from, to) transition.
///
/// Returns `Some(event_id)` only when the match is unambiguous:
///
///   1. Owner is an Entity (Relation-owned trajectories leave
///      triggers None until Phase 5 — see the comment at the call
///      site).
///   2. Exactly one Event in the corpus has `section_position` in
///      the ordinal window `(from.section, to.section]` AND lists
///      the owner as a participant.
///
/// Two+ matches, zero matches, or an unknown section id all return
/// `None`. The conservative stance is deliberate: a wrong trigger
/// pollutes the trajectory interpretation more than a missing one
/// (downstream code already handles `trigger_event = None`).
fn find_trigger_event(
    owner_id: &AtomId,
    from: &State,
    to: &State,
    events: &[Event],
    section_ordinal: &HashMap<String, usize>,
) -> Option<AtomId> {
    if !owner_id.as_str().starts_with("entity-") {
        return None;
    }
    let from_ord = section_ordinal.get(&from.section_range.end)?;
    let to_ord = section_ordinal.get(&to.section_range.start)?;
    if to_ord <= from_ord {
        return None;
    }
    let mut matches: Vec<&Event> = events
        .iter()
        .filter(|e| {
            let Some(ord) = section_ordinal.get(&e.section_position.section_id) else {
                return false;
            };
            // Half-open window (from_ord, to_ord] — the trigger sits
            // strictly after `from` and at or before `to`.
            *ord > *from_ord && *ord <= *to_ord && e.participants.iter().any(|p| p == owner_id)
        })
        .collect();
    if matches.len() == 1 {
        return Some(matches.remove(0).id.clone());
    }
    None
}

fn owner_display(
    owner_id: &AtomId,
    entities: &[Entity],
    relations: &[Relation],
) -> (String, String) {
    if let Some(e) = entities.iter().find(|e| e.id == *owner_id) {
        return (e.canonical_name.clone(), "Entity".into());
    }
    if let Some(r) = relations.iter().find(|r| r.id == *owner_id) {
        return (r.label.clone(), "Relation".into());
    }
    // Unknown owner shouldn't happen — every state's entity_id
    // either points at an Entity (from Step 3a) or a Relation (from
    // Step 3b). Return a sentinel so downstream code can log.
    (owner_id.as_str().to_string(), "Unknown".into())
}
