//! Atlas traversal — execute a [`QueryPlan`] against the resolved
//! atlas and collect the atoms + edges that answer it.
//!
//! The engine is deterministic and cheap: no LLM, no embeddings.
//! It resolves the plan's target entities (via the same
//! salience-aware path Phase 3b uses) and then walks a bounded
//! subgraph of atoms + edges. The brief assembler renders the
//! result into prose; the engine itself has no opinions about
//! presentation.
//!
//! For each plan variant the engine collects:
//!
//! | Plan | Collected |
//! |------|-----------|
//! | `EntityLookup` | Target entity, its claims (via `attributed_to`), its events (via participants), its relations, its states (trajectory). |
//! | `Trajectory` | Target entity, ordered states, Transition edges, trigger events. |
//! | `RelationLookup` | The Relation atom between A and B (if any) + its states + its evidence. |
//! | `TensionList` | Open-question atoms + any Tension edges. |
//! | `ConfigurationList` | All Configuration atoms. |
//! | `CorpusOverview` | Top-salience entities, notable relations, configurations. |
//! | `Unknown` | Nothing — result marked `hit = false`. |

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::enrichment::atlas::atoms::{
    Claim, Configuration, Entity, Event, Question, Relation, ResolutionStatus, State,
};
use crate::enrichment::atlas::edges::{Edge, EdgeType};

use super::classifier::{QueryPlan, QueryTarget};

/// Atoms + edges the traversal found for a given plan, plus
/// metadata about whether the query resolved to anything. The
/// brief assembler consumes this verbatim.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraversalResult {
    /// True when the plan matched real atoms. Unknown plans +
    /// unresolved targets set this false.
    pub hit: bool,
    /// Short machine-readable tag identifying which walk path
    /// ran (`entity_lookup`, `trajectory`, …) — useful for
    /// telemetry + tests.
    pub kind: String,
    /// Human-readable headline the classifier/engine want to
    /// surface on the brief's first line. Empty when the brief
    /// assembler should derive its own headline.
    pub headline: String,

    pub entities: Vec<Entity>,
    pub events: Vec<Event>,
    pub states: Vec<State>,
    pub relations: Vec<Relation>,
    pub claims: Vec<Claim>,
    pub questions: Vec<Question>,
    pub configurations: Vec<Configuration>,
    pub edges: Vec<Edge>,
}

impl TraversalResult {
    fn miss(kind: &str, headline: impl Into<String>) -> Self {
        Self {
            hit: false,
            kind: kind.into(),
            headline: headline.into(),
            ..Default::default()
        }
    }

    fn hit(kind: &str, headline: impl Into<String>) -> Self {
        Self {
            hit: true,
            kind: kind.into(),
            headline: headline.into(),
            ..Default::default()
        }
    }
}

/// Pre-bundled atlas inputs for the traversal engine. Takes
/// borrows so the CLI can hand the engine its already-loaded
/// atom set without cloning.
#[derive(Debug, Clone, Copy)]
pub struct AtlasView<'a> {
    pub entities: &'a [Entity],
    pub events: &'a [Event],
    pub states: &'a [State],
    pub relations: &'a [Relation],
    pub claims: &'a [Claim],
    pub questions: &'a [Question],
    pub configurations: &'a [Configuration],
    pub edges: &'a [Edge],
}

/// Execute the plan. Deterministic: same atlas + same plan → same
/// result (modulo atom ordering, which mirrors the input).
pub fn traverse(plan: &QueryPlan, atlas: AtlasView<'_>) -> TraversalResult {
    match plan {
        QueryPlan::EntityLookup { target } => traverse_entity_lookup(target, atlas),
        QueryPlan::Trajectory { target } => traverse_trajectory(target, atlas),
        QueryPlan::RelationLookup { target_a, target_b } => {
            traverse_relation_lookup(target_a, target_b, atlas)
        }
        QueryPlan::TensionList => traverse_tension_list(atlas),
        QueryPlan::ConfigurationList => traverse_configuration_list(atlas),
        QueryPlan::CorpusOverview => traverse_corpus_overview(atlas),
        QueryPlan::Unknown { raw_query } => {
            TraversalResult::miss("unknown", format!("Unclassified query: {raw_query}"))
        }
    }
}

