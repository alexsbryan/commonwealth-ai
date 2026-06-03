//! Phase 8 — configuration detection.
//!
//! A **Configuration** atom (spec §2.7) captures the interpretive
//! structure the work as a whole enacts through the arrangement of
//! its parts — not a single claim, but a pattern across many atoms.
//! Examples:
//!
//! - *Tractatus*: "ladder structure" — each proposition depends on
//!   the last, then the whole edifice is kicked away at the end.
//! - *Brothers Karamazov*: "three sons as embodiments of faith,
//!   reason, and sensuality" — a character configuration.
//! - *Pride and Prejudice*: "parallel courtships mirror the theme
//!   of first impressions" — a structural rhyme.
//!
//! This module owns two halves:
//!
//! 1. **`AtlasSummary`** — the compact structural synopsis that
//!    Phase 8 prompts get as input. The LLM reads this synopsis,
//!    *not* the raw corpus text — Configurations are about the
//!    relationships between atoms, not new content.
//! 2. **`parse_configurations`** — tolerant JSON deserialiser for
//!    the LLM's response, with id-stamping + atom-id validation.
//!
//! The LLM dispatch itself lives in the pipeline trait
//! (`compose_phase8_configuration` / `parse_phase8_configuration`);
//! this file stays pipeline-agnostic so future pipelines reuse the
//! summary + parser without re-implementing them.

use serde::{Deserialize, Serialize};

use super::super::atoms::{
    AtomId, ChunkRef, Claim, Configuration, Entity, Event, Question, Relation, State,
};
use super::super::edges::Edge;

/// Top-level on-disk representation. Written by `atlas-configuration`
/// even when empty so an operator sees the pass ran. `confidence` on
/// each Configuration stays in `[0.0, 1.0]` and is the LLM's own
/// self-report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigurationsOutput {
    pub schema_version: String,
    pub configurations: Vec<Configuration>,
}

impl ConfigurationsOutput {
    pub const SCHEMA_VERSION: &'static str = "2.0";

    pub fn new(configurations: Vec<Configuration>) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION.to_string(),
            configurations,
        }
    }
}

// ── Atlas summary — LLM input ───────────────────────────────

