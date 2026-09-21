// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 3b trajectories — transition triggers and the `Causes` edge.
//!
//! Sibling of `tests.rs`; see that file's note on the split.

use super::tests_fixtures::*;
use super::*;
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntitySketch, EntityType, EventSketch};
use std::sync::Arc;

// ── Transition trigger matching ─────────────────────────

fn single_entity(idx: usize, name: &str) -> super::super::atoms::Entity {
    use super::super::atoms::{AtomId, ChunkRef};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    super::super::atoms::Entity {
        id: AtomId::entity(idx),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: "x".into(),
        salience: 1.0,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        defining_quote: None,
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    }
}

fn entity_event(
    idx: usize,
    section_id: &str,
    participants: Vec<super::super::atoms::AtomId>,
) -> super::super::atoms::Event {
    use super::super::atoms::{AtomId, SectionPosition};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EventType};
    super::super::atoms::Event {
        attributes: Default::default(),
        id: AtomId::event(idx),
        description: format!("event {idx}"),
        event_type: EventType::Other("x".into()),
        participants,
        evidence: Vec::new(),
        section_position: SectionPosition::section(section_id),
        causal_antecedents: Vec::new(),
        enrichment_depth: EnrichmentDepth::Extracted,
    }
}

#[test]
fn transition_trigger_attaches_unique_event_in_window_with_owner_participant() {
    // Alyosha's state moves from sec_0001 ("at the monastery") to
    // sec_0003 ("leaving"). A single event in sec_0002 has
    // Alyosha as participant — that's the unambiguous trigger.
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityStateSketch};

    let entities = vec![single_entity(1, "Alyosha")];
    let events = vec![entity_event(
        1,
        "sec_0002",
        vec![super::super::atoms::AtomId::entity(1)],
    )];
    let sections = vec![
        SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "At the monastery".into(),
                anchor: String::new(),
                state_type: None,
            }],
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0002".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0003".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Leaving".into(),
                anchor: String::new(),
                state_type: None,
            }],
            ..Default::default()
        },
    ];
    let out = resolve_step_3b(&sections, &entities, &events).unwrap();
    let transitions: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Transition)
        .collect();
    assert_eq!(transitions.len(), 1);
    assert_eq!(
        transitions[0].trigger_event.as_ref().map(|id| id.as_str()),
        Some("event-0001")
    );
    // Trajectory should mirror the edge's trigger.
    let traj = out
        .trajectories
        .get(super::super::atoms::AtomId::entity(1).as_str())
        .unwrap();
    assert_eq!(
        traj.transitions[0].trigger_event.as_deref(),
        Some("event-0001")
    );

    // …and the same trigger is a walkable edge. Without this the
    // trajectory row walks `[Transition, Causes]` over a graph that
    // carries no `Causes` at all, and the triggering Event can only be
    // reached by a reader that already knows to look inside a transition.
    let causes: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Causes)
        .collect();
    assert_eq!(causes.len(), 1, "one trigger, one Causes edge");
    assert_eq!(causes[0].source.as_str(), "event-0001");
    assert_eq!(
        causes[0].target.as_str(),
        transitions[0].target.as_str(),
        "the event caused the state the transition arrives at"
    );
}

/// The conservative stance carries to the edge: an ambiguous trigger
/// produces no `Causes`, rather than a plausible one. Failing input is
/// the same two-event window that keeps `trigger_event` at `None`.
#[test]
fn an_ambiguous_trigger_emits_no_causes_edge() {
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityStateSketch};

    let entities = vec![single_entity(1, "Alyosha")];
    let events = vec![
        entity_event(1, "sec_0002", vec![super::super::atoms::AtomId::entity(1)]),
        entity_event(2, "sec_0002", vec![super::super::atoms::AtomId::entity(1)]),
    ];
    let state = |label: &str| EntityStateSketch {
        entity_name: "Alyosha".into(),
        label: label.into(),
        anchor: String::new(),
        state_type: None,
    };
    let sections = vec![
        SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![state("At the monastery")],
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0002".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0003".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![state("Leaving")],
            ..Default::default()
        },
    ];
    let out = resolve_step_3b(&sections, &entities, &events).unwrap();
    assert!(out
        .edges
        .iter()
        .any(|e| e.edge_type == EdgeType::Transition));
    assert!(
        !out.edges.iter().any(|e| e.edge_type == EdgeType::Causes),
        "two candidate events prove neither; no Causes edge"
    );
}