fn resolve_target<'a>(
    target: &QueryTarget,
    entities: &'a [Entity],
) -> Option<&'a Entity> {
    match target {
        QueryTarget::Resolved { entity_id, .. } => {
            entities.iter().find(|e| e.id.as_str() == entity_id)
        }
        QueryTarget::Unresolved { .. } => None,
    }
}

fn traverse_entity_lookup(target: &QueryTarget, atlas: AtlasView<'_>) -> TraversalResult {
    let Some(entity) = resolve_target(target, atlas.entities) else {
        let label = match target {
            QueryTarget::Resolved { matched_form, .. } => matched_form.clone(),
            QueryTarget::Unresolved { raw_name } => raw_name.clone(),
        };
        return TraversalResult::miss(
            "entity_lookup",
            format!("No entity atom matches '{label}' in this atlas."),
        );
    };

    let mut result = TraversalResult::hit(
        "entity_lookup",
        format!("Entity: {}", entity.canonical_name),
    );
    result.entities.push(entity.clone());

    // Claims attributed to this entity.
    for c in atlas.claims {
        if c.attributed_to.as_ref().map(|a| a == &entity.id).unwrap_or(false) {
            result.claims.push(c.clone());
        }
    }

    // Events this entity participates in.
    for e in atlas.events {
        if e.participants.iter().any(|p| p == &entity.id) {
            result.events.push(e.clone());
        }
    }

    // Relations this entity participates in.
    for r in atlas.relations {
        if r.participants.iter().any(|p| p == &entity.id) {
            result.relations.push(r.clone());
        }
    }

    // States owned by this entity (for the trajectory block).
    for s in atlas.states {
        if s.entity_id == entity.id {
            result.states.push(s.clone());
        }
    }
    // Sort states by section so the brief reads in reading order.
    result
        .states
        .sort_by(|a, b| a.section_range.start.cmp(&b.section_range.start));

    // Involves + Transition edges anchored on this entity's atoms.
    let my_state_ids: HashSet<&str> = result.states.iter().map(|s| s.id.as_str()).collect();
    for edge in atlas.edges {
        let touches = edge.source == entity.id
            || edge.target == entity.id
            || my_state_ids.contains(edge.source.as_str())
            || my_state_ids.contains(edge.target.as_str());
        if touches {
            result.edges.push(edge.clone());
        }
    }

    result
}

fn traverse_trajectory(target: &QueryTarget, atlas: AtlasView<'_>) -> TraversalResult {
    let Some(entity) = resolve_target(target, atlas.entities) else {
        let label = match target {
            QueryTarget::Resolved { matched_form, .. } => matched_form.clone(),
            QueryTarget::Unresolved { raw_name } => raw_name.clone(),
        };
        return TraversalResult::miss(
            "trajectory",
            format!("No entity atom matches '{label}' in this atlas."),
        );
    };

    let mut states: Vec<State> = atlas
        .states
        .iter()
        .filter(|s| s.entity_id == entity.id)
        .cloned()
        .collect();
    states.sort_by(|a, b| a.section_range.start.cmp(&b.section_range.start));

    if states.is_empty() {
        return TraversalResult::miss(
            "trajectory",
            format!("The atlas has no state atoms for {}.", entity.canonical_name),
        );
    }

    let mut result = TraversalResult::hit(
        "trajectory",
        format!("Trajectory: {}", entity.canonical_name),
    );
    result.entities.push(entity.clone());

    // Transition edges whose endpoints are both this entity's states.
    let my_state_ids: HashSet<&str> = states.iter().map(|s| s.id.as_str()).collect();
    for edge in atlas.edges {
        if edge.edge_type == EdgeType::Transition
            && my_state_ids.contains(edge.source.as_str())
            && my_state_ids.contains(edge.target.as_str())
        {
            result.edges.push(edge.clone());
            // If the transition names a trigger event, carry it
            // along so the brief can narrate "triggered by…".
            if let Some(trig_id) = &edge.trigger_event {
                if let Some(ev) = atlas.events.iter().find(|e| &e.id == trig_id) {
                    if !result.events.iter().any(|e| e.id == ev.id) {
                        result.events.push(ev.clone());
                    }
                }
            }
        }
    }

    result.states = states;
    result
}

