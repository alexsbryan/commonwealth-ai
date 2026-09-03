// SPDX-License-Identifier: AGPL-3.0-or-later
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
    Claim, Configuration, Entity, Event, Opposition, Position, Question, Relation,
    ResolutionStatus, State,
};
use crate::enrichment::atlas::edges::{Edge, EdgeType};
use crate::enrichment::ontology::{OntologyPolicies, TypeIndex};

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
    /// Named-view atoms (Gap B). Populated by entity_lookup when
    /// the queried entity is the proponent of a position, or when
    /// the position's content references the entity. Empty for plan
    /// kinds that don't surface positions yet (`tension_list`,
    /// `corpus_overview` will be wired in v2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<Position>,
    /// Structural X-vs-Y framings (Gap B). Populated when one side
    /// of the binary resolves to the queried entity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oppositions: Vec<Opposition>,
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
    pub positions: &'a [Position],
    pub oppositions: &'a [Opposition],
    /// The corpus's declared ontology (`atlas/ontology.json`), or `None` when
    /// it declared nothing. Read by [`traverse_enumerate`] and
    /// [`traverse_aggregate`] only — plans that can only be produced when this
    /// is `Some`, so every pre-ontology walk is untouched.
    pub vocab: Option<&'a OntologyPolicies>,
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
        QueryPlan::Enumerate { entity_type } => traverse_enumerate(entity_type, atlas),
        QueryPlan::Aggregate { entity_type, over } => traverse_aggregate(entity_type, over, atlas),
        QueryPlan::Unknown { raw_query } => {
            TraversalResult::miss("unknown", format!("Unclassified query: {raw_query}"))
        }
    }
}