#[test]
fn transition_trigger_stays_none_on_ambiguous_match() {
    // Two events in the window, both with Alyosha as participant
    // → we can't prove which is the trigger, so leave None rather
    // than pick one arbitrarily.
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityStateSketch};

    let entities = vec![single_entity(1, "Alyosha")];
    let events = vec![
        entity_event(1, "sec_0002", vec![super::super::atoms::AtomId::entity(1)]),
        entity_event(2, "sec_0002", vec![super::super::atoms::AtomId::entity(1)]),
    ];
    let sections = vec![
        SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Before".into(),
                anchor: String::new(),
                state_type: None,
            }],
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0002".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0003".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "After".into(),
                anchor: String::new(),
                state_type: None,
            }],
            ..Default::default()
        },
    ];
    let out = resolve_step_3b(&sections, &entities, &events).unwrap();
    let transitions: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Transition)
        .collect();
    assert_eq!(transitions.len(), 1);
    assert!(transitions[0].trigger_event.is_none());
}

#[test]
fn resolver_surfaces_structured_failures_for_silent_drops() {
    // Dirty input exercises every Phase-3b silent-drop path:
    // (1) entity-state sketch names an unknown entity,
    // (2) relation-introduced sketch has only one resolvable
    //     participant,
    // (3) relation-developed sketch has zero resolvable
    //     participants,
    // (4) claim attributed_to names an unknown entity.
    //
    // Before Landing 3.A all four went to `debug!` and were
    // lost. Now they land in `Step3bOutput.failures` as typed
    // records the `enrich errors` aggregator can group.
    use crate::enrichment::pipeline::atlas::{
        ClaimSketch, DiscourseAct, EnrichmentDepth, EntityStateSketch, EpistemicStatus,
        RelationSketch, RelationStateSketch,
    };
    use crate::enrichment::pipeline::types::PhaseFailureKind;

    let entities = vec![single_entity(1, "Alyosha")];
    let sections = vec![SectionExtraction {
        section_id: "sec_0001".into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        entities_developed: vec![EntityStateSketch {
            entity_name: "Mystery Person".into(), // (1) unknown
            label: "distressed".into(),
            anchor: String::new(),
            state_type: None,
        }],
        relations_introduced: vec![RelationSketch {
            attributes: Default::default(),
            relation_type: None,
            participants: vec!["Alyosha".into(), "Unknown Person".into()], // (2) one unresolved
            label: "doomed partnership".into(),
            anchor: String::new(),
        }],
        relations_developed: vec![RelationStateSketch {
            participants: vec!["Ghost A".into(), "Ghost B".into()], // (3) both unresolved
            label: "phantom bond".into(),
            anchor: String::new(),
            state_type: None,
        }],
        claims: vec![ClaimSketch {
            attributes: Default::default(),
            claim_kind: None,
            subject: None,
            scope: None,
            content: "Faith is hard-won.".into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            attributed_to: Some("Someone Else".into()), // (4) unknown attribution
            anchor: String::new(),
            quotable_excerpt: None,
        }],
        ..Default::default()
    }];

    let out = resolve_step_3b(&sections, &entities, &[]).unwrap();

    let kinds: Vec<PhaseFailureKind> = out.failures.iter().map(|f| f.kind).collect();
    assert!(
        kinds.contains(&PhaseFailureKind::UnresolvedEntityName),
        "expected UnresolvedEntityName from case (1), got kinds: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&PhaseFailureKind::UnresolvedRelationParticipant),
        "expected UnresolvedRelationParticipant from case (2)/(3), got kinds: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&PhaseFailureKind::UnresolvedClaimAttribution),
        "expected UnresolvedClaimAttribution from case (4), got kinds: {:?}",
        kinds
    );
    // The relation-developed case should contribute two
    // UnresolvedRelationParticipant records (one per unresolved
    // participant name) — this is what lets the aggregator count
    // drops at name-granularity rather than sketch-granularity.
    let relation_drops: Vec<_> = out
        .failures
        .iter()
        .filter(|f| f.kind == PhaseFailureKind::UnresolvedRelationParticipant)
        .collect();
    assert!(
        relation_drops.len() >= 3,
        "expected ≥ 3 relation-participant drops (1 from sketch (2), 2 from sketch (3)), got {}",
        relation_drops.len()
    );
    // Subjects carry the sketch-scoped prefix so the aggregator
    // can trace a group back to its exact origin.
    assert!(out
        .failures
        .iter()
        .any(|f| { f.subject.starts_with("sketch:entity_state:sec_0001#") }));
    assert!(out.failures.iter().any(|f| {
        f.subject
            .starts_with("sketch:relation_introduced:sec_0001#")
    }));
    assert!(out
        .failures
        .iter()
        .any(|f| { f.subject.starts_with("sketch:relation_developed:sec_0001#") }));
    assert!(out
        .failures
        .iter()
        .any(|f| { f.subject.starts_with("sketch:claim:sec_0001#") }));
    // Claim content is still emitted — only attribution is lost.
    assert_eq!(out.claims.len(), 1);
    assert_eq!(out.claims[0].attributed_to, None);
}