fn traverse_relation_lookup(
    a: &QueryTarget,
    b: &QueryTarget,
    atlas: AtlasView<'_>,
) -> TraversalResult {
    let (ea, eb) = match (resolve_target(a, atlas.entities), resolve_target(b, atlas.entities)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let label_a = match a {
                QueryTarget::Resolved { matched_form, .. } => matched_form.clone(),
                QueryTarget::Unresolved { raw_name } => raw_name.clone(),
            };
            let label_b = match b {
                QueryTarget::Resolved { matched_form, .. } => matched_form.clone(),
                QueryTarget::Unresolved { raw_name } => raw_name.clone(),
            };
            return TraversalResult::miss(
                "relation_lookup",
                format!("At least one of '{label_a}' / '{label_b}' is not in the atlas."),
            );
        }
    };

    let matching: Vec<&Relation> = atlas
        .relations
        .iter()
        .filter(|r| r.participants.contains(&ea.id) && r.participants.contains(&eb.id))
        .collect();

    if matching.is_empty() {
        return TraversalResult::miss(
            "relation_lookup",
            format!(
                "The atlas records no direct relation between {} and {}.",
                ea.canonical_name, eb.canonical_name
            ),
        );
    }

    let mut result = TraversalResult::hit(
        "relation_lookup",
        format!(
            "Relations between {} and {}",
            ea.canonical_name, eb.canonical_name
        ),
    );
    result.entities.push(ea.clone());
    result.entities.push(eb.clone());
    let relation_ids: HashSet<String> = matching.iter().map(|r| r.id.as_str().to_string()).collect();
    result.relations = matching.into_iter().cloned().collect();

    // States owned by the relation(s).
    for s in atlas.states {
        if relation_ids.contains(s.entity_id.as_str()) {
            result.states.push(s.clone());
        }
    }
    result
        .states
        .sort_by(|a, b| a.section_range.start.cmp(&b.section_range.start));

    // Any Involves edges connecting the relations to these entities.
    for edge in atlas.edges {
        if edge.edge_type == EdgeType::Involves
            && relation_ids.contains(edge.source.as_str())
        {
            result.edges.push(edge.clone());
        }
    }

    result
}

fn traverse_tension_list(atlas: AtlasView<'_>) -> TraversalResult {
    // Open questions are the corpus's first-class tensions today
    // (Landing 3 ships a candidate list + gap detector; LLM
    // classification to real Tension edges is a follow-up).
    let open: Vec<Question> = atlas
        .questions
        .iter()
        .filter(|q| matches!(q.resolution_status, ResolutionStatus::Open))
        .cloned()
        .collect();
    let tension_edges: Vec<Edge> = atlas
        .edges
        .iter()
        .filter(|e| e.edge_type == EdgeType::Tension)
        .cloned()
        .collect();
    if open.is_empty() && tension_edges.is_empty() {
        return TraversalResult::miss(
            "tension_list",
            "No open questions or tension edges on this atlas.".to_string(),
        );
    }
    let mut result = TraversalResult::hit(
        "tension_list",
        format!(
            "{} open question(s), {} tension edge(s)",
            open.len(),
            tension_edges.len()
        ),
    );
    result.questions = open;
    result.edges = tension_edges;
    result
}

fn traverse_configuration_list(atlas: AtlasView<'_>) -> TraversalResult {
    if atlas.configurations.is_empty() {
        return TraversalResult::miss(
            "configuration_list",
            "The atlas has no Configuration atoms. Run `sovereign enrich atlas-configuration \
             <corpus>` (opt-in per pipeline)."
                .to_string(),
        );
    }
    let mut result = TraversalResult::hit(
        "configuration_list",
        format!("{} configuration(s)", atlas.configurations.len()),
    );
    result.configurations = atlas.configurations.to_vec();
    result
}