/// Compact synopsis of the atlas, shaped for a single LLM
/// dispatch. The `summarise_atlas` constructor orders + filters by
/// salience so we fit reasonably into a 24k-token context on the
/// full *Brothers Karamazov* atlas without paging.
///
/// Field shapes:
///
/// - Entity — id + canonical_name + one-line description +
///   salience. Aliases are dropped (the configuration prompt
///   doesn't need them).
/// - Relation — id + label + participant names (pre-resolved).
/// - Trajectory — entity id + state labels in section order.
/// - Top claims + questions — content + attributed-to name.
///
/// The summary itself is serialisable so a future implementation
/// can persist it as part of the run record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSummary {
    pub section_count: usize,
    pub entities: Vec<EntitySynopsis>,
    pub relations: Vec<RelationSynopsis>,
    pub trajectories: Vec<TrajectorySynopsis>,
    pub top_claims: Vec<ClaimSynopsis>,
    pub open_questions: Vec<QuestionSynopsis>,
    pub key_events: Vec<EventSynopsis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySynopsis {
    pub id: String,
    pub canonical_name: String,
    pub description: String,
    pub salience: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationSynopsis {
    pub id: String,
    pub label: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectorySynopsis {
    pub entity_id: String,
    pub canonical_name: String,
    pub state_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimSynopsis {
    pub id: String,
    pub content: String,
    pub attributed_to: Option<String>,
    pub discourse_act: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionSynopsis {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSynopsis {
    pub id: String,
    pub description: String,
    pub participants: Vec<String>,
}

/// Parameters for atlas summarisation. Defaults are calibrated for
/// a Brothers-Karamazov-scale corpus on a 24k-token prompt budget.
#[derive(Debug, Clone, Copy)]
pub struct AtlasSummaryParams {
    pub max_entities: usize,
    pub max_relations: usize,
    pub max_trajectories: usize,
    pub max_claims: usize,
    pub max_questions: usize,
    pub max_events: usize,
}

impl Default for AtlasSummaryParams {
    fn default() -> Self {
        Self {
            max_entities: 20,
            max_relations: 30,
            max_trajectories: 15,
            max_claims: 20,
            max_questions: 15,
            max_events: 20,
        }
    }
}

/// Build the atlas summary from the resolved atoms + edges. Output
/// is deterministic: entities ordered by salience desc (ties broken
/// by id), relations ordered by insertion, trajectories matched to
/// top-salience entities. Claim + event ordering uses confidence
/// then id.
pub fn summarise_atlas(
    entities: &[Entity],
    events: &[Event],
    states: &[State],
    relations: &[Relation],
    claims: &[Claim],
    questions: &[Question],
    _edges: &[Edge],
    section_count: usize,
    params: AtlasSummaryParams,
) -> AtlasSummary {
    // Entities by salience.
    let mut entity_refs: Vec<&Entity> = entities.iter().collect();
    entity_refs.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    entity_refs.truncate(params.max_entities);
    let id_to_name: std::collections::HashMap<&str, &str> = entities
        .iter()
        .map(|e| (e.id.as_str(), e.canonical_name.as_str()))
        .collect();
    let entities_out: Vec<EntitySynopsis> = entity_refs
        .iter()
        .map(|e| EntitySynopsis {
            id: e.id.as_str().to_string(),
            canonical_name: e.canonical_name.clone(),
            description: e.description.clone(),
            salience: e.salience,
        })
        .collect();

    // Relations — take up to max_relations. Participant ids
    // resolve to canonical names when known; unresolved ids pass
    // through.
    let relations_out: Vec<RelationSynopsis> = relations
        .iter()
        .take(params.max_relations)
        .map(|r| RelationSynopsis {
            id: r.id.as_str().to_string(),
            label: r.label.clone(),
            participants: r
                .participants
                .iter()
                .map(|pid| {
                    id_to_name
                        .get(pid.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| pid.as_str().to_string())
                })
                .collect(),
        })
        .collect();

    // Trajectories — one per high-salience entity, chained via
    // section order. Reuses the per-entity states grouping logic
    // that Phase 3b already applied; here we just surface the
    // labels in order.
    let mut states_by_owner: std::collections::HashMap<&str, Vec<&State>> =
        std::collections::HashMap::new();
    for s in states {
        states_by_owner
            .entry(s.entity_id.as_str())
            .or_default()
            .push(s);
    }
    for v in states_by_owner.values_mut() {
        v.sort_by(|a, b| a.section_range.start.cmp(&b.section_range.start));
    }
    let mut trajectories_out: Vec<TrajectorySynopsis> = entity_refs
        .iter()
        .filter_map(|e| {
            states_by_owner
                .get(e.id.as_str())
                .map(|ss| TrajectorySynopsis {
                    entity_id: e.id.as_str().to_string(),
                    canonical_name: e.canonical_name.clone(),
                    state_labels: ss.iter().map(|s| s.label.clone()).collect(),
                })
        })
        .collect();
    trajectories_out.truncate(params.max_trajectories);

    // Top claims by confidence then id.
    let mut claim_refs: Vec<&Claim> = claims.iter().collect();
    claim_refs.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    claim_refs.truncate(params.max_claims);
    let claims_out: Vec<ClaimSynopsis> = claim_refs
        .iter()
        .map(|c| ClaimSynopsis {
            id: c.id.as_str().to_string(),
            content: c.content.clone(),
            attributed_to: c
                .attributed_to
                .as_ref()
                .and_then(|aid| id_to_name.get(aid.as_str()).map(|s| s.to_string())),
            discourse_act: format!("{:?}", c.discourse_act).to_lowercase(),
        })
        .collect();

    // Open questions only — Phase 8 should know what the corpus
    // leaves unanswered.
    let questions_out: Vec<QuestionSynopsis> = questions
        .iter()
        .filter(|q| {
            matches!(
                q.resolution_status,
                super::super::atoms::ResolutionStatus::Open
            )
        })
        .take(params.max_questions)
        .map(|q| QuestionSynopsis {
            id: q.id.as_str().to_string(),
            content: q.content.clone(),
        })
        .collect();

    // Events — take first max_events, assumed pre-ordered by
    // section position from Phase 3a.
    let events_out: Vec<EventSynopsis> = events
        .iter()
        .take(params.max_events)
        .map(|e| EventSynopsis {
            id: e.id.as_str().to_string(),
            description: e.description.clone(),
            participants: e
                .participants
                .iter()
                .map(|pid| {
                    id_to_name
                        .get(pid.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| pid.as_str().to_string())
                })
                .collect(),
        })
        .collect();

    AtlasSummary {
        section_count,
        entities: entities_out,
        relations: relations_out,
        trajectories: trajectories_out,
        top_claims: claims_out,
        open_questions: questions_out,
        key_events: events_out,
    }
}

// ── LLM response parsing ─────────────────────────────────────

/// Shape the Phase 8 prompt is expected to return. The LLM writes
/// bare `Phase8ParseItem`s; `parse_configurations` stamps the ids
/// and the provenance metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct Phase8ParseItem {
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub constituent_atoms: Vec<String>,
    pub interpretive_note: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub evidence_chunk_ids: Vec<String>,
}

fn default_confidence() -> f32 {
    0.5
}

/// Validate + stamp ids on the LLM's parsed items. Drops any
/// reference an atom id not present in `known_atom_ids` (with a
/// debug trace) — the LLM occasionally invents plausible-looking
/// ids, and we prefer silence to a dangling reference.
///
/// **Atom id normalisation + prose extraction.** Two concessions
/// to real-world LLM output we observed on *Brothers Karamazov*:
///
/// 1. The model uses short ids (`claim-7`) where our canonical
///    form is 4-digit (`claim-0007`). We normalise both sides.
/// 2. The model writes `constituent_atoms: []` even when the
///    description + interpretive_note mention specific atoms
///    inline (`"claim-7 explicitly connects …"`, etc.). We
///    scan the prose for atom id patterns and merge them in.
///
/// Configurations come back with sequential ids `config-0001`↑.
/// `enrichment_depth` is stamped `Extracted` — Phase 8 is still
/// grounded in the atoms the LLM sees, it's just interpretive,
/// not structural.
pub fn parse_configurations(
    items: Vec<Phase8ParseItem>,
    known_atom_ids: &std::collections::HashSet<String>,
) -> Vec<Configuration> {
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.into_iter().enumerate() {
        // Start from the explicit list, normalising short ids.
        let mut collected: Vec<String> = item
            .constituent_atoms
            .into_iter()
            .filter_map(|raw| normalise_and_validate_atom_id(&raw, known_atom_ids))
            .collect();
        // Then scan the prose for additional atom references the
        // LLM mentioned but didn't lift into the array.
        for source in [&item.description, &item.interpretive_note] {
            for id in extract_atom_ids_from_prose(source, known_atom_ids) {
                if !collected.contains(&id) {
                    collected.push(id);
                }
            }
        }
        let filtered_atoms: Vec<AtomId> = collected.into_iter().map(AtomId::from_raw).collect();

        let evidence: Vec<ChunkRef> = item
            .evidence_chunk_ids
            .into_iter()
            .map(|cid| ChunkRef::new(cid, None))
            .collect();
        let confidence = item.confidence.clamp(0.0, 1.0);
        out.push(Configuration {
            id: AtomId::from_raw(format!("config-{:04}", i + 1)),
            label: item.label.trim().to_string(),
            description: item.description.trim().to_string(),
            constituent_atoms: filtered_atoms,
            evidence,
            confidence,
            interpretive_note: item.interpretive_note.trim().to_string(),
            enrichment_depth: crate::enrichment::pipeline::atlas::EnrichmentDepth::Extracted,
        });
    }
    out
}

/// Normalise an LLM-emitted atom id to the canonical
/// `<kind>-<NNNN>` form and return `Some(id)` iff the normalised
/// form appears in `known_atom_ids`. Returns `None` on an unknown
/// kind, an unparseable index, or a dangling reference — matching
/// the parser's "prefer silence to a wrong answer" policy.
fn normalise_and_validate_atom_id(
    raw: &str,
    known_atom_ids: &std::collections::HashSet<String>,
) -> Option<String> {
    let Some((kind, rest)) = raw.split_once('-') else {
        return None;
    };
    if !matches!(
        kind,
        "entity" | "event" | "state" | "relation" | "claim" | "question" | "config"
    ) {
        return None;
    }
    let Ok(idx) = rest.parse::<u32>() else {
        return None;
    };
    let normalised = format!("{kind}-{idx:04}");
    if known_atom_ids.contains(&normalised) {
        Some(normalised)
    } else {
        tracing::debug!(
            phase = "8",
            atom_id = %raw,
            normalised = %normalised,
            "configuration references unknown atom; dropping"
        );
        None
    }
}

/// Scan `text` for token-like atom references — kinds listed in
/// `normalise_and_validate_atom_id`, followed by `-` and 1+ digits
/// — and return the normalised + validated ids in first-seen order.
/// Used when the LLM mentions atoms inline without lifting them
/// into the `constituent_atoms` array.
fn extract_atom_ids_from_prose(
    text: &str,
    known_atom_ids: &std::collections::HashSet<String>,
) -> Vec<String> {
    const KINDS: &[&str] = &[
        "entity", "event", "state", "relation", "claim", "question", "config",
    ];
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for kind in KINDS {
        let mut start = 0usize;
        while let Some(rel) = lower[start..].find(kind) {
            let pos = start + rel;
            let after = pos + kind.len();
            // Require a hyphen immediately after.
            if lower.as_bytes().get(after).copied() != Some(b'-') {
                start = after;
                continue;
            }
            // Need a boundary BEFORE `kind` — whitespace, punct,
            // backtick, or start-of-string — so `miscreant-0001`
            // doesn't match `event-0001`.
            let before_ok = pos == 0
                || !lower
                    .as_bytes()
                    .get(pos - 1)
                    .map(|b| b.is_ascii_alphanumeric())
                    .unwrap_or(false);
            if !before_ok {
                start = after;
                continue;
            }
            // Read contiguous digits.
            let digits_start = after + 1;
            let digits_end = lower[digits_start..]
                .bytes()
                .take_while(|b| b.is_ascii_digit())
                .count()
                + digits_start;
            if digits_end == digits_start {
                start = after;
                continue;
            }
            let raw = &lower[pos..digits_end];
            if let Some(id) = normalise_and_validate_atom_id(raw, known_atom_ids) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            start = digits_end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::super::atoms::{
        AtomId, ChunkRef, Claim, Entity, Event, Question, Relation, ResolutionStatus,
        SectionPosition, SectionRange, State,
    };
    use super::*;
    use crate::enrichment::pipeline::atlas::{
        ClaimScope, DiscourseAct, EnrichmentDepth, EntityType, EpistemicStatus, EventType,
        QuestionType, RelationType, StateType,
    };

    fn entity(idx: usize, name: &str, salience: f32) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: format!("Character {name}"),
            salience,
            enrichment_depth: EnrichmentDepth::Extracted,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            concept_kind: None,
        }
    }

    fn relation(idx: usize, label: &str, participants: Vec<AtomId>) -> Relation {
        Relation {
            id: AtomId::relation(idx),
            label: label.into(),
            participants,
            relation_type: RelationType::Other("unclassified".into()),
            evidence: vec![],
            section_range: SectionRange::point("sec_0001"),
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

    #[allow(dead_code)]
    fn claim(idx: usize, content: &str, attrib: Option<usize>) -> Claim {
        Claim {
            id: AtomId::claim(idx),
            content: content.into(),
            discourse_act: DiscourseAct::Argue,
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

    fn question(idx: usize, content: &str, open: bool) -> Question {
        Question {
            id: AtomId::from_raw(format!("question-{idx:04}")),
            content: content.into(),
            question_type: QuestionType::Other("unknown".into()),
            addressed_by: vec![],
            raised_at: vec![ChunkRef::new("sec_0001", None)],
            resolution_status: if open {
                ResolutionStatus::Open
            } else {
                ResolutionStatus::Resolved {
                    claim_id: AtomId::claim(1),
                }
            },
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn event(idx: usize, description: &str, participants: Vec<AtomId>) -> Event {
        Event {
            id: AtomId::event(idx),
            description: description.into(),
            event_type: EventType::Other("unspecified".into()),
            participants,
            evidence: vec![],
            section_position: SectionPosition::section("sec_0001"),
            causal_antecedents: vec![],
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    #[test]
    fn summary_orders_entities_by_salience_descending() {
        let entities = vec![
            entity(1, "Alyosha", 0.3),
            entity(2, "Fyodor", 1.0),
            entity(3, "Dmitri", 0.8),
        ];
        let summary = summarise_atlas(
            &entities,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            3,
            AtlasSummaryParams::default(),
        );
        let order: Vec<&str> = summary
            .entities
            .iter()
            .map(|e| e.canonical_name.as_str())
            .collect();
        assert_eq!(order, vec!["Fyodor", "Dmitri", "Alyosha"]);
    }

    #[test]
    fn summary_resolves_participant_ids_to_canonical_names() {
        let entities = vec![entity(1, "Alyosha", 0.9), entity(2, "Zossima", 0.7)];
        let relations = vec![relation(
            1,
            "Mentor-mentee",
            vec![AtomId::entity(1), AtomId::entity(2)],
        )];
        let summary = summarise_atlas(
            &entities,
            &[],
            &[],
            &relations,
            &[],
            &[],
            &[],
            1,
            AtlasSummaryParams::default(),
        );
        assert_eq!(summary.relations.len(), 1);
        assert_eq!(
            summary.relations[0].participants,
            vec!["Alyosha", "Zossima"]
        );
    }

    #[test]
    fn summary_trajectories_chain_states_in_section_order() {
        let entities = vec![entity(1, "Alyosha", 1.0)];
        // Deliberately out-of-order input — the summariser must
        // sort by section_range.start before emitting.
        let states = vec![
            state(2, 1, "second state", "sec_0003"),
            state(1, 1, "first state", "sec_0001"),
        ];
        let summary = summarise_atlas(
            &entities,
            &[],
            &states,
            &[],
            &[],
            &[],
            &[],
            3,
            AtlasSummaryParams::default(),
        );
        assert_eq!(summary.trajectories.len(), 1);
        assert_eq!(
            summary.trajectories[0].state_labels,
            vec!["first state", "second state"]
        );
    }

    #[test]
    fn summary_keeps_only_open_questions() {
        let questions = vec![
            question(1, "Open one", true),
            question(2, "Closed one", false),
        ];
        let summary = summarise_atlas(
            &[],
            &[],
            &[],
            &[],
            &[],
            &questions,
            &[],
            1,
            AtlasSummaryParams::default(),
        );
        assert_eq!(summary.open_questions.len(), 1);
        assert!(summary.open_questions[0].content.contains("Open"));
    }

    #[test]
    fn parse_configurations_stamps_sequential_ids_and_clamps_confidence() {
        let items = vec![
            Phase8ParseItem {
                label: "Three-sons configuration".into(),
                description: "Alyosha / Ivan / Dmitri as faith / reason / sensuality".into(),
                constituent_atoms: vec!["entity-0001".into(), "entity-0002".into()],
                interpretive_note: "Alternative reading: psychological triad".into(),
                confidence: 1.3, // over-clamp → 1.0
                evidence_chunk_ids: vec![],
            },
            Phase8ParseItem {
                label: "Parallel marriages".into(),
                description: "First and second marriages mirror each other".into(),
                constituent_atoms: vec!["entity-0002".into()],
                interpretive_note: "Could also read as decline narrative".into(),
                confidence: 0.7,
                evidence_chunk_ids: vec!["sec_0001".into()],
            },
        ];
        let known: std::collections::HashSet<String> = ["entity-0001", "entity-0002"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let configs = parse_configurations(items, &known);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].id.as_str(), "config-0001");
        assert_eq!(configs[1].id.as_str(), "config-0002");
        assert_eq!(configs[0].confidence, 1.0); // clamped
        assert_eq!(configs[0].constituent_atoms.len(), 2);
        assert_eq!(configs[1].evidence.len(), 1);
    }

    #[test]
    fn parse_configurations_normalises_short_ids_to_four_digits() {
        // LLM emits `claim-7` where the canonical form is
        // `claim-0007`. The parser normalises AND validates
        // against the known set.
        let items = vec![Phase8ParseItem {
            label: "x".into(),
            description: "".into(),
            constituent_atoms: vec!["claim-7".into(), "entity-42".into()],
            interpretive_note: "".into(),
            confidence: 0.8,
            evidence_chunk_ids: vec![],
        }];
        let known: std::collections::HashSet<String> = ["claim-0007", "entity-0042"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let configs = parse_configurations(items, &known);
        let ids: Vec<&str> = configs[0]
            .constituent_atoms
            .iter()
            .map(|a| a.as_str())
            .collect();
        assert_eq!(ids, vec!["claim-0007", "entity-0042"]);
    }

    #[test]
    fn parse_configurations_extracts_atom_refs_from_description_prose() {
        // Observed on Brothers Karamazov: the LLM writes the
        // constituent atoms inline in the description ("claim-7
        // explicitly connects manipulation…") while leaving the
        // array empty. The prose-scan fallback recovers them.
        let items = vec![Phase8ParseItem {
            label: "Financial Abandonment".into(),
            description: "The father's financial manipulation (claim-7) creates the conditions \
                          for convergence at Zossima. event-6 shows Dmitri discovering that \
                          nothing remains."
                .into(),
            constituent_atoms: vec![],
            interpretive_note: "I weight this configuration because relation-4 shows the \
                                servant-child bond amid financial vacancy."
                .into(),
            confidence: 0.8,
            evidence_chunk_ids: vec![],
        }];
        let known: std::collections::HashSet<String> =
            ["claim-0007", "event-0006", "relation-0004"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        let configs = parse_configurations(items, &known);
        let ids: std::collections::HashSet<&str> = configs[0]
            .constituent_atoms
            .iter()
            .map(|a| a.as_str())
            .collect();
        assert!(ids.contains("claim-0007"));
        assert!(ids.contains("event-0006"));
        assert!(ids.contains("relation-0004"));
    }

    #[test]
    fn parse_configurations_prose_scan_respects_word_boundary() {
        // Don't match atom-id-looking substrings embedded in
        // other words. "miscreant-0001" is NOT an atom reference.
        let items = vec![Phase8ParseItem {
            label: "".into(),
            description: "The miscreant-0001 wallpaper was green.".into(),
            constituent_atoms: vec![],
            interpretive_note: "".into(),
            confidence: 0.5,
            evidence_chunk_ids: vec![],
        }];
        let known: std::collections::HashSet<String> =
            std::iter::once("event-0001".to_string()).collect();
        let configs = parse_configurations(items, &known);
        assert!(
            configs[0].constituent_atoms.is_empty(),
            "prose scan must not lift atom-id-looking suffixes out of unrelated words"
        );
    }

    #[test]
    fn parse_configurations_drops_unknown_atom_ids_silently() {
        let items = vec![Phase8ParseItem {
            label: "Test".into(),
            description: "".into(),
            constituent_atoms: vec![
                "entity-0001".into(), // known
                "entity-9999".into(), // unknown — must drop
            ],
            interpretive_note: "".into(),
            confidence: 0.5,
            evidence_chunk_ids: vec![],
        }];
        let known: std::collections::HashSet<String> =
            std::iter::once("entity-0001".to_string()).collect();
        let configs = parse_configurations(items, &known);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].constituent_atoms.len(), 1);
        assert_eq!(configs[0].constituent_atoms[0].as_str(), "entity-0001");
    }

    #[test]
    fn summary_suppresses_entities_beyond_budget_even_at_matched_salience() {
        // Budget of 2 with 4 equal-salience entities — stable tie-
        // break is id ascending so we know which 2 survive.
        let entities = vec![
            entity(1, "A", 0.5),
            entity(2, "B", 0.5),
            entity(3, "C", 0.5),
            entity(4, "D", 0.5),
        ];
        let mut params = AtlasSummaryParams::default();
        params.max_entities = 2;
        let summary = summarise_atlas(&entities, &[], &[], &[], &[], &[], &[], 1, params);
        let names: Vec<&str> = summary
            .entities
            .iter()
            .map(|e| e.canonical_name.as_str())
            .collect();
        assert_eq!(names, vec!["A", "B"]);
    }

    #[test]
    fn summary_resolves_event_participant_ids_to_names() {
        // Phase 8's interpretive pass cares who was *in* a key
        // event; mirror the relation-resolution path so participant
        // ids become canonical names before the LLM sees them.
        let entities = vec![entity(1, "Alyosha", 1.0)];
        let events = vec![event(
            1,
            "Alyosha kneels at the elder's feet",
            vec![AtomId::entity(1), AtomId::from_raw("entity-0999")],
        )];
        let summary = summarise_atlas(
            &entities,
            &events,
            &[],
            &[],
            &[],
            &[],
            &[],
            1,
            AtlasSummaryParams::default(),
        );
        assert_eq!(summary.key_events.len(), 1);
        assert_eq!(
            summary.key_events[0].participants,
            vec!["Alyosha", "entity-0999"]
        );
    }
}