#[test]
fn resolver_emits_none_confidence_on_derived_states_and_claims() {
    // Glassbox invariant behind the confidence-histogram fix:
    // the deterministic Phase 3b resolver must never stamp a
    // fake `Some(1.0)` on atoms it derives. Phase 5 (atom
    // interpretation) will replace `None` with a real score.
    // Until then, honest `None` keeps the schema-validation
    // histogram reflecting only LLM-reported confidence.
    use crate::enrichment::pipeline::atlas::{
        ClaimSketch, DiscourseAct, EnrichmentDepth, EntityStateSketch, EpistemicStatus,
    };

    let entities = vec![single_entity(1, "Alyosha")];
    let sections = vec![SectionExtraction {
        section_id: "sec_0001".into(),
        enrichment_depth: EnrichmentDepth::Extracted,
        entities_developed: vec![EntityStateSketch {
            entity_name: "Alyosha".into(),
            label: "resolute".into(),
            anchor: String::new(),
            state_type: None,
        }],
        claims: vec![ClaimSketch {
            attributes: Default::default(),
            claim_kind: None,
            subject: None,
            scope: None,
            content: "Active love is harder than dreamt love.".into(),
            discourse_act: DiscourseAct::Argue,
            epistemic_status: EpistemicStatus::Confident,
            attributed_to: Some("Alyosha".into()),
            anchor: String::new(),
            quotable_excerpt: None,
        }],
        ..Default::default()
    }];
    let out = resolve_step_3b(&sections, &entities, &[]).unwrap();
    assert_eq!(out.states.len(), 1);
    assert!(
        out.states[0].confidence.is_none(),
        "derived state must not stamp a fake LLM confidence"
    );
    assert_eq!(out.claims.len(), 1);
    assert!(
        out.claims[0].confidence.is_none(),
        "derived claim must not stamp a fake LLM confidence"
    );
}

#[test]
fn transition_trigger_stays_none_when_no_event_in_window_names_owner() {
    // The only event in the window is about a different entity
    // (Ivan), not Alyosha. The owner-participant filter drops it,
    // so there's no match → None.
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityStateSketch};

    let entities = vec![single_entity(1, "Alyosha"), single_entity(2, "Ivan")];
    let events = vec![entity_event(
        1,
        "sec_0002",
        vec![super::super::atoms::AtomId::entity(2)],
    )];
    let sections = vec![
        SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "Before".into(),
                anchor: String::new(),
                state_type: None,
            }],
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0002".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            ..Default::default()
        },
        SectionExtraction {
            section_id: "sec_0003".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![EntityStateSketch {
                entity_name: "Alyosha".into(),
                label: "After".into(),
                anchor: String::new(),
                state_type: None,
            }],
            ..Default::default()
        },
    ];
    let out = resolve_step_3b(&sections, &entities, &events).unwrap();
    let alyosha_transitions: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Transition && e.source.as_str().starts_with("state-"))
        .collect();
    assert_eq!(alyosha_transitions.len(), 1);
    assert!(alyosha_transitions[0].trigger_event.is_none());
}