fn traverse_corpus_overview(atlas: AtlasView<'_>) -> TraversalResult {
    // Top-salience entities (max 8 so the brief stays scannable).
    let mut ents: Vec<Entity> = atlas.entities.to_vec();
    ents.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    ents.truncate(8);

    // First 8 relations as a sketch of the relational structure.
    let rels: Vec<Relation> = atlas.relations.iter().take(8).cloned().collect();

    let mut result = TraversalResult::hit(
        "corpus_overview",
        format!(
            "{} entities / {} relations / {} configurations",
            atlas.entities.len(),
            atlas.relations.len(),
            atlas.configurations.len()
        ),
    );
    result.entities = ents;
    result.relations = rels;
    result.configurations = atlas.configurations.to_vec();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::atoms::{
        AtomId, ChunkRef, Claim, Entity, Question, Relation, ResolutionStatus, SectionRange,
        State,
    };
    use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, QuestionType,
        RelationType, StateType,
    };

    fn entity(idx: usize, name: &str) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: format!("{name} description"),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn state(idx: usize, owner: usize, label: &str, section: &str) -> State {
        State {
            id: AtomId::from_raw(format!("state-{idx:04}")),
            entity_id: AtomId::entity(owner),
            label: label.into(),
            state_type: StateType::Other("unknown".into()),
            evidence: vec![],
            section_range: SectionRange::point(section),
            confidence: Some(1.0),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn relation(idx: usize, label: &str, a: usize, b: usize) -> Relation {
        Relation {
            id: AtomId::relation(idx),
            label: label.into(),
            participants: vec![AtomId::entity(a), AtomId::entity(b)],
            relation_type: RelationType::Other("unclassified".into()),
            evidence: vec![],
            section_range: SectionRange::point("sec_0001"),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn transition(src: usize, tgt: usize) -> Edge {
        Edge {
            id: EdgeId::new(src * 100 + tgt),
            edge_type: EdgeType::Transition,
            source: AtomId::from_raw(format!("state-{src:04}")),
            target: AtomId::from_raw(format!("state-{tgt:04}")),
            evidence: vec![],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    fn claim(idx: usize, content: &str, attrib: Option<usize>) -> Claim {
        Claim {
            id: AtomId::claim(idx),
            content: content.into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            attributed_to: attrib.map(AtomId::entity),
            confidence: Some(0.9),
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn question(idx: usize, content: &str) -> Question {
        Question {
            id: AtomId::from_raw(format!("question-{idx:04}")),
            content: content.into(),
            question_type: QuestionType::Other("unknown".into()),
            addressed_by: vec![],
            raised_at: vec![ChunkRef::new("sec_0001", None)],
            resolution_status: ResolutionStatus::Open,
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn fixture() -> (
        Vec<Entity>,
        Vec<State>,
        Vec<Relation>,
        Vec<Edge>,
        Vec<Claim>,
        Vec<Question>,
    ) {
        let entities = vec![entity(1, "Alyosha"), entity(2, "Zossima"), entity(3, "Fyodor")];
        let states = vec![
            state(1, 1, "naive novice", "sec_0001"),
            state(2, 1, "resolves to leave", "sec_0003"),
            state(3, 2, "dying elder", "sec_0002"),
        ];
        let relations = vec![relation(1, "Mentor-mentee", 1, 2)];
        let edges = vec![transition(1, 2)];
        let claims = vec![claim(1, "Faith must act in the world", Some(2))];
        let questions = vec![question(1, "Can faith survive outside the cell?")];
        (entities, states, relations, edges, claims, questions)
    }

    fn view<'a>(
        entities: &'a [Entity],
        states: &'a [State],
        relations: &'a [Relation],
        edges: &'a [Edge],
        claims: &'a [Claim],
        questions: &'a [Question],
    ) -> AtlasView<'a> {
        AtlasView {
            entities,
            events: &[],
            states,
            relations,
            claims,
            questions,
            configurations: &[],
            edges,
        }
    }

    #[test]
    fn entity_lookup_collects_related_atoms() {
        let (ents, states, rels, edges, claims, questions) = fixture();
        let plan = QueryPlan::EntityLookup {
            target: QueryTarget::Resolved {
                entity_id: "entity-0002".into(),
                matched_form: "Zossima".into(),
            },
        };
        let result = traverse(&plan, view(&ents, &states, &rels, &edges, &claims, &questions));
        assert!(result.hit);
        assert_eq!(result.entities.len(), 1);
        assert_eq!(result.entities[0].canonical_name, "Zossima");
        assert_eq!(result.relations.len(), 1); // Mentor-mentee
        assert_eq!(result.claims.len(), 1); // Zossima's claim
        assert_eq!(result.states.len(), 1); // Zossima's dying elder state
    }

    #[test]
    fn entity_lookup_returns_miss_for_unresolved_target() {
        let (ents, states, rels, edges, claims, questions) = fixture();
        let plan = QueryPlan::EntityLookup {
            target: QueryTarget::Unresolved {
                raw_name: "Grushenka".into(),
            },
        };
        let result = traverse(&plan, view(&ents, &states, &rels, &edges, &claims, &questions));
        assert!(!result.hit);
        assert!(result.headline.contains("Grushenka"));
    }

    #[test]
    fn trajectory_orders_states_by_section_and_collects_transitions() {
        let (ents, states, rels, edges, claims, questions) = fixture();
        let plan = QueryPlan::Trajectory {
            target: QueryTarget::Resolved {
                entity_id: "entity-0001".into(),
                matched_form: "Alyosha".into(),
            },
        };
        let result = traverse(&plan, view(&ents, &states, &rels, &edges, &claims, &questions));
        assert!(result.hit);
        assert_eq!(result.states.len(), 2);
        // Section order — sec_0001 before sec_0003.
        assert_eq!(result.states[0].section_range.start, "sec_0001");
        assert_eq!(result.states[1].section_range.start, "sec_0003");
        assert_eq!(result.edges.len(), 1); // one transition
    }

    #[test]
    fn relation_lookup_finds_the_relation_between_two_entities() {
        let (ents, states, rels, edges, claims, questions) = fixture();
        let plan = QueryPlan::RelationLookup {
            target_a: QueryTarget::Resolved {
                entity_id: "entity-0001".into(),
                matched_form: "Alyosha".into(),
            },
            target_b: QueryTarget::Resolved {
                entity_id: "entity-0002".into(),
                matched_form: "Zossima".into(),
            },
        };
        let result = traverse(&plan, view(&ents, &states, &rels, &edges, &claims, &questions));
        assert!(result.hit);
        assert_eq!(result.relations.len(), 1);
        assert_eq!(result.relations[0].label, "Mentor-mentee");
    }

    #[test]
    fn relation_lookup_misses_when_no_direct_relation_exists() {
        let (ents, states, rels, edges, claims, questions) = fixture();
        let plan = QueryPlan::RelationLookup {
            target_a: QueryTarget::Resolved {
                entity_id: "entity-0001".into(),
                matched_form: "Alyosha".into(),
            },
            target_b: QueryTarget::Resolved {
                entity_id: "entity-0003".into(),
                matched_form: "Fyodor".into(),
            },
        };
        let result = traverse(&plan, view(&ents, &states, &rels, &edges, &claims, &questions));
        assert!(!result.hit);
        assert!(result.headline.contains("no direct relation"));
    }

    #[test]
    fn tension_list_returns_open_questions() {
        let (ents, states, rels, edges, claims, questions) = fixture();
        let result = traverse(
            &QueryPlan::TensionList,
            view(&ents, &states, &rels, &edges, &claims, &questions),
        );
        assert!(result.hit);
        assert_eq!(result.questions.len(), 1);
    }

    #[test]
    fn corpus_overview_truncates_to_top_salience() {
        // Make 10 entities with descending salience; overview
        // keeps the top 8.
        let mut entities: Vec<Entity> = (1..=10)
            .map(|i| {
                let mut e = entity(i, &format!("e{i}"));
                e.salience = 1.0 - (i as f32 * 0.05);
                e
            })
            .collect();
        // Shuffle so sort is exercised.
        entities.reverse();
        let result = traverse(
            &QueryPlan::CorpusOverview,
            AtlasView {
                entities: &entities,
                events: &[],
                states: &[],
                relations: &[],
                claims: &[],
                questions: &[],
                configurations: &[],
                edges: &[],
            },
        );
        assert!(result.hit);
        assert_eq!(result.entities.len(), 8);
        // Top entity should be e1 (highest salience).
        assert_eq!(result.entities[0].canonical_name, "e1");
    }
}