fn resolve_target<'a>(target: &QueryTarget, entities: &'a [Entity]) -> Option<&'a Entity> {
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
        if c.attributed_to
            .as_ref()
            .map(|a| a == &entity.id)
            .unwrap_or(false)
        {
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

    // Gap-B typed atoms: positions and oppositions touching this
    // entity. Two surfaces:
    //   - structural — `proponent_id` / `left_atom_id` /
    //     `right_atom_id` resolved to this entity at extract time.
    //   - textual — the position name/content or opposition
    //     left/right/framing mentions the entity's canonical name
    //     or any alias. Catches the common case where the
    //     resolver didn't snap a proponent string to an Entity
    //     (because the model used a surname-only or short-form
    //     reference) but the typed atom is still clearly about
    //     this entity.
    // Build a needle list from canonical_name + aliases — full forms
    // AND surname-or-token tail (so "Jane Jacobs" also matches "Jacobs"
    // alone). Tokens shorter than 4 chars dropped to avoid spurious
    // hits ("the", "and", "of").
    let mut entity_names_lower: Vec<String> = Vec::new();
    for raw in std::iter::once(entity.canonical_name.clone()).chain(entity.aliases.iter().cloned())
    {
        let lower = raw.to_lowercase();
        if lower.len() >= 4 && !entity_names_lower.contains(&lower) {
            entity_names_lower.push(lower.clone());
        }
        for tok in raw.split_whitespace() {
            let t = tok
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            if t.len() >= 4 && !entity_names_lower.contains(&t) {
                entity_names_lower.push(t);
            }
        }
    }
    let mentions_entity = |s: &str| -> bool {
        let lower = s.to_lowercase();
        entity_names_lower.iter().any(|n| lower.contains(n))
    };
    for p in atlas.positions {
        let structural = p.proponent_id.as_ref() == Some(&entity.id);
        let textual = mentions_entity(&p.canonical_name) || mentions_entity(&p.content);
        if structural || textual {
            result.positions.push(p.clone());
        }
    }
    for o in atlas.oppositions {
        let structural = o.left_atom_id.as_ref() == Some(&entity.id)
            || o.right_atom_id.as_ref() == Some(&entity.id);
        let textual = mentions_entity(&o.left_label)
            || mentions_entity(&o.right_label)
            || mentions_entity(&o.framing);
        if structural || textual {
            result.oppositions.push(o.clone());
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
            format!(
                "The atlas has no state atoms for {}.",
                entity.canonical_name
            ),
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
    let (ea, eb) = match (
        resolve_target(a, atlas.entities),
        resolve_target(b, atlas.entities),
    ) {
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
    let relation_ids: HashSet<String> =
        matching.iter().map(|r| r.id.as_str().to_string()).collect();
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
        if edge.edge_type == EdgeType::Involves && relation_ids.contains(edge.source.as_str()) {
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

/// Cap on how many atoms an enumeration or aggregation returns. Matches the
/// brief's scannability budget; `traverse_corpus_overview` uses 8 for a
/// sample, but an enumeration's whole point is completeness, so this is the
/// larger "a catalogue, not a sample" bound.
const ENUMERATE_MAX: usize = 64;

/// Every Entity of a declared type, including its `specializes` descendants.
///
/// What a headline calls instances of a declared type: the author's `label`
/// when they declared one, else the type name. One accessor, so the
/// enumeration and the tally cannot call the same type two different things.
///
/// `label` is SINGULAR by its own contract ("what the UI calls instances of
/// this type"), and an author's noun cannot be pluralised by a rule we own —
/// so the enumeration headline names the type and then counts
/// (`coin: 7 in this atlas`) rather than trying to agree in number. Until
/// 2026-09-03 it read `7 coin in this atlas` for every shipped template; the
/// only test that covered it declared a plural `label` no template carries.
fn declared_label(index: &TypeIndex, entity_type: &str) -> String {
    index
        .get(entity_type)
        .and_then(|d| d.label.clone())
        .unwrap_or_else(|| entity_type.to_string())
}

/// This is why an enumeration of `coin` returns the sceattas too: the atlas
/// stores each atom under its OWN declared subtype, and `sceatta specializes
/// coin` is what makes a sceatta a coin. The walk goes through
/// [`TypeIndex::is_a`] — the one place the chain is walked.
fn traverse_enumerate(entity_type: &str, atlas: AtlasView<'_>) -> TraversalResult {
    let Some(policies) = atlas.vocab else {
        // Unreachable via `classify_query_with` (the plan is only minted when
        // a vocabulary exists), but a hand-built plan must refuse rather than
        // silently enumerate on equality alone.
        return TraversalResult::miss(
            "enumerate",
            format!("No declared ontology in this atlas, so '{entity_type}' names no type."),
        );
    };
    let index = TypeIndex::from_policies(policies);
    let mut matched: Vec<Entity> = atlas
        .entities
        .iter()
        .filter(|e| index.is_a(e.entity_type.as_str_repr(), entity_type))
        .cloned()
        .collect();
    let total = matched.len();
    matched.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    matched.truncate(ENUMERATE_MAX);

    tracing::debug!(
        entity_type,
        total,
        returned = matched.len(),
        "atlas traversal: enumerate over declared type"
    );

    if matched.is_empty() {
        return TraversalResult::miss(
            "enumerate",
            format!("No {entity_type} atoms in this atlas."),
        );
    }
    let mut result = TraversalResult::hit(
        "enumerate",
        format!(
            "{}: {total} in this atlas",
            declared_label(&index, entity_type)
        ),
    );
    result.entities = matched;
    result
}

/// Tally the declared type's atoms by one of its declared attributes.
///
/// Entities and Claims both carry `attributes`, and a declared claim type is
/// as tallyable as a declared entity type ("how many attributions by grade"),
/// so both are walked. An atom missing the attribute is counted under
/// `(unset)` rather than dropped — an absence is reported, never defaulted.
fn traverse_aggregate(entity_type: &str, over: &str, atlas: AtlasView<'_>) -> TraversalResult {
    let Some(policies) = atlas.vocab else {
        return TraversalResult::miss(
            "aggregate",
            format!("No declared ontology in this atlas, so '{entity_type}' names no type."),
        );
    };
    let index = TypeIndex::from_policies(policies);
    const UNSET: &str = "(unset)";

    let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut bucket = |attrs: &serde_json::Map<String, serde_json::Value>| {
        let key = match attrs.get(over) {
            Some(serde_json::Value::String(s)) => match s.trim() {
                "" => UNSET.to_string(),
                t => t.to_string(),
            },
            Some(serde_json::Value::Null) | None => UNSET.to_string(),
            Some(v) => v.to_string(),
        };
        *tally.entry(key).or_insert(0) += 1;
    };

    let mut result = TraversalResult::hit("aggregate", String::new());
    for e in atlas.entities {
        if index.is_a(e.entity_type.as_str_repr(), entity_type) {
            bucket(&e.attributes);
            result.entities.push(e.clone());
        }
    }
    for c in atlas.claims {
        let subtype = c.claim_kind.as_deref().unwrap_or_default();
        if index.is_a(subtype, entity_type) {
            bucket(&c.attributes);
            result.claims.push(c.clone());
        }
    }

    let total: usize = tally.values().sum();
    tracing::debug!(
        entity_type,
        over,
        total,
        buckets = tally.len(),
        "atlas traversal: aggregate over declared attribute"
    );
    if total == 0 {
        return TraversalResult::miss(
            "aggregate",
            format!("No {entity_type} atoms in this atlas to tally by {over}."),
        );
    }
    let breakdown = tally
        .iter()
        .map(|(k, n)| format!("{k}: {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    result.headline = format!(
        "{total} {} by {over} — {breakdown}",
        declared_label(&index, entity_type)
    );
    result.entities.truncate(ENUMERATE_MAX);
    result.claims.truncate(ENUMERATE_MAX);
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
        AtomId, ChunkRef, Claim, Entity, Question, Relation, ResolutionStatus, SectionRange, State,
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
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    // ── ontology-v1 P5: declared-type walks ──────────────────

    use crate::recipe_templates::numismatics_policies as numismatics;

    /// A coin atom typed under the AUTHOR'S noun, with declared attributes.
    fn coin(idx: usize, name: &str, subtype: &str, metal: &str, salience: f32) -> Entity {
        let mut e = entity(idx, name);
        e.entity_type = EntityType::Other(subtype.to_string());
        e.salience = salience;
        e.attributes
            .insert("metal".into(), serde_json::Value::String(metal.into()));
        e
    }

    fn declared_view<'a>(entities: &'a [Entity], vocab: &'a OntologyPolicies) -> AtlasView<'a> {
        AtlasView {
            entities,
            events: &[],
            states: &[],
            relations: &[],
            claims: &[],
            questions: &[],
            configurations: &[],
            edges: &[],
            positions: &[],
            oppositions: &[],
            vocab: Some(vocab),
        }
    }

    /// The wessex-hoard count. Four of the seven catalogue coins are typed
    /// `sceatta`; `sceatta specializes coin`, so an enumeration of `coin`
    /// returns SEVEN, not three. This is the whole reason the compare walks
    /// `specializes` instead of testing equality.
    #[test]
    fn enumerate_a_declared_type_includes_its_specializations() {
        let vocab = numismatics();
        let entities = vec![
            coin(1, "Aldfrith penny, Series Y", "sceatta", "silver", 0.9),
            coin(2, "Series R sceatta", "sceatta", "silver", 0.8),
            coin(3, "Beonna penny", "coin", "silver", 0.7),
            coin(4, "Offa gold dinar", "coin", "gold", 0.6),
            coin(5, "Coenwulf mancus", "coin", "gold", 0.5),
            coin(6, "Series X sceatta", "sceatta", "silver", 0.4),
            coin(7, "Series E porcupine sceatta", "sceatta", "billon", 0.3),
            entity(8, "Offa"), // a Person; not a coin
        ];
        let plan = QueryPlan::Enumerate {
            entity_type: "coin".into(),
        };
        let result = traverse(&plan, declared_view(&entities, &vocab));
        assert!(result.hit);
        assert_eq!(result.kind, "enumerate");
        assert_eq!(
            result.entities.len(),
            7,
            "expected all seven catalogue coins"
        );
        // Salience-sorted.
        assert_eq!(
            result.entities[0].canonical_name,
            "Aldfrith penny, Series Y"
        );
        // The shipped template declares no `label`, so the headline names the
        // author's type and counts. It does not try to pluralise `coin`.
        assert_eq!(result.headline, "coin: 7 in this atlas");

        // The narrower type returns only its own.
        let sceattas = traverse(
            &QueryPlan::Enumerate {
                entity_type: "sceatta".into(),
            },
            declared_view(&entities, &vocab),
        );
        assert_eq!(sceattas.entities.len(), 4);
    }

    /// A declared `label` is what the headline calls the type. No shipped
    /// template declares one for an entity type, which is why the fixture is
    /// the shipped declaration with the facet added rather than invented.
    #[test]
    fn a_declared_label_names_the_type_in_the_headline() {
        let mut vocab = numismatics();
        vocab
            .shape
            .types
            .iter_mut()
            .find(|t| t.name == "coin")
            .expect("the template declares coin")
            .label = Some("penny".into());
        let entities = vec![coin(1, "Beonna penny", "coin", "silver", 0.7)];
        let result = traverse(
            &QueryPlan::Enumerate {
                entity_type: "coin".into(),
            },
            declared_view(&entities, &vocab),
        );
        assert_eq!(result.headline, "penny: 1 in this atlas");
    }

    /// A declared type with no atoms is a miss, not an empty hit — the caller
    /// must be able to tell "nothing of this type" from "here is the set".
    #[test]
    fn enumerate_with_no_matching_atoms_misses() {
        let vocab = numismatics();
        let entities = vec![entity(1, "Offa")];
        let result = traverse(
            &QueryPlan::Enumerate {
                entity_type: "coin".into(),
            },
            declared_view(&entities, &vocab),
        );
        assert!(!result.hit);
        assert_eq!(result.kind, "enumerate");
    }

    /// An Enumerate plan handed an atlas with NO declared vocabulary refuses.
    /// It cannot be produced by the classifier in that state, and enumerating
    /// on bare equality would be a second, weaker answer to "is this a coin".
    #[test]
    fn enumerate_without_a_vocabulary_refuses() {
        let entities = vec![coin(1, "Beonna penny", "coin", "silver", 0.7)];
        let vocab = numismatics();
        let mut view = declared_view(&entities, &vocab);
        view.vocab = None;
        let result = traverse(
            &QueryPlan::Enumerate {
                entity_type: "coin".into(),
            },
            view,
        );
        assert!(!result.hit);
        assert!(result.headline.contains("No declared ontology"));
    }

    /// The tally groups by the declared attribute and reports an absent value
    /// as `(unset)` rather than dropping the atom.
    #[test]
    fn aggregate_tallies_by_a_declared_attribute() {
        let vocab = numismatics();
        let mut untyped = coin(9, "Unmeasured fragment", "coin", "", 0.1);
        untyped.attributes.remove("metal");
        let entities = vec![
            coin(1, "Aldfrith penny", "sceatta", "silver", 0.9),
            coin(2, "Offa gold dinar", "coin", "gold", 0.6),
            coin(3, "Coenwulf mancus", "coin", "gold", 0.5),
            untyped,
        ];
        let result = traverse(
            &QueryPlan::Aggregate {
                entity_type: "coin".into(),
                over: "metal".into(),
            },
            declared_view(&entities, &vocab),
        );
        assert!(result.hit);
        assert_eq!(result.kind, "aggregate");
        assert_eq!(
            result.headline,
            "4 coin by metal — (unset): 1, gold: 2, silver: 1"
        );
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
            attributes: Default::default(),
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
            attributes: Default::default(),
            subject: None,
            id: AtomId::claim(idx),
            content: content.into(),
            discourse_act: DiscourseAct::Assert,
            epistemic_status: EpistemicStatus::Confident,
            scope: ClaimScope::Universal,
            evidence: vec![],
            attributed_to: attrib.map(AtomId::entity),
            confidence: Some(0.9),
            anchor: None,
            enrichment_depth: EnrichmentDepth::Extracted,
            quotable_excerpt: None,
            claim_kind: None,
            concession_outcome: None,
            evidence_kind: None,
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
        let entities = vec![
            entity(1, "Alyosha"),
            entity(2, "Zossima"),
            entity(3, "Fyodor"),
        ];
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
            positions: &[],
            oppositions: &[],
            vocab: None,
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
        let result = traverse(
            &plan,
            view(&ents, &states, &rels, &edges, &claims, &questions),
        );
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
        let result = traverse(
            &plan,
            view(&ents, &states, &rels, &edges, &claims, &questions),
        );
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
        let result = traverse(
            &plan,
            view(&ents, &states, &rels, &edges, &claims, &questions),
        );
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
        let result = traverse(
            &plan,
            view(&ents, &states, &rels, &edges, &claims, &questions),
        );
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
        let result = traverse(
            &plan,
            view(&ents, &states, &rels, &edges, &claims, &questions),
        );
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
                positions: &[],
                oppositions: &[],
                vocab: None,
            },
        );
        assert!(result.hit);
        assert_eq!(result.entities.len(), 8);
        // Top entity should be e1 (highest salience).
        assert_eq!(result.entities[0].canonical_name, "e1");
    }
}