#[tokio::test]
async fn atlas_resolve_synthesis_resolves_later_mentions_via_fuzzy_match() {
    // Phase 1 names "Daniel Dennett" as a participant in sec_0001
    // and the shorter form "Dennett" in sec_0002. Without
    // synthesis both mentions drop. With synthesis, the first
    // creates a minimal `Daniel Dennett` Entity, and the second
    // resolves to that same atom via the fuzzy long-token path —
    // no duplicate synthesis. Event descriptions are crafted
    // with very different first-byte profiles so the deterministic
    // fake embed yields cosine well below the merge thresholds —
    // the events stay distinct across sections.
    let sections = vec![
        section(
            "sec_0001",
            vec![entity("Harry Frankfurt", &[], "Frankfurt cases author.")],
            vec![event(
                "\0Anomalous: Frankfurt cases challenge PAP",
                &["Harry Frankfurt", "Daniel Dennett"],
            )],
        ),
        section(
            "sec_0002",
            vec![],
            vec![event(
                "zMajestic compatibilism defense by Dennett",
                &["Dennett"],
            )],
        ),
    ];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();

    // Two atoms total: the Phase-1 Frankfurt + a single
    // synthesized `Daniel Dennett`. The sec_0002 `Dennett`
    // mention must NOT trigger a second synthesis.
    assert_eq!(
        out.entities.len(),
        2,
        "fuzzy match against the synthesized atom should prevent a duplicate"
    );
    let dennett = out
        .entities
        .iter()
        .find(|e| e.canonical_name == "Daniel Dennett")
        .expect("synthesized Dennett entity missing");
    assert!(
        (dennett.salience - SYNTHESIZED_ENTITY_SALIENCE).abs() < 1e-6,
        "synthesized atoms must carry the indirect-evidence salience tier"
    );
    assert_eq!(dennett.first_appearance.chunk_id, "sec_0001");

    // sec_0001 event has 2 Involves, sec_0002 event has 1 — three
    // total. If the events had merged the count would be 2.
    assert_eq!(out.events.len(), 2, "events must stay distinct");
    let involves_edges = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Involves)
        .count();
    assert_eq!(involves_edges, 3);

    assert!(
        out.failures.is_empty(),
        "synthesis should clear participant failures: {:?}",
        out.failures
    );
}

#[tokio::test]
async fn atlas_resolve_synthesis_skips_empty_and_whitespace_participants() {
    // The synthesizer must not invent atoms from blank strings —
    // the LLM occasionally emits empty participant slots and we
    // should silently skip those rather than create a zero-name
    // entity.
    let sections = vec![section(
        "sec_0001",
        vec![],
        vec![event(
            "Anonymous event with blank participant",
            &["", "   "],
        )],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert!(
        out.entities.is_empty(),
        "blank participants must not synthesize entities"
    );
}

fn typed_entity(name: &str, ty: crate::enrichment::pipeline::atlas::EntityType) -> EntitySketch {
    EntitySketch {
        attributes: Default::default(),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: ty,
        description: String::new(),
        anchor: String::new(),
        defining_quote: None,
    }
}

#[tokio::test]
async fn atlas_resolve_collapses_typo_fragmented_entity_atoms() {
    // Models with weaker spelling (Qwopus 9B Q8 on sep-compatibilism)
    // emit four distinct entity atoms for the same canonical
    // concept: "Classical Compatibilism" alongside three typo
    // variants. Empty descriptions disable the existing Rule 2 /
    // Rule 3.5 cosine-driven merges, so the existing resolver
    // lets all four through. The post-synthesis typo-dedup pass
    // collapses them into a single atom whose aliases preserve
    // the variant spellings for audit.
    use crate::enrichment::pipeline::atlas::EntityType;
    let sections = vec![section(
        "sec_0001",
        vec![
            typed_entity("Classical Compatibilism", EntityType::Concept),
            typed_entity("Classical Compatiblistism", EntityType::Concept),
            typed_entity("Classical compatbilism", EntityType::Concept),
            typed_entity("Classical compatibelism", EntityType::Concept),
        ],
        vec![],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        1,
        "all four typo variants should collapse into a single atom; got: {:?}",
        out.entities
            .iter()
            .map(|e| &e.canonical_name)
            .collect::<Vec<_>>()
    );
    let survivor = &out.entities[0];
    // The three loser spellings must surface as aliases so an
    // operator (or downstream Phase 5) can audit which forms got
    // folded together.
    for variant in [
        "Classical Compatiblistism",
        "Classical compatbilism",
        "Classical compatibelism",
    ] {
        let canonical_match = survivor.canonical_name.eq_ignore_ascii_case(variant);
        let alias_match = survivor
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case(variant));
        assert!(
            canonical_match || alias_match,
            "loser spelling {variant:?} should survive as canonical or alias; \
             canonical={:?} aliases={:?}",
            survivor.canonical_name,
            survivor.aliases
        );
    }
}

#[tokio::test]
async fn atlas_resolve_typo_dedup_does_not_merge_prefix_distinct_concepts() {
    // "Compatibilism" and "Incompatibilism" are folded-Lev 2 — a
    // pure edit-distance check would collapse them. The first-4-
    // chars prefix guard cleanly separates the two: "comp" vs
    // "inco". Both atoms must remain distinct after the dedup pass.
    use crate::enrichment::pipeline::atlas::EntityType;
    let sections = vec![section(
        "sec_0001",
        vec![
            typed_entity("Compatibilism", EntityType::Concept),
            typed_entity("Incompatibilism", EntityType::Concept),
        ],
        vec![],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    let names: Vec<&str> = out
        .entities
        .iter()
        .map(|e| e.canonical_name.as_str())
        .collect();
    assert!(
        names.contains(&"Compatibilism") && names.contains(&"Incompatibilism"),
        "prefix-distinct concepts must stay separate; got: {names:?}"
    );
    assert_eq!(out.entities.len(), 2);
}

#[tokio::test]
async fn atlas_resolve_typo_dedup_skips_short_names() {
    // "Wolf" / "Wolfe" / "Wolff" are folded-Lev 0/1 from each
    // other but each sits below TYPO_DEDUP_MIN_FOLDED_LEN. The
    // dedup pass must keep its hands off short names — the
    // existing alias and shared-token rules (or human review)
    // are the right tool there.
    use crate::enrichment::pipeline::atlas::EntityType;
    let sections = vec![section(
        "sec_0001",
        vec![
            typed_entity("Wolf", EntityType::Person),
            typed_entity("Wolfe", EntityType::Person),
            typed_entity("Wolff", EntityType::Person),
        ],
        vec![],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(
        out.entities.len(),
        3,
        "short names must not be typo-merged; got: {:?}",
        out.entities
            .iter()
            .map(|e| &e.canonical_name)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn atlas_resolve_typo_dedup_redirects_event_participants_to_survivor() {
    // The whole point of the dedup pass is keeping atlas
    // hygiene through to downstream Involves edges: an event
    // that names a typo-variant of a canonical entity should
    // resolve to the survivor's id, not to a ghost atom or a
    // dropped participant. Run with one canonical entity plus
    // a typo variant, then assert the event's involves edges
    // route through the survivor.
    use crate::enrichment::pipeline::atlas::EntityType;
    let sections = vec![section(
        "sec_0001",
        vec![
            typed_entity("Classical Compatibilism", EntityType::Concept),
            typed_entity("Classical Compatiblistism", EntityType::Concept),
        ],
        vec![event(
            "Classical Compatiblistism stakes its claim against the Consequence Argument.",
            &["Classical Compatiblistism"],
        )],
    )];
    let out = resolve_entities_and_events(&sections, &fake_embed())
        .await
        .unwrap();
    assert_eq!(out.entities.len(), 1, "typo variant should merge");
    let survivor_id = &out.entities[0].id;
    let involves: Vec<_> = out
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Involves)
        .collect();
    assert_eq!(involves.len(), 1, "one Involves edge for the one event");
    assert_eq!(
        &involves[0].target, survivor_id,
        "typo-named participant must route to the survivor atom"
    );
    assert!(
        out.failures.is_empty(),
        "no participant should drop after typo-dedup: {:?}",
        out.failures
    );
}

#[test]
fn typo_dedup_match_blocks_short_names() {
    use super::super::atoms::{AtomId, Entity};
    use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};
    let mk = |name: &str| Entity {
        id: AtomId::entity(1),
        canonical_name: name.into(),
        aliases: Vec::new(),
        entity_type: EntityType::Person,
        first_appearance: ChunkRef::new("sec_0001", None),
        description: String::new(),
        defining_quote: None,
        salience: 0.5,
        enrichment_depth: EnrichmentDepth::Extracted,
        affiliation: None,
        role: None,
        participants: Vec::new(),
        provenance: Default::default(),
        attributes: serde_json::Map::new(),
        concept_kind: None,
    };
    // Short names below TYPO_DEDUP_MIN_FOLDED_LEN must not match.
    assert!(!typo_dedup_match(&mk("Lewis"), &mk("Lewes")));
    // Long-enough names with prefix mismatch must not match.
    assert!(!typo_dedup_match(
        &mk("Compatibilism"),
        &mk("Incompatibilism")
    ));
    // Long-enough names with prefix match and Lev within the cap
    // must match.
    assert!(typo_dedup_match(&mk("Compatibilism"), &mk("Compatibelism")));
    // Different entity types must not match even when the names
    // are otherwise dedup-eligible.
    let person = Entity {
        entity_type: EntityType::Person,
        ..mk("Frankfurter")
    };
    let concept = Entity {
        entity_type: EntityType::Concept,
        ..mk("Frankfurter")
    };
    assert!(!typo_dedup_match(&person, &concept));
}
