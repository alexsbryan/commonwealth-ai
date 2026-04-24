//! Phase 3a atom resolution — merge entity + event sketches across
//! sections into canonical atoms with assigned IDs.
//!
//! The resolution rules are pinned in the rollout plan (Phase A Step
//! 3a). Summary:
//!
//! **Entity merge** — two sketches merge when ANY of:
//!
//! 1. One `canonical_name` appears in the other's `aliases` list.
//! 2. Name-form Levenshtein ≤ 2 AND description embedding cosine ≥
//!    0.90 (both required).
//! 3. Name tokens overlap on ≥ 2 shared tokens (Russian patronymic:
//!    `"Alexei Fyodorovich Karamazov"` and `"Alexei Fyodorovich"`).
//!
//! Search is bounded to a 5-section lookback window; longer-range
//! matches require alias evidence (rule 1). Prevents quadratic
//! blow-up on 96-chapter corpora.
//!
//! **Event dedupe** — within a ±2-section window, sketches merge when:
//!
//! - Description cosine ≥ 0.93, OR
//! - Description cosine ≥ 0.88 AND ≥ 1 shared participant.
//!
//! Events across non-adjacent sections are kept distinct — narrative
//! repetition (remembered events) is a feature to capture, not noise
//! to deduplicate.
//!
//! The resolver also emits one `Involves` edge per participant that
//! an event sketch lists, after resolving participant names to
//! canonical entity IDs. Orphan participant names (never introduced
//! as an entity) are elided but logged via tracing.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::enrichment::pipeline::atlas::{
    EntitySketch, EventSketch, EventType, SectionExtraction,
};
use crate::error::Result;
use crate::types::EmbedFn;

use super::atoms::{AtomId, ChunkRef, Entity, Event, SectionPosition};
use super::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};

// ── Tuning constants ────────────────────────────────────────

/// Number of previous sections we scan when looking for an existing
/// entity that a new sketch might refer to. Cross-window matches
/// require alias evidence (rule 1). Keeps resolution cost
/// manageable on novel-length corpora.
pub const ENTITY_WINDOW_SECTIONS: usize = 5;

/// Lower description-similarity threshold for entity merging. Paired
/// with Levenshtein ≤ 2; both conditions must hold. Originally 0.85;
/// raised to 0.90 to guard against Ivan/Ilya false-merges; relaxed
/// back to 0.85 after the Lev guard proved sufficient on its own
/// (Ivan→Ilya is Lev 3, blocked before cosine is evaluated). The
/// change recovers pairs like `Dmitri Karamaов` (with Cyrillic
/// chars) ↔ `Dmitri Karamazov` where the transliterated
/// Levenshtein is 1 but cosine lands in the 0.85-0.90 band.
pub const ENTITY_MERGE_COSINE: f32 = 0.85;

/// Minimum length, after folding, for a name to participate in the
/// substring-match merge rule. 5 chars keeps short ambiguous names
/// (`Ivan`, `Ilya`, `Anna`, `Lise`) from triggering substring
/// merges they shouldn't — `Ivan` being a substring of `Ivanovich`
/// is linguistically coincidental, not a merge signal.
pub const ENTITY_MERGE_SUBSTRING_MIN_LEN: usize = 5;

/// Levenshtein cap for name-form rule. Kept tight; two-character
/// edits catch transliteration variants without bridging distinct
/// names.
pub const ENTITY_MERGE_LEVENSHTEIN: usize = 2;

/// Minimum shared tokens for the patronymic rule. Token = whitespace-
/// split, length ≥ 3, case-folded.
pub const ENTITY_MERGE_MIN_SHARED_TOKENS: usize = 2;

/// Description cosine threshold for the single-long-token rule
/// (Phase 3a Rule 3.5). This rule fires when two sketches share
/// only *one* long token but their descriptions are tightly similar
/// — the semantic signal substitutes for the syntactic weight of a
/// second shared token. 0.92 is deliberately higher than rule 2's
/// 0.85 because the syntactic constraint is weaker.
///
/// Motivating case from the *Brothers Karamazov* smoke run:
/// `Fyódor Pavlóvič Karámazòv` and `Fyódor Kárazóv` share only the
/// first token `fyodor` after fold + Lev-1 — `karazov ↔ karamazov`
/// is Lev 2, above the shared_token_overlap fuzzy cap. Both describe
/// the same patriarch, so cosine is high; merge fires.
pub const ENTITY_MERGE_SINGLE_TOKEN_COSINE: f32 = 0.92;

/// Minimum length for a token to count toward the shared-token rule.
/// Drops short patronymic particles ("de", "of", "the") that would
/// otherwise spuriously link unrelated names.
pub const ENTITY_MERGE_TOKEN_MIN_LEN: usize = 3;

/// Dedupe window for events. Events across non-adjacent sections
/// stay distinct.
pub const EVENT_WINDOW_SECTIONS: usize = 2;

/// High-similarity dedupe threshold for events. Above this, dedupe
/// even without shared participants.
pub const EVENT_DEDUPE_COSINE_STRICT: f32 = 0.93;

/// Permissive dedupe threshold for events, paired with ≥ 1 shared
/// participant. Catches narration of the same event from two POVs.
pub const EVENT_DEDUPE_COSINE_WITH_PARTICIPANT: f32 = 0.88;

// ── Public entry point ──────────────────────────────────────

/// Resolve entity + event sketches from an ordered list of section
/// extractions into canonical atoms + `Involves` edges.
///
/// `sections` must be in corpus order (the first section first) —
/// the resolver uses ordinal position to compute salience and to
/// bound the cross-section search window.
pub async fn resolve_entities_and_events(
    sections: &[SectionExtraction],
    embed_fn: &EmbedFn,
) -> Result<ResolutionOutput> {
    let entity_result = resolve_entities(sections, embed_fn).await?;
    // Token-inverted index for fuzzy participant lookup — covers
    // LLM-typo names like `Alyshka` / `Alysha` / `Adeladа Miюsova`
    // that share a long token with the canonical entity but do not
    // match exactly.
    let token_index = build_token_index(&entity_result.entities);
    let event_result = resolve_events(
        sections,
        embed_fn,
        &entity_result.name_index,
        &token_index,
    )
    .await?;
    Ok(ResolutionOutput {
        entities: entity_result.entities,
        events: event_result.events,
        edges: event_result.involves_edges,
        failures: event_result.failures,
    })
}

/// Bundle returned by [`resolve_entities_and_events`].
#[derive(Debug, Clone)]
pub struct ResolutionOutput {
    pub entities: Vec<Entity>,
    pub events: Vec<Event>,
    pub edges: Vec<Edge>,
    /// Structured drops — event participants whose name didn't
    /// resolve to any Entity atom. Previously logged at `debug!` and
    /// lost; now surfaced so `sovereign enrich errors` can group
    /// these and hint at the remediation (usually: the entity name
    /// missed Phase 1 extraction or the seed list diverged). Empty
    /// for a clean run.
    pub failures: Vec<crate::enrichment::pipeline::types::PhaseFailure>,
}

/// Bundle returned by [`resolve_step_3b`].
#[derive(Debug, Clone)]
pub struct Step3bOutput {
    pub states: Vec<super::atoms::State>,
    pub relations: Vec<super::atoms::Relation>,
    pub claims: Vec<super::atoms::Claim>,
    pub questions: Vec<super::atoms::Question>,
    /// New edges this pass emits — Transition chains, Grounds edges
    /// on claims + states, Involves edges on claims / states /
    /// relations / questions. Does NOT include the Step 3a Involves
    /// edges — caller merges with pre-existing edges.
    pub edges: Vec<Edge>,
    /// Per-entity and per-relation state sequences, keyed by the
    /// entity/relation AtomId. Written to `atlas/trajectories.json`.
    pub trajectories: std::collections::BTreeMap<String, Trajectory>,
    /// Structured drops from the deterministic resolver: entity-state
    /// sketches whose entity name didn't resolve, relation sketches
    /// with < 2 resolvable participants, claims whose `attributed_to`
    /// didn't match. These used to be silent `debug!` logs; now the
    /// aggregator surfaces them grouped by kind with a remediation
    /// hint per group.
    pub failures: Vec<crate::enrichment::pipeline::types::PhaseFailure>,
}

/// A single per-entity (or per-relation) state sequence. Mirrors the
/// shape in spec §6.4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub canonical_name: String,
    pub atom_type: String, // "Entity" or "Relation"
    pub states: Vec<TrajectoryState>,
    pub transitions: Vec<TrajectoryTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryState {
    pub state_id: String,
    pub label: String,
    pub section_range: super::atoms::SectionRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryTransition {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<String>,
}

// ── Entity resolution ───────────────────────────────────────

struct EntityResolution {
    entities: Vec<Entity>,
    /// Case-folded alias/canonical-name → entity AtomId index.
    /// Event resolution reads this when wiring Involves edges.
    name_index: HashMap<String, AtomId>,
}

async fn resolve_entities(
    sections: &[SectionExtraction],
    embed_fn: &EmbedFn,
) -> Result<EntityResolution> {
    let mut entities: Vec<Entity> = Vec::new();
    let mut descriptions: Vec<Vec<f32>> = Vec::new();
    let mut section_refs: Vec<usize> = Vec::new();
    let mut name_index: HashMap<String, AtomId> = HashMap::new();
    // Track which section each entity was introduced in, so we can
    // enforce the 5-section lookback rule.
    let mut first_section_ordinal: Vec<usize> = Vec::new();

    for (section_ordinal, section) in sections.iter().enumerate() {
        // Each section contributes one "touch" per entity mentioned —
        // entities_introduced plus entities_developed.
        let mut touched_this_section: Vec<usize> = Vec::new();

        // Collect introduced sketches plus synthesized sketches from
        // the `entities_developed` name references into a single list
        // so the loop can borrow uniformly.
        let mut section_sketches: Vec<EntitySketch> = section.entities_introduced.clone();
        section_sketches.extend(entity_sketches_from_developed(section));

        for sketch in &section_sketches {
            let candidate_emb = if sketch.description.trim().is_empty() {
                // No description → rule 2 can't apply. We still take
                // rules 1 and 3, which don't require embeddings.
                Vec::new()
            } else {
                (embed_fn)(&sketch.description).await?
            };

            match find_merge_target(
                sketch,
                &entities,
                &descriptions,
                &first_section_ordinal,
                &name_index,
                &candidate_emb,
                section_ordinal,
            ) {
                Some(existing_idx) => {
                    merge_into_existing(
                        &mut entities[existing_idx],
                        sketch,
                        section,
                        &mut name_index,
                    );
                    if !touched_this_section.contains(&existing_idx) {
                        section_refs[existing_idx] += 1;
                        touched_this_section.push(existing_idx);
                    }
                }
                None => {
                    let new_id = AtomId::entity(entities.len() + 1);
                    let new_idx = entities.len();
                    let entity = Entity {
                        id: new_id.clone(),
                        canonical_name: sketch.canonical_name.trim().to_string(),
                        aliases: dedup_aliases(&sketch.aliases, &sketch.canonical_name),
                        entity_type: sketch.entity_type.clone(),
                        first_appearance: ChunkRef::new(
                            section.section_id.clone(),
                            if sketch.anchor.is_empty() {
                                None
                            } else {
                                Some(sketch.anchor.clone())
                            },
                        ),
                        description: sketch.description.trim().to_string(),
                        // Salience is filled in after all sections
                        // are processed.
                        salience: 0.0,
                        enrichment_depth: section.enrichment_depth,
                    };
                    entities.push(entity);
                    descriptions.push(candidate_emb);
                    section_refs.push(1);
                    first_section_ordinal.push(section_ordinal);
                    touched_this_section.push(new_idx);
                    // Prime the name index with every form the entity
                    // answers to.
                    name_index.insert(fold(&sketch.canonical_name), new_id.clone());
                    for alias in &sketch.aliases {
                        name_index.insert(fold(alias), new_id.clone());
                    }
                }
            }
        }
    }

    // Normalise salience: reference count ÷ max count. A one-mention
    // entity lands at 1/max; the most-mentioned entity at 1.0.
    // Constant salience of 0 when nothing was extracted, which is
    // only possible when sections is empty.
    let max_refs = *section_refs.iter().max().unwrap_or(&1).max(&1);
    for (e, refs) in entities.iter_mut().zip(section_refs.iter()) {
        e.salience = *refs as f32 / max_refs as f32;
    }

    Ok(EntityResolution { entities, name_index })
}

/// Small adapter: the EntityStateSketch's `entity_name` field is
/// effectively a reference to an entity, not a full entity
/// introduction. We treat those mentions as touches on the entity
/// (for salience) but not as new sketches themselves — the original
/// sketch lives in `entities_introduced` somewhere upstream. If we
/// encounter a name in `entities_developed` that was never
/// introduced, we fabricate a bare entity sketch so downstream
/// traversal has something to point at.
fn entity_sketches_from_developed<'a>(
    section: &'a SectionExtraction,
) -> impl Iterator<Item = EntitySketch> + 'a {
    use crate::enrichment::pipeline::atlas::EntityType;
    section.entities_developed.iter().map(|s| EntitySketch {
        canonical_name: s.entity_name.clone(),
        aliases: Vec::new(),
        // Unknown at this point — the entity-state sketch doesn't
        // carry a type. Phase 3a uses `Other("unspecified")` so the
        // atom is still typed; a later pass can promote it when
        // more evidence arrives.
        entity_type: EntityType::Other("unspecified".into()),
        description: String::new(),
        anchor: s.anchor.clone(),
    })
}

fn find_merge_target(
    sketch: &EntitySketch,
    entities: &[Entity],
    descriptions: &[Vec<f32>],
    first_section_ordinal: &[usize],
    name_index: &HashMap<String, AtomId>,
    candidate_emb: &[f32],
    current_section_ordinal: usize,
) -> Option<usize> {
    let folded_name = fold(&sketch.canonical_name);

    // Rule 1: alias match via the name index — cheap and strongest
    // signal. Works across any section distance.
    if let Some(id) = name_index.get(&folded_name) {
        return entities.iter().position(|e| e.id == *id);
    }
    for alias in &sketch.aliases {
        if let Some(id) = name_index.get(&fold(alias)) {
            return entities.iter().position(|e| e.id == *id);
        }
    }

    // Rule 4: substring match across any section distance. Triggers
    // when either folded name contains the other AND the shorter
    // string is ≥ 5 chars. Covers the common "Alyosha" ↔ "Alyosha
    // Karamazov" and "Zossima" ↔ "Father Zossima" fragmentation from
    // the Step 3b smoke test while keeping the 4-char Ivan/Ilya guard
    // intact. Unbounded distance because substring is a strong
    // signal — rule 1 is the only other unbounded rule, and an
    // alias-less follow-up sketch deserves the same courtesy.
    if let Some(idx) = find_substring_match(&folded_name, entities) {
        return Some(idx);
    }

    // Rules 2 and 3 require scanning; bound the scan to a 5-section
    // lookback window. Start from the most recent entities so we
    // match the nearest section first (stable when ties happen).
    let lookback_ordinal = current_section_ordinal
        .saturating_sub(ENTITY_WINDOW_SECTIONS);
    for (idx, existing) in entities.iter().enumerate().rev() {
        if first_section_ordinal[idx] < lookback_ordinal {
            // Past the window — rule 2/3 doesn't apply.
            continue;
        }

        // Rule 3: shared-token overlap (patronymic case), gated on
        // first-token match. The first token of a Slavic-style name
        // is the individuating given name; two siblings like
        // Alexei and Dmitri Fyodorovich Karamazov share patronymic
        // + surname (2 exact tokens) but MUST NOT merge. Requiring
        // the first token to match (exactly or within Lev 1 to
        // absorb drift like `Alexey` ↔ `Alexei`) preserves rule 3's
        // original intent — catching patronymic-abbreviation like
        // `Alexei Fyodorovich Karamazov` ↔ `Alexei Fyodorovich` —
        // without false-merging siblings.
        if first_token_matches(&sketch.canonical_name, &existing.canonical_name)
            && shared_token_overlap(&sketch.canonical_name, &existing.canonical_name)
                >= ENTITY_MERGE_MIN_SHARED_TOKENS
        {
            return Some(idx);
        }
        for alias in &existing.aliases {
            if first_token_matches(&sketch.canonical_name, alias)
                && shared_token_overlap(&sketch.canonical_name, alias)
                    >= ENTITY_MERGE_MIN_SHARED_TOKENS
            {
                return Some(idx);
            }
        }

        let has_both_embeddings =
            !candidate_emb.is_empty() && !descriptions[idx].is_empty();
        let one_side_empty =
            candidate_emb.is_empty() ^ descriptions[idx].is_empty();

        // Rule 2: Levenshtein ≤ 2 on whole folded name AND
        // description cosine ≥ ENTITY_MERGE_COSINE. Tight
        // syntactic + moderate semantic. Only fires when both
        // sides have embeddings.
        if has_both_embeddings
            && levenshtein(&folded_name, &fold(&existing.canonical_name))
                <= ENTITY_MERGE_LEVENSHTEIN
        {
            let cosine = cosine_similarity(candidate_emb, &descriptions[idx]);
            if cosine >= ENTITY_MERGE_COSINE {
                return Some(idx);
            }
        }

        // Rule 3.5: single-long-token + first-token match. Catches
        // the "Fyódor Pavlóvič Karámazòv ↔ Fyódor Kárazóv" drift
        // class — only `fyodor` shares exactly across both names
        // (karazov ↔ karamazov is Lev 2, above rule 3's fuzzy cap).
        //
        // Two paths depending on description coverage:
        //
        // - **Strict path** (both sides have descriptions). Require
        //   cosine ≥ ENTITY_MERGE_SINGLE_TOKEN_COSINE (0.92). The
        //   semantic signal substitutes for the weaker syntactic
        //   constraint; higher than rule 2's threshold because the
        //   syntactic side is weaker.
        // - **Sparse path** (exactly one side has no description).
        //   Merge without cosine. An entity with no description is
        //   a sparse ghost reference the earlier Phase 1 pass
        //   couldn't anchor — merging it into a well-described
        //   entity sharing the first name + a long token is
        //   almost always correct. The observed Fyodor drift is
        //   this case (entity-0007 has no description because the
        //   Phase 1 sketch that produced it was a single
        //   mid-prose reference).
        //
        // first_token_matches keeps both paths safe against
        // sibling collapse (Alexei vs Dmitri don't merge via a
        // shared `karamazov` token alone).
        if first_token_matches(&sketch.canonical_name, &existing.canonical_name)
            && shared_long_token_count(
                &sketch.canonical_name,
                &existing.canonical_name,
            ) >= 1
        {
            if has_both_embeddings {
                let cosine = cosine_similarity(candidate_emb, &descriptions[idx]);
                if cosine >= ENTITY_MERGE_SINGLE_TOKEN_COSINE {
                    return Some(idx);
                }
            } else if one_side_empty {
                return Some(idx);
            }
            // Both-sides empty: no semantic signal, no merge.
        }
    }

    None
}

fn merge_into_existing(
    entity: &mut Entity,
    sketch: &EntitySketch,
    _section: &SectionExtraction,
    name_index: &mut HashMap<String, AtomId>,
) {
    // Union aliases — canonical_name from the new sketch + every
    // alias from the sketch, minus the existing canonical_name.
    let canon = sketch.canonical_name.trim().to_string();
    if !canon.is_empty()
        && !canon.eq_ignore_ascii_case(&entity.canonical_name)
        && !entity.aliases.iter().any(|a| a.eq_ignore_ascii_case(&canon))
    {
        entity.aliases.push(canon);
    }
    for alias in &sketch.aliases {
        let a = alias.trim().to_string();
        if a.is_empty()
            || a.eq_ignore_ascii_case(&entity.canonical_name)
            || entity.aliases.iter().any(|e| e.eq_ignore_ascii_case(&a))
        {
            continue;
        }
        entity.aliases.push(a);
    }
    // Register every form of the name so future lookups find this
    // entity. `insert` replaces on collision, which is fine —
    // collisions only happen when the same alias appears in two
    // sketches that already merged.
    name_index.insert(fold(&entity.canonical_name), entity.id.clone());
    for alias in &entity.aliases {
        name_index.insert(fold(alias), entity.id.clone());
    }
    // Keep the longer description — spec §2.1 says the description
    // is a routing aid; a fuller one is strictly more useful.
    if sketch.description.trim().len() > entity.description.len() {
        entity.description = sketch.description.trim().to_string();
    }
}

fn dedup_aliases(aliases: &[String], canonical: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(aliases.len());
    for a in aliases {
        let t = a.trim().to_string();
        if t.is_empty() || t.eq_ignore_ascii_case(canonical) {
            continue;
        }
        if out.iter().any(|e: &String| e.eq_ignore_ascii_case(&t)) {
            continue;
        }
        out.push(t);
    }
    out
}

// ── Event resolution ────────────────────────────────────────

struct EventResolution {
    events: Vec<Event>,
    involves_edges: Vec<Edge>,
    failures: Vec<crate::enrichment::pipeline::types::PhaseFailure>,
}

async fn resolve_events(
    sections: &[SectionExtraction],
    embed_fn: &EmbedFn,
    name_index: &HashMap<String, AtomId>,
    token_index: &HashMap<String, Vec<AtomId>>,
) -> Result<EventResolution> {
    use crate::enrichment::pipeline::types::{
        PhaseFailure, PhaseFailureKind, PipelinePhase,
    };

    let mut events: Vec<Event> = Vec::new();
    let mut descriptions: Vec<Vec<f32>> = Vec::new();
    let mut section_ordinal_of_event: Vec<usize> = Vec::new();
    let mut involves_edges: Vec<Edge> = Vec::new();
    let mut failures: Vec<PhaseFailure> = Vec::new();

    for (section_ordinal, section) in sections.iter().enumerate() {
        for (sketch_index, sketch) in section.events.iter().enumerate() {
            let candidate_emb = if sketch.description.trim().is_empty() {
                Vec::new()
            } else {
                (embed_fn)(&sketch.description).await?
            };
            let (participant_ids, unresolved) =
                resolve_participant_ids(&sketch.participants, name_index, token_index);
            // Surface dropped participants as structured failures so
            // the aggregator can show the operator which entity names
            // Phase 1 missed. Each unresolved participant is its own
            // record — grouping happens in the aggregator, not here.
            for name in &unresolved {
                failures.push(PhaseFailure {
                    phase: PipelinePhase::Questions, // Phase 3a rides on the Questions cache
                    subject: format!(
                        "sketch:event:{}#{}",
                        section.section_id, sketch_index
                    ),
                    kind: PhaseFailureKind::UnresolvedEntityName,
                    reason: format!(
                        "event participant `{}` did not resolve to any Entity atom \
                         (event description: `{}`)",
                        name,
                        sketch.description.trim()
                    ),
                    raw_response_head: None,
                });
            }

            let merge_target = find_event_merge_target(
                sketch,
                &candidate_emb,
                &participant_ids,
                &events,
                &descriptions,
                &section_ordinal_of_event,
                section_ordinal,
            );

            let event_id = match merge_target {
                Some(existing_idx) => {
                    // Merge: append evidence, union participants. The
                    // section_position of the original sketch stays
                    // (events happen at a point; a restatement adds
                    // evidence, not a new position).
                    let existing = &mut events[existing_idx];
                    existing.evidence.push(ChunkRef::new(
                        section.section_id.clone(),
                        if sketch.anchor.is_empty() {
                            None
                        } else {
                            Some(sketch.anchor.clone())
                        },
                    ));
                    for pid in &participant_ids {
                        if !existing.participants.contains(pid) {
                            existing.participants.push(pid.clone());
                        }
                    }
                    existing.id.clone()
                }
                None => {
                    let new_id = AtomId::event(events.len() + 1);
                    let ev = Event {
                        id: new_id.clone(),
                        description: sketch.description.trim().to_string(),
                        // Event-type classification is deferred to
                        // Phase 5. Step 3a tags all events as
                        // unspecified so the schema still types them.
                        event_type: EventType::Other("unspecified".into()),
                        participants: participant_ids.clone(),
                        evidence: vec![ChunkRef::new(
                            section.section_id.clone(),
                            if sketch.anchor.is_empty() {
                                None
                            } else {
                                Some(sketch.anchor.clone())
                            },
                        )],
                        section_position: SectionPosition::section(section.section_id.clone()),
                        causal_antecedents: Vec::new(),
                        enrichment_depth: section.enrichment_depth,
                    };
                    events.push(ev);
                    descriptions.push(candidate_emb);
                    section_ordinal_of_event.push(section_ordinal);
                    new_id
                }
            };

            // Emit Involves edges for this sketch's participants
            // regardless of whether we merged or created — new
            // participants on a merged event need edges too. Skip
            // duplicates.
            for pid in &participant_ids {
                let already_edged = involves_edges.iter().any(|e| {
                    e.edge_type == EdgeType::Involves
                        && e.source == event_id
                        && e.target == *pid
                });
                if already_edged {
                    continue;
                }
                involves_edges.push(Edge {
                    id: EdgeId::new(involves_edges.len() + 1),
                    edge_type: EdgeType::Involves,
                    source: event_id.clone(),
                    target: pid.clone(),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
        }
    }

    Ok(EventResolution { events, involves_edges, failures })
}

/// Resolve a list of participant names to Entity atom ids.
///
/// Returns `(resolved, unresolved)` — the unresolved names are
/// returned rather than silently dropped so the caller can decide
/// how to surface them (previously these all went to `debug!` logs
/// that no one read at runtime; the aggregator now turns them into
/// structured failures).
fn resolve_participant_ids(
    names: &[String],
    name_index: &HashMap<String, AtomId>,
    token_index: &HashMap<String, Vec<AtomId>>,
) -> (Vec<AtomId>, Vec<String>) {
    let mut resolved = Vec::with_capacity(names.len());
    let mut unresolved = Vec::new();
    for name in names {
        match resolve_entity_id_fuzzy(name, name_index, token_index) {
            Some(id) => {
                if !resolved.contains(&id) {
                    resolved.push(id);
                }
            }
            None => {
                unresolved.push(name.clone());
            }
        }
    }
    (resolved, unresolved)
}

fn find_event_merge_target(
    sketch: &EventSketch,
    candidate_emb: &[f32],
    candidate_participants: &[AtomId],
    existing: &[Event],
    descriptions: &[Vec<f32>],
    section_ordinal_of_event: &[usize],
    current_section_ordinal: usize,
) -> Option<usize> {
    if candidate_emb.is_empty() {
        // Can't dedupe without a description embedding; keep distinct.
        return None;
    }
    let lookback = current_section_ordinal.saturating_sub(EVENT_WINDOW_SECTIONS);
    for (idx, event) in existing.iter().enumerate().rev() {
        if section_ordinal_of_event[idx] < lookback {
            continue;
        }
        if descriptions[idx].is_empty() {
            continue;
        }
        let cosine = cosine_similarity(candidate_emb, &descriptions[idx]);
        if cosine >= EVENT_DEDUPE_COSINE_STRICT {
            return Some(idx);
        }
        if cosine >= EVENT_DEDUPE_COSINE_WITH_PARTICIPANT
            && candidate_participants
                .iter()
                .any(|p| event.participants.contains(p))
        {
            return Some(idx);
        }
        let _ = sketch; // silence unused-variable lint if scope changes
    }
    None
}

// ── Step 3b: state / relation / claim / question resolution ──

/// Resolve state, relation, claim, and question atoms from Phase 1
/// sketches using the canonical entity atoms from Step 3a.
///
/// This is the structural pass — atom ids, trajectory chains, and
/// Grounds/Involves edges all land deterministically from sketch
/// content + the Step 3a entity name index. A later pass (Phase 5)
/// can enrich atom descriptions + classify state_type/event_type
/// via LLM; the shape written here stays stable.
///
/// Returns the new atoms + edges + trajectory index. The caller
/// merges this output with Step 3a's existing atoms/edges before
/// writing the combined `atlas/atoms.json` + `atlas/edges.json`.
pub fn resolve_step_3b(
    sections: &[SectionExtraction],
    entities: &[super::atoms::Entity],
    events: &[super::atoms::Event],
) -> Result<Step3bOutput> {
    use crate::enrichment::pipeline::types::{
        PhaseFailure, PhaseFailureKind, PipelinePhase,
    };

    let name_index = build_name_index(entities);
    let token_index = build_token_index(entities);
    let mut edges: Vec<Edge> = Vec::new();
    // Phase 3b drop buffer — every silent `debug!` path below now
    // also pushes a structured PhaseFailure so the aggregator can
    // show an operator how much evidence the deterministic resolver
    // lost on this corpus, grouped by kind.
    let mut failures: Vec<PhaseFailure> = Vec::new();

    // 1. Entity states (one State per EntityStateSketch)
    let mut states: Vec<super::atoms::State> = Vec::new();
    for (section_ordinal, section) in sections.iter().enumerate() {
        for (sketch_index, sketch) in section.entities_developed.iter().enumerate() {
            if sketch.entity_name.trim().is_empty() || sketch.label.trim().is_empty() {
                continue;
            }
            let Some(entity_id) = resolve_entity_id_fuzzy(
                &sketch.entity_name,
                &name_index,
                &token_index,
            ) else {
                let reason = format!(
                    "entity-state sketch references unknown entity `{}` (state label: `{}`)",
                    sketch.entity_name.trim(),
                    sketch.label.trim()
                );
                debug!(
                    entity_name = %sketch.entity_name,
                    section = %section.section_id,
                    "atlas/resolution 3b: entity-state sketch references unknown entity; dropping"
                );
                failures.push(PhaseFailure {
                    phase: PipelinePhase::Questions,
                    subject: format!(
                        "sketch:entity_state:{}#{}",
                        section.section_id, sketch_index
                    ),
                    kind: PhaseFailureKind::UnresolvedEntityName,
                    reason,
                    raw_response_head: None,
                });
                continue;
            };
            let state_id = super::atoms::AtomId::state(states.len() + 1);
            let evidence = sketch_anchor_evidence(&section.section_id, &sketch.anchor);
            let state = super::atoms::State {
                id: state_id.clone(),
                entity_id: entity_id.clone(),
                label: sketch.label.trim().to_string(),
                // State-type classification defers to Phase 5 — the
                // sketch carries no type. Use Other("unclassified")
                // so atoms are well-typed on disk.
                state_type: crate::enrichment::pipeline::atlas::StateType::Other(
                    "unclassified".into(),
                ),
                evidence: evidence.clone(),
                section_range: super::atoms::SectionRange::point(section.section_id.clone()),
                // Deterministic derivation — no LLM scoring here. The
                // atom exists because a sketch exists. Phase 5 (atom
                // interpretation) will replace this with a real
                // score with evidence depth. Leaving `None` keeps
                // the schema-validation histogram honest: it
                // tallies LLM-reported confidence only, so derived
                // atoms don't pile up in the [0.9-1.0) bucket and
                // mask the real calibration signal.
                confidence: None,
                enrichment_depth: section.enrichment_depth,
            };
            // Emit Involves (state → entity) and Grounds (state → evidence).
            edges.push(Edge {
                id: EdgeId::new(edges.len() + 1),
                edge_type: EdgeType::Involves,
                source: state_id.clone(),
                target: entity_id,
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
            for e in evidence {
                edges.push(Edge {
                    id: EdgeId::new(edges.len() + 1),
                    edge_type: EdgeType::Grounds,
                    source: state_id.clone(),
                    // Grounds edges target a chunk — we store the
                    // chunk_id as a raw AtomId so the edge's shape
                    // stays uniform. Downstream traversal
                    // distinguishes chunk-targets via the
                    // `evidence` slot.
                    target: super::atoms::AtomId::from_raw(e.chunk_id.clone()),
                    evidence: vec![e],
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
            // Keep section ordinal alongside each state for the
            // trajectory pass below.
            states.push(state);
            let _ = section_ordinal;
        }
    }

    // 2. Relations (one Relation per distinct participant-set from
    //    RelationSketch introductions). Dedup across sections on
    //    sorted-participant-ids key.
    let mut relations: Vec<super::atoms::Relation> = Vec::new();
    let mut relation_key_to_id: HashMap<String, super::atoms::AtomId> = HashMap::new();
    for section in sections {
        for (sketch_index, sketch) in section.relations_introduced.iter().enumerate() {
            let (participant_ids, unresolved) =
                resolve_entity_ids(&sketch.participants, entities, &name_index, &token_index);
            for name in &unresolved {
                failures.push(PhaseFailure {
                    phase: PipelinePhase::Questions,
                    subject: format!(
                        "sketch:relation_introduced:{}#{}",
                        section.section_id, sketch_index
                    ),
                    kind: PhaseFailureKind::UnresolvedRelationParticipant,
                    reason: format!(
                        "relation participant `{}` did not resolve (relation label: `{}`)",
                        name,
                        sketch.label.trim()
                    ),
                    raw_response_head: None,
                });
            }
            if participant_ids.len() < 2 {
                debug!(
                    participants = ?sketch.participants,
                    "atlas/resolution 3b: relation sketch has <2 resolvable participants; dropping"
                );
                continue;
            }
            let key = relation_key(&participant_ids);
            if relation_key_to_id.contains_key(&key) {
                continue;
            }
            let rel_id = super::atoms::AtomId::relation(relations.len() + 1);
            relation_key_to_id.insert(key, rel_id.clone());
            relations.push(super::atoms::Relation {
                id: rel_id.clone(),
                label: sketch.label.trim().to_string(),
                participants: participant_ids.clone(),
                relation_type: crate::enrichment::pipeline::atlas::RelationType::Other(
                    "unclassified".into(),
                ),
                evidence: sketch_anchor_evidence(&section.section_id, &sketch.anchor),
                section_range: super::atoms::SectionRange::point(section.section_id.clone()),
                enrichment_depth: section.enrichment_depth,
            });
            // Emit one Involves edge per participant.
            for pid in &participant_ids {
                edges.push(Edge {
                    id: EdgeId::new(edges.len() + 1),
                    edge_type: EdgeType::Involves,
                    source: rel_id.clone(),
                    target: pid.clone(),
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
        }
    }

    // 3. Relation states (State atoms pointing at a Relation atom).
    //    If the relation wasn't introduced, synthesise one lazily so
    //    the state has somewhere to attach.
    for section in sections {
        for (sketch_index, sketch) in section.relations_developed.iter().enumerate() {
            let (participant_ids, unresolved) =
                resolve_entity_ids(&sketch.participants, entities, &name_index, &token_index);
            for name in &unresolved {
                failures.push(PhaseFailure {
                    phase: PipelinePhase::Questions,
                    subject: format!(
                        "sketch:relation_developed:{}#{}",
                        section.section_id, sketch_index
                    ),
                    kind: PhaseFailureKind::UnresolvedRelationParticipant,
                    reason: format!(
                        "relation-state participant `{}` did not resolve (state label: `{}`)",
                        name,
                        sketch.label.trim()
                    ),
                    raw_response_head: None,
                });
            }
            if participant_ids.len() < 2 {
                debug!(
                    participants = ?sketch.participants,
                    "atlas/resolution 3b: relation-state sketch has <2 resolvable participants; dropping"
                );
                continue;
            }
            let key = relation_key(&participant_ids);
            let rel_id = match relation_key_to_id.get(&key).cloned() {
                Some(id) => id,
                None => {
                    let new_id = super::atoms::AtomId::relation(relations.len() + 1);
                    relation_key_to_id.insert(key.clone(), new_id.clone());
                    relations.push(super::atoms::Relation {
                        id: new_id.clone(),
                        label: format!("Unnamed relation between {}", sketch.participants.join(" × ")),
                        participants: participant_ids.clone(),
                        relation_type:
                            crate::enrichment::pipeline::atlas::RelationType::Other(
                                "unclassified".into(),
                            ),
                        evidence: Vec::new(),
                        section_range: super::atoms::SectionRange::point(
                            section.section_id.clone(),
                        ),
                        enrichment_depth: section.enrichment_depth,
                    });
                    new_id
                }
            };
            let state_id = super::atoms::AtomId::state(states.len() + 1);
            let evidence = sketch_anchor_evidence(&section.section_id, &sketch.anchor);
            states.push(super::atoms::State {
                id: state_id.clone(),
                entity_id: rel_id.clone(),
                label: sketch.label.trim().to_string(),
                state_type: crate::enrichment::pipeline::atlas::StateType::Other(
                    "unclassified".into(),
                ),
                evidence: evidence.clone(),
                section_range: super::atoms::SectionRange::point(section.section_id.clone()),
                // Derived — see the note at the entity-state call site.
                confidence: None,
                enrichment_depth: section.enrichment_depth,
            });
            edges.push(Edge {
                id: EdgeId::new(edges.len() + 1),
                edge_type: EdgeType::Involves,
                source: state_id.clone(),
                target: rel_id,
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
            for e in evidence {
                edges.push(Edge {
                    id: EdgeId::new(edges.len() + 1),
                    edge_type: EdgeType::Grounds,
                    source: state_id.clone(),
                    target: super::atoms::AtomId::from_raw(e.chunk_id.clone()),
                    evidence: vec![e],
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
        }
    }

    // 4. Claims — one atom per ClaimSketch with attributed_to
    //    resolved to an Entity id when possible.
    let mut claims: Vec<super::atoms::Claim> = Vec::new();
    for section in sections {
        for (sketch_index, sketch) in section.claims.iter().enumerate() {
            let claim_id = super::atoms::AtomId::claim(claims.len() + 1);
            let attributed_to = sketch.attributed_to.as_ref().and_then(|name| {
                let resolved =
                    resolve_entity_id_with_salience(name, entities, &name_index, &token_index);
                if resolved.is_none() {
                    // Claim keeps its content; only attribution drops.
                    // Recorded so the aggregator can group these by
                    // kind and the operator sees how many claims lost
                    // attribution without having to diff the content.
                    failures.push(PhaseFailure {
                        phase: PipelinePhase::Questions,
                        subject: format!(
                            "sketch:claim:{}#{}",
                            section.section_id, sketch_index
                        ),
                        kind: PhaseFailureKind::UnresolvedClaimAttribution,
                        reason: format!(
                            "claim attributed_to `{}` did not resolve (claim content: `{}`)",
                            name,
                            sketch.content.trim()
                        ),
                        raw_response_head: None,
                    });
                }
                resolved
            });
            let evidence = sketch_anchor_evidence(&section.section_id, &sketch.anchor);
            claims.push(super::atoms::Claim {
                id: claim_id.clone(),
                content: sketch.content.trim().to_string(),
                discourse_act: sketch.discourse_act.clone(),
                epistemic_status: sketch.epistemic_status.clone(),
                // Scope defers to Phase 5 (sketches dropped it per
                // the slim schema). Fictional is the literary default.
                scope: crate::enrichment::pipeline::atlas::ClaimScope::Fictional,
                evidence: evidence.clone(),
                attributed_to: attributed_to.clone(),
                // Derived — Phase 5 will replace with LLM score.
                confidence: None,
                enrichment_depth: section.enrichment_depth,
            });
            if let Some(entity_id) = attributed_to {
                edges.push(Edge {
                    id: EdgeId::new(edges.len() + 1),
                    edge_type: EdgeType::Involves,
                    source: claim_id.clone(),
                    target: entity_id,
                    evidence: Vec::new(),
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
            for e in evidence {
                edges.push(Edge {
                    id: EdgeId::new(edges.len() + 1),
                    edge_type: EdgeType::Grounds,
                    source: claim_id.clone(),
                    target: super::atoms::AtomId::from_raw(e.chunk_id.clone()),
                    evidence: vec![e],
                    trigger_event: None,
                    sub_question: None,
                    confidence: 1.0,
                    provenance: EdgeProvenance::Derived,
                });
            }
        }
    }

    // 5. Questions — one atom per QuestionSketch. Resolution status
    //    defaults to Open; Phase 5 can tie `addressed_by` claim
    //    atoms once a question cluster resolves.
    let mut questions: Vec<super::atoms::Question> = Vec::new();
    for section in sections {
        for sketch in &section.questions_raised {
            let q_id = super::atoms::AtomId::question(questions.len() + 1);
            let evidence = sketch_anchor_evidence(&section.section_id, &sketch.anchor);
            questions.push(super::atoms::Question {
                id: q_id,
                content: sketch.content.trim().to_string(),
                question_type: crate::enrichment::pipeline::atlas::QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: evidence,
                resolution_status: super::atoms::ResolutionStatus::Open,
                enrichment_depth: section.enrichment_depth,
            });
        }
    }

    // 6. Trajectory index + Transition edges. Group states by their
    //    entity_id (or relation_id — same field), sort by section
    //    start, then emit one Transition per consecutive pair. Each
    //    transition gets a best-effort deterministic `trigger_event`
    //    when the evidence is unambiguous: an Event that lives in
    //    the section window between `from` and `to` AND has the
    //    owning entity as a participant. Ambiguous cases leave
    //    `trigger_event = None` — we'd rather say "I don't know"
    //    than attach a plausible-but-wrong event. Relation-owned
    //    trajectories currently leave triggers None (Events don't
    //    reference Relation ids in `participants`; a full match
    //    would require participant-set intersection, and Phase 5
    //    will handle relation-state triggers with evidence depth).
    let section_ordinal = build_section_ordinal_map(sections);
    let mut trajectories: std::collections::BTreeMap<String, Trajectory> =
        std::collections::BTreeMap::new();
    let states_by_owner = group_states_by_owner(&states);
    for (owner_id, owner_states) in states_by_owner {
        let (owner_name, owner_atom_type) =
            owner_display(&owner_id, entities, &relations);
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
            edges.push(Edge {
                id: EdgeId::new(edges.len() + 1),
                edge_type: EdgeType::Transition,
                source: from.id.clone(),
                target: to.id.clone(),
                evidence: Vec::new(),
                trigger_event: trigger.clone(),
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::Derived,
            });
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

    Ok(Step3bOutput {
        states,
        relations,
        claims,
        questions,
        edges,
        trajectories,
        failures,
    })
}

// ── Step 3b helpers ────────────────────────────────────────

fn build_name_index(
    entities: &[super::atoms::Entity],
) -> HashMap<String, super::atoms::AtomId> {
    let mut index: HashMap<String, super::atoms::AtomId> = HashMap::new();
    for e in entities {
        index.insert(fold(&e.canonical_name), e.id.clone());
        for alias in &e.aliases {
            index.insert(fold(alias), e.id.clone());
        }
    }
    index
}

/// Token-level inverted index: folded token → list of entity ids
/// whose canonical_name or any alias contains that token as a
/// whitespace-delimited word. Tokens of length <
/// `ENTITY_MERGE_TOKEN_MIN_LEN` are omitted (matches the
/// shared-token-overlap guard). Built alongside `name_index` for
/// fuzzy participant lookups in Step 3b.
fn build_token_index(
    entities: &[super::atoms::Entity],
) -> HashMap<String, Vec<super::atoms::AtomId>> {
    let mut idx: HashMap<String, Vec<super::atoms::AtomId>> = HashMap::new();
    for e in entities {
        for name in std::iter::once(&e.canonical_name).chain(e.aliases.iter()) {
            for token in fold(name).split_whitespace() {
                if token.len() < ENTITY_MERGE_TOKEN_MIN_LEN {
                    continue;
                }
                let bucket = idx.entry(token.to_string()).or_default();
                if !bucket.contains(&e.id) {
                    bucket.push(e.id.clone());
                }
            }
        }
    }
    idx
}

/// Fuzzy participant / attribution lookup with three fallbacks,
/// tried in order of decreasing confidence:
///
/// 1. Exact fold match on the whole name (covers canonical_name +
///    alias the name_index already knows). `fold()` already applies
///    Cyrillic transliteration so `Karamазов` matches `Karamazov`.
/// 2. Long-token inverted-index match: a folded query token of
///    length ≥ `FUZZY_TOKEN_MIN_LEN` that appears in exactly one
///    entity's tokens resolves to that entity.
/// 3. Per-token Levenshtein vote: for each query token of
///    length ≥ `FUZZY_TOKEN_MIN_LEN`, find every index token within
///    edit distance ≤ `FUZZY_TOKEN_LEVENSHTEIN_MAX`. If the matches
///    for that query token all belong to a single entity, that
///    entity gets a vote. After scanning all query tokens, if
///    exactly one entity has ≥ 1 vote, snap to it. Catches
///    hallucinations like `Karazoff` → `Karamazoff` (2 edits) while
///    the single-match policy keeps `Ivan` from collapsing into
///    `Ilya`.
///
/// Ambiguous at any stage → None. We prefer silence to a confident
/// wrong answer — a dropped participant is recoverable downstream,
/// a wrong participant pollutes the trajectory.
pub(super) fn resolve_entity_id_fuzzy(
    name: &str,
    name_index: &HashMap<String, super::atoms::AtomId>,
    token_index: &HashMap<String, Vec<super::atoms::AtomId>>,
) -> Option<super::atoms::AtomId> {
    let folded = fold(name);
    if let Some(id) = name_index.get(&folded) {
        return Some(id.clone());
    }
    // Fallback 2: long-token exact inverted-index lookup.
    for token in folded.split_whitespace() {
        if token.len() < FUZZY_TOKEN_MIN_LEN {
            continue;
        }
        if let Some(candidates) = token_index.get(token) {
            if candidates.len() == 1 {
                return Some(candidates[0].clone());
            }
        }
    }
    // Fallback 3: per-token Levenshtein vote. Each query token
    // (len ≥ FUZZY_TOKEN_MIN_LEN) scans every index token for edits
    // ≤ FUZZY_TOKEN_LEVENSHTEIN_MAX. If a query token's matches all
    // belong to one entity, that entity gets a vote. A single
    // entity with ≥ 1 vote wins; ties or splits stay unresolved.
    let mut votes: HashMap<super::atoms::AtomId, u32> = HashMap::new();
    for q_token in folded.split_whitespace() {
        if q_token.len() < FUZZY_TOKEN_MIN_LEN {
            continue;
        }
        let mut matched_entities: Vec<super::atoms::AtomId> = Vec::new();
        for (idx_token, ids) in token_index.iter() {
            if idx_token.len() < FUZZY_TOKEN_MIN_LEN {
                continue;
            }
            if levenshtein(q_token, idx_token) <= FUZZY_TOKEN_LEVENSHTEIN_MAX {
                for id in ids {
                    if !matched_entities.contains(id) {
                        matched_entities.push(id.clone());
                    }
                }
            }
        }
        if matched_entities.len() == 1 {
            *votes.entry(matched_entities.into_iter().next().unwrap()).or_insert(0) += 1;
        }
    }
    if votes.len() == 1 {
        return votes.into_keys().next();
    }
    None
}

/// Minimum folded-token length considered by the fuzzy paths.
/// Protects short ambiguous names (`ivan`, `anna`, `lise`) from
/// collapsing across distinct entities.
const FUZZY_TOKEN_MIN_LEN: usize = 5;

/// Salience ratio required for the dominance tiebreaker to pick a
/// winner from multiple candidates. When several entities share a
/// name/token and the strict fuzzy resolver bails on ambiguity,
/// the salience-aware fallback picks the top candidate only if its
/// salience is at least `SALIENCE_DOMINANCE_FACTOR × the runner-up`.
/// Calibrated so a drift variant with low extraction salience does
/// not block attribution to the dominant entity — in *Brothers
/// Karamazov* an `attributed_to: "Fyodor"` query should land on
/// the father (salience ~1.0) rather than stay unresolved because
/// a once-referenced drift variant (salience ~0.15) also matches
/// the token `fyodor`.
const SALIENCE_DOMINANCE_FACTOR: f32 = 2.0;

/// Attribution/participant resolution with a salience-aware
/// fallback. Tries the strict `resolve_entity_id_fuzzy` first; if
/// that returns None because multiple entities share the query's
/// tokens, applies a salience tiebreaker:
///
/// 1. Shortlist entities whose first-token matches (exactly or
///    within Lev 1) AND at least one long query token appears in
///    any of the entity's name-or-alias tokens.
/// 2. If the shortlist has one entry, take it.
/// 3. If the shortlist has N > 1 entries, sort by salience
///    descending. Pick the top only if it dominates the next by
///    `SALIENCE_DOMINANCE_FACTOR`. Otherwise return None.
///
/// The strict fuzzy path stays in place for Phase 3a entity
/// merging where safety-first matters. The salience fallback is
/// opt-in for Phase 3b attribution/relation resolution, where
/// coverage of cross-section connections matters more than a
/// single wrong snap.
pub(super) fn resolve_entity_id_with_salience(
    name: &str,
    entities: &[super::atoms::Entity],
    name_index: &HashMap<String, super::atoms::AtomId>,
    token_index: &HashMap<String, Vec<super::atoms::AtomId>>,
) -> Option<super::atoms::AtomId> {
    // Strict path first — preserves the current safety guarantees.
    if let Some(id) = resolve_entity_id_fuzzy(name, name_index, token_index) {
        return Some(id);
    }

    let folded = fold(name);
    let q_long_tokens: Vec<String> = folded
        .split_whitespace()
        .filter(|t| t.len() >= FUZZY_TOKEN_MIN_LEN)
        .map(str::to_string)
        .collect();
    if q_long_tokens.is_empty() {
        // Below the min-token-length guard; the strict resolver
        // already refused to snap and salience can't safely
        // distinguish short ambiguous names. Stay silent.
        return None;
    }

    // Shortlist entities that (a) share a first token (exact or
    // Lev ≤ 1) with the query AND (b) share at least one long
    // token with the query.
    let mut shortlist: Vec<&super::atoms::Entity> = Vec::new();
    for e in entities {
        let first_ok = first_token_matches(name, &e.canonical_name)
            || e
                .aliases
                .iter()
                .any(|a| first_token_matches(name, a));
        if !first_ok {
            continue;
        }
        let mut shared_long = false;
        for n in std::iter::once(&e.canonical_name).chain(e.aliases.iter()) {
            let n_tokens: std::collections::HashSet<String> = fold(n)
                .split_whitespace()
                .filter(|t| t.len() >= FUZZY_TOKEN_MIN_LEN)
                .map(str::to_string)
                .collect();
            if q_long_tokens.iter().any(|q| n_tokens.contains(q)) {
                shared_long = true;
                break;
            }
        }
        if shared_long {
            shortlist.push(e);
        }
    }

    if shortlist.is_empty() {
        return None;
    }
    if shortlist.len() == 1 {
        return Some(shortlist[0].id.clone());
    }

    // Salience tiebreaker. `partial_cmp` can return None for NaN
    // salience; fall back to Equal so NaN doesn't crash the sort.
    shortlist.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top = shortlist[0].salience;
    let second = shortlist.get(1).map(|e| e.salience).unwrap_or(0.0);
    // `top >= factor * second` covers the second == 0 case
    // cleanly (the top wins unconditionally). When second is
    // positive, top must be at least factor × bigger.
    if top >= SALIENCE_DOMINANCE_FACTOR * second {
        Some(shortlist[0].id.clone())
    } else {
        None
    }
}

/// Max Levenshtein distance between a query token and an index
/// token that still counts as a match in the vote fallback.
/// Calibrated against observed model drift: `karazoff ↔ karamazoff`
/// (2 edits — real character drop + substitution) snaps, but
/// `pvlvitch ↔ pavlovich` (4-5 edits — actual hallucination of the
/// patronymic) stays silent. 2 is the knob.
const FUZZY_TOKEN_LEVENSHTEIN_MAX: usize = 2;

/// Resolve a list of participant names via the salience-aware
/// fuzzy path. Returns `(resolved, unresolved)` — the unresolved
/// names are returned rather than silently dropped (see
/// `resolve_participant_ids` for the rationale).
fn resolve_entity_ids(
    names: &[String],
    entities: &[super::atoms::Entity],
    name_index: &HashMap<String, super::atoms::AtomId>,
    token_index: &HashMap<String, Vec<super::atoms::AtomId>>,
) -> (Vec<super::atoms::AtomId>, Vec<String>) {
    let mut resolved = Vec::with_capacity(names.len());
    let mut unresolved = Vec::new();
    for n in names {
        // Relation/event participant resolution goes through the
        // salience-aware path so a dominant-salience entity snaps
        // when multiple drift variants share the name. Phase 3a's
        // alias/cluster-merge rules should collapse most variants
        // before we get here; the salience fallback is the safety
        // net for the cases that slipped through.
        if let Some(id) = resolve_entity_id_with_salience(n, entities, name_index, token_index) {
            if !resolved.contains(&id) {
                resolved.push(id);
            }
        } else {
            unresolved.push(n.clone());
        }
    }
    (resolved, unresolved)
}

fn relation_key(participant_ids: &[super::atoms::AtomId]) -> String {
    let mut sorted: Vec<&str> = participant_ids.iter().map(|a| a.as_str()).collect();
    sorted.sort();
    sorted.join("|")
}

/// One-shot Evidence ChunkRef derived from a sketch's anchor. Used
/// everywhere sketches carry anchors — Phase 5 will replace this
/// with a real chunk lookup against the LanceDB index.
fn sketch_anchor_evidence(section_id: &str, anchor: &str) -> Vec<super::atoms::ChunkRef> {
    if anchor.trim().is_empty() {
        vec![super::atoms::ChunkRef::new(section_id.to_string(), None)]
    } else {
        vec![super::atoms::ChunkRef::new(
            section_id.to_string(),
            Some(anchor.trim().to_string()),
        )]
    }
}

fn group_states_by_owner(
    states: &[super::atoms::State],
) -> Vec<(super::atoms::AtomId, Vec<super::atoms::State>)> {
    let mut map: HashMap<String, Vec<super::atoms::State>> = HashMap::new();
    for s in states {
        map.entry(s.entity_id.as_str().to_string())
            .or_default()
            .push(s.clone());
    }
    // Deterministic order by owner id for stable transition
    // numbering across runs. Within an owner, sort states by section
    // start — the trajectory pass assumes this order.
    let mut entries: Vec<(super::atoms::AtomId, Vec<super::atoms::State>)> = map
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_by(|a, b| a.section_range.start.cmp(&b.section_range.start));
            (super::atoms::AtomId::from_raw(k), v)
        })
        .collect();
    entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    entries
}

/// Map `section_id` → ordinal position in the corpus. Used by the
/// Transition-trigger matcher to decide which section ids fall
/// between a `from` state and a `to` state. Building this from the
/// order `sections` arrive in is deterministic across runs.
fn build_section_ordinal_map(
    sections: &[SectionExtraction],
) -> HashMap<String, usize> {
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
    owner_id: &super::atoms::AtomId,
    from: &super::atoms::State,
    to: &super::atoms::State,
    events: &[super::atoms::Event],
    section_ordinal: &HashMap<String, usize>,
) -> Option<super::atoms::AtomId> {
    if !owner_id.as_str().starts_with("entity-") {
        return None;
    }
    let from_ord = section_ordinal.get(&from.section_range.end)?;
    let to_ord = section_ordinal.get(&to.section_range.start)?;
    if to_ord <= from_ord {
        return None;
    }
    let mut matches: Vec<&super::atoms::Event> = events
        .iter()
        .filter(|e| {
            let Some(ord) = section_ordinal.get(&e.section_position.section_id) else {
                return false;
            };
            // Half-open window (from_ord, to_ord] — the trigger sits
            // strictly after `from` and at or before `to`.
            *ord > *from_ord
                && *ord <= *to_ord
                && e.participants.iter().any(|p| p == owner_id)
        })
        .collect();
    if matches.len() == 1 {
        return Some(matches.remove(0).id.clone());
    }
    None
}

fn owner_display(
    owner_id: &super::atoms::AtomId,
    entities: &[super::atoms::Entity],
    relations: &[super::atoms::Relation],
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

// ── Small utilities ─────────────────────────────────────────

/// Case-fold + trim + Russian-Cyrillic transliteration + Latin
/// combining-diacritic strip. The name index is keyed on this form
/// so lookups are forgiving of:
///
/// - Case and surrounding whitespace.
/// - Russian-novel LLM output that mixes Cyrillic mid-word
///   (`Karamазов` ↔ `Karamazov`) — collapsed by `transliterate_cyrillic`.
/// - Latin diacritic drift from models that over-decorate
///   transliterations (`Karámazov` ↔ `Karamazov`, `Fyódor` ↔ `Fyodor`,
///   `Miüsov` ↔ `Miusov`) — collapsed by NFD decomposition followed by
///   dropping Unicode combining marks.
///
/// The NFD step decomposes a precomposed `á` into `a` + U+0301 (combining
/// acute); we then filter the marks out, leaving plain `a`. This makes
/// fold idempotent under diacritic perturbation — the model can emit
/// any mixture of decorations and the index still finds the entity.
///
/// Scope: Russian (and passthrough Ukrainian) Cyrillic; Latin
/// diacritics across the full Unicode combining-mark block. Adding
/// Serbian, Greek, or Arabic scripts is cheap but untested; do it
/// when a corpus requires it.
pub fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    transliterate_cyrillic(&s.trim().to_lowercase())
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect()
}

/// Best-effort lower-case Russian Cyrillic → Latin transliteration.
/// Passes every non-Cyrillic char through unchanged — so an already-
/// Latin string round-trips byte-for-byte. Chosen to match the
/// transliteration the LLM itself produces when asked for an
/// English form (Karamazov, Zosima, Alyosha).
fn transliterate_cyrillic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let repl: &str = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' => "",
            'ы' => "y",
            'ь' => "",
            'э' => "e",
            'ю' => "yu",
            'я' => "ya",
            // Ukrainian additions — cheap to include; exact Russian
            // texts will never hit these branches.
            'є' => "ye",
            'і' => "i",
            'ї' => "yi",
            'ґ' => "g",
            // Any other char passes through.
            other => {
                out.push(other);
                continue;
            }
        };
        out.push_str(repl);
    }
    out
}

/// Count tokens shared between two name forms. Tokens are
/// whitespace-split, case-folded, Cyrillic-transliterated, and must
/// be length ≥ 3 (after transliteration) to count. Drops short
/// patronymic particles.
///
/// Transliteration is what makes mixed-encoding names merge: the
/// token `Karamазов` (Cyrillic letters mid-word) transliterates to
/// `karamazov`, which matches the pure-Latin `Karamazov` lowered to
/// `karamazov`. Without this step they would be byte-distinct and
/// rule 3 would fail to fire.
/// Rule 4 helper. Returns the index of the first existing entity
/// whose folded canonical_name or any alias is a proper substring
/// of `folded_query` (or vice versa), when the shorter side is at
/// least `ENTITY_MERGE_SUBSTRING_MIN_LEN` characters after
/// transliteration + case-fold.
///
/// Substring match is a STRONGER signal than shared-token overlap
/// (proper substring implies all tokens of the shorter appear in
/// the longer, in order) so we let it cross the section window.
fn find_substring_match(folded_query: &str, entities: &[Entity]) -> Option<usize> {
    let q = folded_query;
    if q.len() < ENTITY_MERGE_SUBSTRING_MIN_LEN {
        return None;
    }
    for (idx, existing) in entities.iter().enumerate() {
        for name in std::iter::once(&existing.canonical_name).chain(existing.aliases.iter()) {
            let candidate = fold(name);
            if candidate.len() < ENTITY_MERGE_SUBSTRING_MIN_LEN {
                continue;
            }
            if candidate == q {
                // Exact-fold match should have been caught by rule 1;
                // defensive here.
                return Some(idx);
            }
            // Require a word boundary so `Ivan` doesn't substring-merge
            // into `Ivanovich` via bare contains(). Word boundary:
            // either start/end of string, OR preceded/followed by a
            // non-alphanumeric char (whitespace, hyphen, apostrophe).
            if has_whole_word(q, &candidate) || has_whole_word(&candidate, q) {
                return Some(idx);
            }
        }
    }
    None
}

/// True when `needle` appears inside `haystack` with whitespace /
/// punctuation (or string boundary) on both sides. Prevents
/// `Ivan` from substring-merging into `Ivanovich`.
fn has_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let is_boundary = |c: char| !(c.is_alphanumeric() || c == '_');
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(needle) {
        let pos = start + rel;
        let before_ok = pos == 0
            || haystack[..pos]
                .chars()
                .last()
                .map(is_boundary)
                .unwrap_or(true);
        let end = pos + needle.len();
        let after_ok = end == haystack.len()
            || haystack[end..].chars().next().map(is_boundary).unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// True when the first folded token of `a` matches the first folded
/// token of `b` exactly, OR within Levenshtein 1 when both are at
/// least `FUZZY_TOKEN_MIN_LEN` characters. Used as rule 3's first-
/// token guard so siblings sharing patronymic + surname don't
/// collapse into one entity. A single-token name (`Grigory`,
/// `Zossima`) counts its whole name as its first token — correct
/// behaviour for mononyms.
fn first_token_matches(a: &str, b: &str) -> bool {
    let fa = fold(a);
    let fb = fold(b);
    let ta = fa.split_whitespace().next().unwrap_or("");
    let tb = fb.split_whitespace().next().unwrap_or("");
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    if ta == tb {
        return true;
    }
    if ta.len() >= FUZZY_TOKEN_MIN_LEN
        && tb.len() >= FUZZY_TOKEN_MIN_LEN
        && levenshtein(ta, tb) <= 1
    {
        return true;
    }
    false
}

fn shared_token_overlap(a: &str, b: &str) -> usize {
    // Use `fold()` (not just transliterate + lowercase) so Latin
    // combining diacritics are stripped before comparison. Without
    // this, the model's decorated `Pavlovič` (NFD: p-a-v-l-o-v-i-č)
    // and clean `Pavlovitch` share no token, and rule 3 fails to
    // fire across drift variants of the same name. The tokeniser
    // itself splits on whitespace; we fold each token individually.
    let tokens_a: Vec<String> = fold(a)
        .split_whitespace()
        .filter(|t| t.len() >= ENTITY_MERGE_TOKEN_MIN_LEN)
        .map(str::to_string)
        .collect();
    let tokens_b: Vec<String> = fold(b)
        .split_whitespace()
        .filter(|t| t.len() >= ENTITY_MERGE_TOKEN_MIN_LEN)
        .map(str::to_string)
        .collect();
    let a_exact: std::collections::HashSet<&str> =
        tokens_a.iter().map(String::as_str).collect();
    // Two pass: count exact matches first (each b-token can match at
    // most one a-token) then count fuzzy matches for b-tokens that
    // did not match exactly. Fuzzy = Levenshtein ≤ 1 on tokens of
    // length ≥ 5 on BOTH sides (preserves the Ivan/Ilya guard and
    // limits the risk of collapsing unrelated short surnames). This
    // catches the drift class the model emits today:
    // `Karazov ↔ Karamazov` (Lev 2 — too loose for the per-token
    // guard, caught by the participant-side vote fallback instead),
    // `Fyodoric ↔ Fyodoroic` (Lev 1), `Zossima ↔ Zosima` (Lev 1).
    // A Lev-2 per-token cap would catch `Karazov ↔ Karamazov`
    // here but also risks collapsing `Ivanov ↔ Ivanko`; Lev-1 is
    // the conservative balance that Phase 3a wants when deciding
    // whether to coalesce two entity atoms.
    let mut count = 0;
    let mut a_used = vec![false; tokens_a.len()];
    for tb in &tokens_b {
        if a_exact.contains(tb.as_str()) {
            // Mark the corresponding a-token as used so later fuzzy
            // passes don't double-count.
            if let Some(i) = tokens_a.iter().position(|ta| ta == tb) {
                if !a_used[i] {
                    a_used[i] = true;
                    count += 1;
                }
            }
        }
    }
    // Fuzzy pass — only for b-tokens that had no exact match.
    for tb in tokens_b
        .iter()
        .filter(|tb| !a_exact.contains(tb.as_str()))
    {
        if tb.len() < FUZZY_TOKEN_MIN_LEN {
            continue;
        }
        for (i, ta) in tokens_a.iter().enumerate() {
            if a_used[i] || ta.len() < FUZZY_TOKEN_MIN_LEN {
                continue;
            }
            if levenshtein(tb, ta) <= 1 {
                a_used[i] = true;
                count += 1;
                break;
            }
        }
    }
    count
}

/// Like [`shared_token_overlap`] but counts only *long* tokens
/// (length ≥ `FUZZY_TOKEN_MIN_LEN` after fold). Used by Phase 3a
/// Rule 3.5 — short tokens like `the` or first-name variants that
/// already got counted by `first_token_matches` shouldn't double-
/// count here. Exact-match only (no fuzzy pass) — this rule relies
/// on a clean shared surname-class token + description cosine for
/// its signal.
fn shared_long_token_count(a: &str, b: &str) -> usize {
    let tokens_a: std::collections::HashSet<String> = fold(a)
        .split_whitespace()
        .filter(|t| t.len() >= FUZZY_TOKEN_MIN_LEN)
        .map(str::to_string)
        .collect();
    fold(b)
        .split_whitespace()
        .filter(|t| t.len() >= FUZZY_TOKEN_MIN_LEN)
        .filter(|t| tokens_a.contains(*t))
        .count()
}

/// Classical DP Levenshtein distance. Inputs are expected case-
/// folded; the function itself is purely character-based.
fn levenshtein(a: &str, b: &str) -> usize {
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1) // deletion
                .min(cur[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::atlas::{
        EnrichmentDepth, EntitySketch, EntityType, EventSketch,
    };
    use std::sync::Arc;

    /// Deterministic embed: first two letters seed a small 3-vector.
    /// Same text → same vector; slightly different text → different
    /// direction. Lets us pin cosine-threshold behaviour without
    /// touching a model.
    fn fake_embed() -> EmbedFn {
        Arc::new(move |s: &str| {
            let s = s.to_string();
            Box::pin(async move {
                let bytes = s.as_bytes();
                let a = bytes.first().copied().unwrap_or(0) as f32;
                let b = bytes.get(1).copied().unwrap_or(0) as f32;
                let c = bytes.get(2).copied().unwrap_or(0) as f32;
                Ok(vec![a, b, c])
            })
        })
    }

    /// Embed that forces a specific cosine similarity — returns the
    /// same vector for any input, so all comparisons land at 1.0.
    fn always_one_embed() -> EmbedFn {
        Arc::new(move |_s: &str| Box::pin(async move { Ok(vec![1.0_f32, 0.0, 0.0]) }))
    }

    fn section(
        id: &str,
        entities: Vec<EntitySketch>,
        events: Vec<EventSketch>,
    ) -> SectionExtraction {
        SectionExtraction {
            section_id: id.into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_introduced: entities,
            entities_developed: Vec::new(),
            relations_introduced: Vec::new(),
            relations_developed: Vec::new(),
            events,
            claims: Vec::new(),
            questions_raised: Vec::new(),
        }
    }

    fn entity(name: &str, aliases: &[&str], description: &str) -> EntitySketch {
        EntitySketch {
            canonical_name: name.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            entity_type: EntityType::Person,
            description: description.into(),
            anchor: String::new(),
        }
    }

    fn event(desc: &str, participants: &[&str]) -> EventSketch {
        EventSketch {
            description: desc.into(),
            participants: participants.iter().map(|s| s.to_string()).collect(),
            anchor: String::new(),
        }
    }

    #[tokio::test]
    async fn atlas_resolve_merges_alias_variants_into_single_entity() {
        // Rule 1: alias match. Sketch in sec_0002 has canonical_name
        // "Alyosha" which appears in sec_0001's aliases.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Alexei Fyodorovich Karamazov",
                    &["Alyosha", "Alexey"],
                    "The youngest Karamazov brother.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Alyosha", &[], "Novice at the monastery.")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            1,
            "alias match should merge into a single entity"
        );
        // Merged entity keeps the original canonical_name, picks up
        // the richer description, and unions aliases.
        let e = &out.entities[0];
        assert_eq!(e.canonical_name, "Alexei Fyodorovich Karamazov");
        assert!(e.aliases.iter().any(|a| a == "Alyosha"));
    }

    #[tokio::test]
    async fn atlas_resolve_keeps_distinct_entities_with_similar_names_and_different_descriptions() {
        // Rule 2 requires BOTH Levenshtein ≤ 2 AND cosine ≥ 0.90.
        // "Ivan" and "Ilya" are Levenshtein 2 apart but the fake
        // embed gives them different directions → cosine ≈ 0.98-ish
        // actually since the vectors are (i,v,a) and (i,l,y)... the
        // first character matches, the rest don't. We force the
        // cosine gap by using descriptions that differ in the first
        // byte.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity("Ivan", &[], "One distinct description starting with O.")],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Ilya", &[], "A different description starting with A.")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            2,
            "Ivan and Ilya must stay distinct when descriptions diverge"
        );
    }

    #[tokio::test]
    async fn atlas_resolve_merges_russian_patronymic_variants_via_shared_tokens() {
        // Rule 3: ≥ 2 shared tokens of length ≥ 3 after case-folding.
        // "Alexei Fyodorovich Karamazov" and "Alexei Fyodorovich"
        // share "alexei" and "fyodorovich" — 2 tokens, qualifies.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Alexei Fyodorovich Karamazov",
                    &[],
                    "Youngest brother.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Alexei Fyodorovich", &[], "The novice.")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            1,
            "shared-token overlap should merge patronymic variants"
        );
        assert!(out.entities[0]
            .aliases
            .iter()
            .any(|a| a == "Alexei Fyodorovich"));
    }

    #[tokio::test]
    async fn atlas_resolve_rule_3_bounds_cross_section_matching_to_window() {
        // Rule 3 (shared-token overlap) requires ≥ 2 shared tokens
        // AND stays within the 5-section lookback. Pick a name pair
        // that (a) doesn't substring-match — so rule 4 stays out —
        // and (b) would match via rule 3 if the window allowed.
        // "Fyodorovich Alexei" and "Alexei Fyodorovich Karamazov"
        // share "alexei" + "fyodorovich" (2 tokens, both len ≥ 5)
        // but neither is a substring of the other.
        let mut sections = Vec::new();
        sections.push(section(
            "sec_0001",
            vec![entity(
                "Fyodorovich Alexei",
                &[],
                "Youngest brother.",
            )],
            vec![],
        ));
        for i in 2..=9 {
            sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
        }
        sections.push(section(
            "sec_0010",
            vec![entity(
                "Alexei Fyodorovich Karamazov",
                &[],
                "The novice.",
            )],
            vec![],
        ));
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            2,
            "rule 3 shouldn't cross the 5-section window without alias evidence"
        );
    }

    #[tokio::test]
    async fn atlas_resolve_rule_4_substring_crosses_any_section_distance() {
        // Rule 4 (substring match, len ≥ 5) is a stronger signal
        // than shared-token overlap and is allowed to cross the
        // lookback window. Covers the common fragmentation of
        // `Alyosha Karamazov` (earlier section) ↔ `Alyosha` (much
        // later) that blocked Step 3b trajectory construction on
        // real Brothers Karamazov data.
        let mut sections = Vec::new();
        sections.push(section(
            "sec_0001",
            vec![entity("Alyosha Karamazov", &[], "Youngest brother.")],
            vec![],
        ));
        for i in 2..=19 {
            sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
        }
        sections.push(section(
            "sec_0020",
            vec![entity("Alyosha", &[], "")],
            vec![],
        ));
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            1,
            "substring match with shorter ≥ 5 chars must cross any section distance"
        );
        assert!(out.entities[0]
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case("Alyosha")));
    }

    #[tokio::test]
    async fn atlas_resolve_rule_4_respects_whole_word_boundary() {
        // `Ivan` is a substring of `Ivanovich` byte-wise but not a
        // whole-word match. Rule 4 must honour word boundaries so
        // it doesn't false-merge a short-named character into a
        // longer patronymic.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity("Ivanovich", &[], "Distinct character A.")],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Ivan", &[], "Distinct character B.")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        // Though "Ivan" is 4 chars (below the 5-char floor anyway),
        // this test also pins the behavior should the floor ever
        // drop: "Ivan" would still not substring-merge into
        // "Ivanovich" because of whole-word guard.
        assert_eq!(out.entities.len(), 2);
    }

    #[tokio::test]
    async fn atlas_resolve_rule_4_merges_title_prefix_names() {
        // "Zossima" vs "Father Zossima" — zossima (7 chars, len ≥ 5)
        // is a whole-word substring of "father zossima". Real
        // smoke-test case: Phase 1 extracted both forms from
        // different chapters and they fragmented before rule 4.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity("Zossima", &[], "Monastery elder.")],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Father Zossima", &[], "The elder blesses Alyosha.")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(out.entities.len(), 1);
    }

    #[tokio::test]
    async fn rule_3_5_merges_fyodor_drift_variants_via_single_long_token_and_high_cosine() {
        // The Landing 4 smoke-test residual: "Fyodor Pavlovich
        // Karamazov" and "Fyodor Karazov" describe the same
        // patriarch but only share one long token (`fyodor`) after
        // fold — `karazov` ↔ `karamazov` is Lev 2, above rule 3's
        // fuzzy cap. Rule 3.5 catches this when description cosine
        // ≥ 0.92. We use `always_one_embed` to force cosine = 1.0.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Fyodor Pavlovich Karamazov",
                    &[],
                    "The Karamazov patriarch.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity(
                    "Fyodor Karazov",
                    &[],
                    "The Karamazov patriarch.",
                )],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &always_one_embed())
            .await
            .unwrap();
        assert_eq!(
            out.entities.len(),
            1,
            "Rule 3.5 should merge Fyodor drift variants on single-token + cosine"
        );
    }

    #[tokio::test]
    async fn rule_3_5_blocks_sibling_collapse_on_single_shared_surname() {
        // Alexei and Dmitri share only `karamazov` (surname) as a
        // long token; first_token_matches fails so rule 3.5 does
        // NOT fire even with identical descriptions. Protects the
        // sibling-distinction invariant.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Alexei Karamazov",
                    &[],
                    "A Karamazov brother.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Dmitri Karamazov", &[], "A Karamazov brother.")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &always_one_embed())
            .await
            .unwrap();
        assert_eq!(
            out.entities.len(),
            2,
            "Rule 3.5 must not collapse siblings sharing only surname + cosine"
        );
    }

    #[tokio::test]
    async fn rule_3_5_sparse_path_merges_drift_variant_with_empty_description() {
        // The real-world residual from the Landing 4 smoke: the
        // sparse Fyodor drift has an empty description. Rule 3.5's
        // sparse path fires when first_token_matches + ≥ 1 shared
        // long token + exactly one side has no description.
        // first_token_matches still guards against sibling
        // collapse.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Fyodor Pavlovich Karamazov",
                    &[],
                    "The Karamazov patriarch, wealthy landowner and provocateur.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                // Empty description — the actual condition that
                // tripped up rule 3.5's strict path in Landing 4.
                vec![entity("Fyodor Karazov", &[], "")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &always_one_embed())
            .await
            .unwrap();
        assert_eq!(
            out.entities.len(),
            1,
            "sparse drift variant must merge via rule 3.5 sparse path"
        );
    }

    #[tokio::test]
    async fn rule_3_5_sparse_path_respects_first_token_guard() {
        // Same empty-description case but different first names —
        // rule 3.5 sparse path must NOT fire. Protects against
        // "Alexei Karamazov" vs bare "Dmitri" (or anyone else with
        // a sparse reference sharing only the surname).
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Alexei Karamazov",
                    &[],
                    "The youngest brother.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity("Dmitri Karamazov", &[], "")],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &always_one_embed())
            .await
            .unwrap();
        assert_eq!(
            out.entities.len(),
            2,
            "sparse path must not bypass the first_token_matches guard"
        );
    }

    #[tokio::test]
    async fn rule_3_5_requires_high_cosine_even_with_shared_first_token() {
        // Same first token, same single long token, but DIFFERENT
        // descriptions → cosine below 0.92 → no merge. Protects
        // "two distinct Fyodors" who happen to share a surname-like
        // token.
        // `fake_embed` derives vectors from first bytes; crafting
        // descriptions that start with different characters makes
        // the cosine land well below 0.92.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity(
                    "Fyodor Pavlovich Karamazov",
                    &[],
                    "A drunkard patriarch known for his vice and cunning.",
                )],
                vec![],
            ),
            section(
                "sec_0002",
                vec![entity(
                    "Fyodor Karazov",
                    &[],
                    "Different saintly healer, helps pilgrims find their way.",
                )],
                vec![],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            2,
            "different descriptions must keep the two Fyodors distinct"
        );
    }

    #[tokio::test]
    async fn atlas_resolve_cross_window_merges_when_alias_evidence_fires() {
        // Same setup as the boundary test but the later sketch
        // carries the earlier entity's name in its aliases — rule 1
        // crosses any distance.
        let mut sections = Vec::new();
        sections.push(section(
            "sec_0001",
            vec![entity(
                "Alexei Fyodorovich Karamazov",
                &[],
                "Youngest brother.",
            )],
            vec![],
        ));
        for i in 2..=9 {
            sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
        }
        sections.push(section(
            "sec_0010",
            vec![entity(
                "Alyosha",
                &["Alexei Fyodorovich Karamazov"],
                "The novice.",
            )],
            vec![],
        ));
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(
            out.entities.len(),
            1,
            "alias match should merge across any section distance"
        );
    }

    #[tokio::test]
    async fn atlas_events_dedupe_within_adjacent_sections() {
        // Same event described near-identically in adjacent sections
        // merges into a single event atom with both evidence refs.
        let sections = vec![
            section(
                "sec_0001",
                vec![entity("Alyosha", &[], "")],
                vec![event(
                    "Alyosha arrives at the monastery.",
                    &["Alyosha"],
                )],
            ),
            section(
                "sec_0002",
                vec![],
                vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
            ),
        ];
        let out = resolve_entities_and_events(&sections, &always_one_embed()).await.unwrap();
        assert_eq!(
            out.events.len(),
            1,
            "high-similarity events in adjacent sections should dedupe"
        );
        assert_eq!(out.events[0].evidence.len(), 2);
    }

    #[tokio::test]
    async fn atlas_events_stay_distinct_across_wide_section_gaps() {
        // The same event narrated again 10 sections later is worth
        // preserving — it's narrative repetition, not noise.
        let mut sections = Vec::new();
        sections.push(section(
            "sec_0001",
            vec![entity("Alyosha", &[], "")],
            vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
        ));
        for i in 2..=10 {
            sections.push(section(&format!("sec_{i:04}"), vec![], vec![]));
        }
        sections.push(section(
            "sec_0011",
            vec![],
            vec![event("Alyosha arrives at the monastery.", &["Alyosha"])],
        ));
        let out = resolve_entities_and_events(&sections, &always_one_embed()).await.unwrap();
        assert_eq!(
            out.events.len(),
            2,
            "events beyond the ±2-section dedupe window stay distinct"
        );
    }

    #[tokio::test]
    async fn atlas_resolve_emits_involves_edges_for_event_participants() {
        let sections = vec![section(
            "sec_0001",
            vec![entity("Alyosha", &[], ""), entity("Zosima", &[], "")],
            vec![event(
                "Zosima instructs Alyosha.",
                &["Zosima", "Alyosha"],
            )],
        )];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(out.events.len(), 1);
        // One Involves edge per participant, source = event id.
        let involves: Vec<&Edge> = out
            .edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Involves)
            .collect();
        assert_eq!(involves.len(), 2);
        let src = &out.events[0].id;
        assert!(involves.iter().all(|e| e.source == *src));
        assert_eq!(involves[0].provenance, EdgeProvenance::Derived);
    }

    #[tokio::test]
    async fn atlas_resolve_salience_is_frequency_normalised() {
        // Alyosha appears in 3 sections, Zosima in 1. Salience is
        // a monotonic function of reference count; Alyosha > Zosima.
        let sections = vec![
            section("sec_0001", vec![entity("Alyosha", &[], ""), entity("Zosima", &[], "")], vec![]),
            section("sec_0002", vec![entity("Alyosha", &[], "")], vec![]),
            section("sec_0003", vec![entity("Alyosha", &[], "")], vec![]),
        ];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        let a = out
            .entities
            .iter()
            .find(|e| e.canonical_name == "Alyosha")
            .unwrap();
        let z = out
            .entities
            .iter()
            .find(|e| e.canonical_name == "Zosima")
            .unwrap();
        assert!(a.salience > z.salience);
        assert!((a.salience - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn atlas_resolve_drops_orphan_event_participants() {
        // Participant name not matching any introduced entity is
        // dropped from the event's participant list (and no
        // Involves edge is emitted for it). The event still lands.
        let sections = vec![section(
            "sec_0001",
            vec![entity("Alyosha", &[], "")],
            vec![event(
                "Alyosha and some Stranger meet.",
                &["Alyosha", "Stranger"],
            )],
        )];
        let out = resolve_entities_and_events(&sections, &fake_embed()).await.unwrap();
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.events[0].participants.len(), 1);
        assert_eq!(
            out.events[0].participants[0].as_str(),
            "entity-0001"
        );
        assert_eq!(out.edges.len(), 1);
    }

    #[test]
    fn levenshtein_matches_expected_values() {
        // Pin the DP against well-known textbook pairs plus a few
        // names that drive resolution rule 2.
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        // ivan → ilya: i stays, v→l, a→y, n→a = 3 edits.
        assert_eq!(levenshtein("ivan", "ilya"), 3);
        // "ivan" vs "iván" (transliteration with accented character)
        // would be 1 — we keep rule 2's cap at 2 to leave headroom.
        assert_eq!(levenshtein("ivan", "ivn"), 1);
    }

    #[test]
    fn step_3b_resolves_state_relation_claim_question_atoms_from_sketches() {
        use crate::enrichment::pipeline::atlas::{
            ClaimSketch, DiscourseAct, EnrichmentDepth, EntitySketch, EntityStateSketch,
            EntityType, EpistemicStatus, QuestionSketch, RelationSketch, RelationStateSketch,
        };
        use super::super::atoms::{AtomId, Entity, ChunkRef};

        // Build two canonical entities from (simulated) Step 3a.
        let entities = vec![
            Entity {
                id: AtomId::entity(1),
                canonical_name: "Alyosha".into(),
                aliases: vec!["Alexei Fyodorovich".into()],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "Youngest Karamazov.".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            Entity {
                id: AtomId::entity(2),
                canonical_name: "Zossima".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "Monastery elder.".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
        ];

        // Two sections, in order. sec_0001 introduces a state +
        // relation + claim + question; sec_0002 develops the state
        // further (so Transition edges fire).
        let sections = vec![
            SectionExtraction {
                section_id: "sec_0001".into(),
                enrichment_depth: EnrichmentDepth::Extracted,
                entities_introduced: vec![
                    EntitySketch {
                        canonical_name: "Alyosha".into(),
                        aliases: vec![],
                        entity_type: EntityType::Person,
                        description: "".into(),
                        anchor: String::new(),
                    },
                ],
                entities_developed: vec![EntityStateSketch {
                    entity_name: "Alyosha".into(),
                    label: "Eager attention at the elder's feet".into(),
                    anchor: "knelt at Zossima's feet".into(),
                }],
                relations_introduced: vec![RelationSketch {
                    participants: vec!["Alyosha".into(), "Zossima".into()],
                    label: "Novice-elder bond".into(),
                    anchor: "laid his hand on Alyosha's head".into(),
                }],
                relations_developed: vec![RelationStateSketch {
                    participants: vec!["Alyosha".into(), "Zossima".into()],
                    label: "Formation through blessing".into(),
                    anchor: "blessed the novice".into(),
                }],
                events: vec![],
                claims: vec![ClaimSketch {
                    content: "Active love costs more than dreamt love.".into(),
                    discourse_act: DiscourseAct::Argue,
                    epistemic_status: EpistemicStatus::Confident,
                    attributed_to: Some("Zossima".into()),
                    anchor: "love in dreams is greedy".into(),
                }],
                questions_raised: vec![QuestionSketch {
                    content: "Can a faith formed in the cell survive the world?".into(),
                    anchor: "faith in the cell".into(),
                }],
            },
            SectionExtraction {
                section_id: "sec_0002".into(),
                enrichment_depth: EnrichmentDepth::Extracted,
                entities_developed: vec![EntityStateSketch {
                    entity_name: "Alyosha".into(),
                    label: "Resolve to leave the monastery".into(),
                    anchor: "must go out into the world".into(),
                }],
                ..Default::default()
            },
        ];

        let out = resolve_step_3b(&sections, &entities, &[]).unwrap();

        // 2 states from entities_developed (Alyosha × 2) + 1 state
        // from relations_developed (Alyosha↔Zossima) = 3 states.
        assert_eq!(out.states.len(), 3);
        // 1 Relation introduced.
        assert_eq!(out.relations.len(), 1);
        assert_eq!(out.relations[0].participants.len(), 2);
        // 1 Claim, attributed to entity-0002 (Zossima).
        assert_eq!(out.claims.len(), 1);
        assert_eq!(out.claims[0].attributed_to, Some(AtomId::entity(2)));
        // 1 Question, resolution_status defaults to Open.
        assert_eq!(out.questions.len(), 1);
        assert!(matches!(
            out.questions[0].resolution_status,
            super::super::atoms::ResolutionStatus::Open
        ));

        // Trajectory index carries Alyosha (2 states → 1 transition)
        // and the Alyosha↔Zossima relation (1 state → 0 transitions).
        let alyosha_traj = out
            .trajectories
            .get(AtomId::entity(1).as_str())
            .expect("Alyosha trajectory");
        assert_eq!(alyosha_traj.atom_type, "Entity");
        assert_eq!(alyosha_traj.states.len(), 2);
        assert_eq!(alyosha_traj.transitions.len(), 1);
        // States ordered by section (sec_0001 before sec_0002).
        assert_eq!(alyosha_traj.states[0].section_range.start, "sec_0001");
        assert_eq!(alyosha_traj.states[1].section_range.start, "sec_0002");

        let relation_id = &out.relations[0].id;
        let relation_traj = out
            .trajectories
            .get(relation_id.as_str())
            .expect("relation trajectory");
        assert_eq!(relation_traj.atom_type, "Relation");
        assert_eq!(relation_traj.states.len(), 1);

        // Edges: 2 Involves (state→Alyosha) + 2 Grounds (state
        // evidence) + 2 Involves (relation→each participant) + 1
        // Involves (state→relation) + 1 Grounds (relation state
        // evidence) + 1 Involves (claim→Zossima) + 1 Grounds
        // (claim evidence) + 1 Transition (Alyosha state chain).
        // Exact count is 11 for this fixture; pin the categories
        // rather than the total so a future reorder of edge
        // emission doesn't break the test.
        let count = |t: EdgeType| out.edges.iter().filter(|e| e.edge_type == t).count();
        assert!(count(EdgeType::Involves) >= 5); // state→entity×2, rel→participant×2, claim→attributed_to
        assert!(count(EdgeType::Grounds) >= 3); // state evidence + claim evidence
        assert_eq!(count(EdgeType::Transition), 1);
    }

    #[test]
    fn step_3b_drops_sketches_with_unknown_entity_names() {
        use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityStateSketch};

        let entities = vec![super::super::atoms::Entity {
            id: super::super::atoms::AtomId::entity(1),
            canonical_name: "Alyosha".into(),
            aliases: Vec::new(),
            entity_type:
                crate::enrichment::pipeline::atlas::EntityType::Person,
            first_appearance: super::super::atoms::ChunkRef::new("sec_0001", None),
            description: "x".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
        }];

        let sections = vec![SectionExtraction {
            section_id: "sec_0001".into(),
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_developed: vec![
                EntityStateSketch {
                    entity_name: "Alyosha".into(),
                    label: "real state".into(),
                    anchor: String::new(),
                },
                EntityStateSketch {
                    entity_name: "Mystery Character".into(),
                    label: "orphan state".into(),
                    anchor: String::new(),
                },
            ],
            ..Default::default()
        }];

        let out = resolve_step_3b(&sections, &entities, &[]).unwrap();
        // Only the resolvable state lands; the orphan drops.
        assert_eq!(out.states.len(), 1);
        assert_eq!(out.states[0].label, "real state");
    }

    #[test]
    fn fold_strips_latin_combining_diacritics_for_folded_lookup() {
        // Observed in the Landing 2 smoke test: the model emits
        // decorated Latin forms like `Karámazov`, `Fyódor Pávlovič`,
        // `Miüsov`. NFD decomposes the precomposed diacritic char
        // and we drop the combining mark, leaving plain Latin.
        // This makes fold idempotent regardless of which mixture of
        // diacritics the model chose this call.
        assert_eq!(fold("Karámazov"), "karamazov");
        assert_eq!(fold("Fyódor Pávlovič"), "fyodor pavlovic");
        assert_eq!(fold("Miüsov"), "miusov");
        // Mixed Cyrillic + Latin-diacritic case (realistic drift):
        // Cyrillic `а` transliterates to Latin `a`, the á loses
        // its acute. Final form matches the clean canonical.
        assert_eq!(fold("Karámázов"), "karamazov");
        // Already-plain input passes through byte-for-byte.
        assert_eq!(fold("Karamazov"), "karamazov");
    }

    #[test]
    fn transliterate_cyrillic_maps_russian_to_english_form() {
        // The canonical Brothers Karamazov cases that the raw
        // resolver misses: mid-word Cyrillic chars in a Latin
        // transliteration should fold to the same form.
        assert_eq!(transliterate_cyrillic("karamазов"), "karamazov");
        assert_eq!(transliterate_cyrillic("adelаida"), "adelaida");
        assert_eq!(transliterate_cyrillic("mityа"), "mitya");
        // Already-Latin strings pass through unchanged.
        assert_eq!(transliterate_cyrillic("karamazov"), "karamazov");
        // Pure Russian spelling transliterates to the English form.
        assert_eq!(transliterate_cyrillic("карамазов"), "karamazov");
        // Non-Cyrillic passthrough preserves spaces / punctuation.
        assert_eq!(
            transliterate_cyrillic("fyodor pavlovich karamazov"),
            "fyodor pavlovich karamazov"
        );
    }

    #[test]
    fn shared_token_overlap_merges_mixed_encoding_tokens() {
        // The real-world bug Phase 3a hit: `Karamазов` with a few
        // Cyrillic chars mid-word vs `Karamazov` pure Latin. After
        // transliteration both fold to `karamazov` and count as 1
        // shared token, so with a matching first name ("Fyodor")
        // rule 3 fires (≥2 shared tokens).
        assert_eq!(
            shared_token_overlap(
                "Fyodor Karamазов",
                "Fyodor Pavlovich Karamazov"
            ),
            2
        );
    }

    #[test]
    fn first_token_matches_allows_exact_and_fuzzy_firstname_drift() {
        assert!(first_token_matches("Fyodor Karamazov", "Fyodor Pavlovitch"));
        assert!(first_token_matches(
            "Alyosha Karamazov",
            "Alyoshá Karámázov"
        )); // diacritic strip folds both first tokens
        assert!(first_token_matches("Alexey Karamazov", "Alexei Karamazov")); // Lev 1
        assert!(!first_token_matches(
            "Alexei Fyodorovic Karamazov",
            "Dmitri Fyodorovic Karamazov"
        )); // different first names must not match
        assert!(!first_token_matches("Ivan", "Ilya")); // 4-char guard — below fuzzy floor
    }

    // ── Landing 5 — Rule 3.5 single-long-token + cosine ─────

    #[test]
    fn shared_long_token_count_only_counts_tokens_of_minimum_length() {
        // `the`, `and` — below FUZZY_TOKEN_MIN_LEN=5, don't count.
        // Long tokens do.
        assert_eq!(
            shared_long_token_count("The Fyodor Karamazov", "The Fyodor Bank"),
            1 // only "fyodor" is long enough AND shared
        );
        // Two long shared tokens: "alexei" (6) + "karamazov" (9).
        assert_eq!(
            shared_long_token_count(
                "Alexei Fyodorovich Karamazov",
                "Alexei Petrovich Karamazov"
            ),
            2
        );
        // Single shared long token: the Fyodor drift case.
        assert_eq!(
            shared_long_token_count(
                "Fyodor Pavlovich Karamazov",
                "Fyodor Karazov"
            ),
            1
        );
        // No long tokens shared — all short tokens would be
        // filtered out.
        assert_eq!(
            shared_long_token_count("the end", "the top"),
            0
        );
    }

    #[test]
    fn rule_3_requires_first_token_match_to_prevent_sibling_collapse() {
        // Two siblings share patronymic + surname exactly (2 shared
        // tokens, above the rule 3 threshold) but differ in first
        // name. Pre-guard, shared_token_overlap alone would have
        // merged them — the first-token-matches guard blocks this.
        // Inputs are real Brothers Karamazov names so future
        // regressions on this specific case surface loudly.
        assert_eq!(
            shared_token_overlap(
                "Alexei Fyodorovich Karamazov",
                "Dmitri Fyodorovich Karamazov"
            ),
            2
        );
        assert!(!first_token_matches(
            "Alexei Fyodorovich Karamazov",
            "Dmitri Fyodorovich Karamazov"
        ));
    }

    #[test]
    fn shared_token_overlap_counts_lev1_fuzzy_matches_on_long_tokens() {
        // Landing 2 observation: Phase 3a rule 3 was failing to
        // merge `Zossima` ↔ `Elder Zósima` (one-char drop after
        // diacritic strip) because exact-token overlap = 0 even
        // though every shared token is one edit away. Fuzzy pass
        // at Lev ≤ 1 for tokens ≥ 5 chars catches this while
        // staying conservative enough to keep distinct entities
        // distinct (below).
        assert_eq!(
            shared_token_overlap("Zossima", "Elder Zósima"),
            1,
            "zossima ↔ zosima (Lev 1) should count as a fuzzy match"
        );
        assert_eq!(
            shared_token_overlap(
                "Ivan Fyodoroič Kárámazov",
                "Iván Fyódorič Kárazòv"
            ),
            // After fold + Lev-1 fuzzy:
            //   ivan (4 chars) — filtered by min-len 3 but below
            //   fuzzy guard 5, so must match exactly → does match.
            //   fyodoroic ↔ fyodoric (Lev 1) → fuzzy match.
            //   karamazov ↔ karazov (Lev 2) → NOT fuzzy at Lev 1.
            // Total: ivan + fyodoric = 2. Rule 3 (≥ 2) would fire.
            2,
        );
        // Distinct entities must stay apart — only one shared
        // long-token, no fuzzy headroom.
        assert_eq!(
            shared_token_overlap("Ivan Karamazov", "Ilya Karamazov"),
            1,
        );
    }

    #[test]
    fn shared_token_overlap_ignores_short_tokens() {
        // "of the house" vs "the house of" — tokens of length ≥ 3
        // are "the" and "house"; both appear in both strings.
        // Overlap = 2. Tokens of length < 3 ("of") are filtered.
        assert_eq!(shared_token_overlap("of the house", "the house of"), 2);
        // Russian patronymic: 2 long-enough tokens share.
        assert_eq!(
            shared_token_overlap(
                "alexei fyodorovich karamazov",
                "alexei fyodorovich"
            ),
            2
        );
        // Disjoint tokens share none.
        assert_eq!(shared_token_overlap("ivan karamazov", "alexei smerdyakov"), 0);
    }

    // ── Landing 2.A — fuzzy participant snap fallbacks ────────

    /// Build a minimal resolved-entity set for fuzzy-lookup tests.
    /// Mirrors what Step 3a would produce in the real pipeline.
    fn fuzzy_fixture_entities() -> Vec<super::super::atoms::Entity> {
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        vec![
            Entity {
                id: AtomId::entity(1),
                canonical_name: "Fyodor Fyodorovitch Karamazoff".into(),
                aliases: vec!["Fyodor".into(), "Karamazoff".into()],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "Patriarch.".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            Entity {
                id: AtomId::entity(2),
                canonical_name: "Alexei Fyedorovitch Kramzof".into(),
                aliases: vec!["Alyosha".into(), "Alexei".into()],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "Youngest son.".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            Entity {
                id: AtomId::entity(3),
                canonical_name: "Sofya Ivanovna Karamzova".into(),
                aliases: vec!["Sofya".into()],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0003", None),
                description: "Second wife.".into(),
                salience: 0.5,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
        ]
    }

    #[test]
    fn resolve_entity_id_fuzzy_snaps_cyrillic_mangled_participant_to_canonical() {
        // Observed in the Landing 1 smoke test: relation participant
        // strings leak mid-word Cyrillic chars (`Sofya Ivаnovna
        // Karаmzova` — а is Cyrillic) even when the entity atom was
        // resolved from the clean Latin form. After `fold` applies
        // `transliterate_cyrillic` both sides collapse to the same
        // folded key and the exact-match path takes it — no
        // Levenshtein needed. Lock this behaviour.
        let entities = fuzzy_fixture_entities();
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        let mangled = "Sofya Ivаnovna Karаmzova"; // Cyrillic а in two places
        let id = resolve_entity_id_fuzzy(mangled, &name_index, &token_index)
            .expect("cyrillic-mangled form should snap to Sofya entity");
        assert_eq!(id.as_str(), "entity-0003");
    }

    #[test]
    fn resolve_entity_id_fuzzy_snaps_levenshtein_within_two_after_translit() {
        // Isolate the vote fallback: `Karazoff` is not a canonical,
        // alias, or token-index key (the real forms are `Karamazoff`
        // with an extra `m-a`). Exact-match fallbacks (1) and (2)
        // both miss. Lev(`karazoff`, `karamazoff`) = 2 and
        // `karamazoff` appears in exactly one entity's tokens, so
        // the vote fallback lands on that entity.
        let entities = fuzzy_fixture_entities();
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        let drifted = "Karazoff";
        let id = resolve_entity_id_fuzzy(drifted, &name_index, &token_index)
            .expect("Karazoff ↔ Karamazoff within Levenshtein-2 should snap via vote fallback");
        assert_eq!(id.as_str(), "entity-0001");
    }

    #[test]
    fn resolve_entity_id_fuzzy_snap_is_robust_to_mixed_drift_with_one_clean_token() {
        // Realistic smoke-test form: `Fyodor Pvlvitch Karazoff`.
        // The `Fyodor` token short-circuits via fallback 2 (appears
        // in exactly one entity's tokens). The point of this test
        // is to assert the call resolves regardless of which
        // fallback path takes it — so if we later tighten the
        // long-token fallback, the vote path would catch it.
        let entities = fuzzy_fixture_entities();
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        let id = resolve_entity_id_fuzzy(
            "Fyodor Pvlvitch Karazoff",
            &name_index,
            &token_index,
        )
        .expect("Fyodor + Karazoff should resolve to entity-0001");
        assert_eq!(id.as_str(), "entity-0001");
    }

    #[test]
    fn resolve_entity_id_fuzzy_refuses_to_snap_on_multi_match_ambiguity() {
        // When a query token sits within Levenshtein 2 of tokens
        // belonging to two distinct entities, the vote for that
        // token is ambiguous and must not count. If no query token
        // produces a single-entity vote, the lookup returns None.
        // Construct two entities whose long tokens collide under
        // Lev 2 so the fallback cannot disambiguate.
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        let entities = vec![
            Entity {
                id: AtomId::entity(1),
                canonical_name: "Marina".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            Entity {
                id: AtomId::entity(2),
                canonical_name: "Marika".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
        ];
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        // `Marnka` is Lev-1 from `Marina` (replace i→n wait no, marna
        // vs marina... let me recompute). Take `Marinka` — Lev-1
        // from `Marina` (insert k) AND Lev-1 from `Marika` (insert n).
        let ambiguous = "Marinka";
        assert_eq!(levenshtein("marinka", "marina"), 1);
        assert_eq!(levenshtein("marinka", "marika"), 1);
        assert!(
            resolve_entity_id_fuzzy(ambiguous, &name_index, &token_index).is_none(),
            "ambiguous Lev-1 match against 2 entities must not snap"
        );
    }

    #[test]
    fn resolve_entity_id_fuzzy_respects_min_token_length_guard() {
        // `Ivan` (4 chars) is below FUZZY_TOKEN_MIN_LEN=5 so the
        // vote fallback cannot consider it — this is the guard
        // that prevents `Ivan` ↔ `Ilya` (Lev 3, would be rejected
        // anyway) or `Anna` ↔ `Anka` style near-misses from
        // collapsing short ambiguous names.
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        let entities = vec![Entity {
            id: AtomId::entity(1),
            canonical_name: "Anna".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
        }];
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        // `Anka` is Lev-1 from `Anna` but both are 4 chars. Guard
        // must keep them distinct.
        assert!(
            resolve_entity_id_fuzzy("Anka", &name_index, &token_index).is_none(),
            "4-char tokens are below the fuzzy floor and must not snap"
        );
    }

    // ── Landing 4.B — salience-aware attribution resolver ─────

    fn high_salience_fyodor() -> super::super::atoms::Entity {
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        // Aliases are the richer full-name forms Phase 3a merges
        // together — NOT the bare first name. Real-world smoke
        // data from brothers_karamazov showed no entity had
        // "Fyodor" as a standalone alias; every alias was a full
        // patronymic form. Mirror that here so the strict fuzzy
        // resolver actually has to bail on "Fyodor" alone.
        Entity {
            id: AtomId::entity(1),
            canonical_name: "Fyodor Pavlovich Karamazov".into(),
            aliases: vec!["Fyodor Pavlovitch Karamazov".into()],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "Patriarch.".into(),
            salience: 1.0,
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn low_salience_fyodor_drift() -> super::super::atoms::Entity {
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        Entity {
            id: AtomId::entity(2),
            canonical_name: "Fyodor Karazov".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0004", None),
            description: "Variant.".into(),
            salience: 0.2,
            enrichment_depth: EnrichmentDepth::Extracted,
        }
    }

    fn two_contested_ivans() -> (super::super::atoms::Entity, super::super::atoms::Entity) {
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        let a = Entity {
            id: AtomId::entity(1),
            canonical_name: "Ivan Karamazov".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0001", None),
            description: "Brother.".into(),
            salience: 0.8,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        let b = Entity {
            id: AtomId::entity(2),
            canonical_name: "Ivan Petrovich Sidorov".into(),
            aliases: vec![],
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("sec_0002", None),
            description: "Different Ivan.".into(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::Extracted,
        };
        (a, b)
    }

    #[test]
    fn salience_resolver_snaps_to_dominant_when_strict_bails_on_ambiguity() {
        // Two entities share the token "fyodor" — the strict
        // resolver refuses to guess (single-match-wins fails). The
        // salience fallback picks the father (salience 1.0) over
        // the drift variant (salience 0.2) because 1.0 > 2.0 × 0.2.
        let entities = vec![high_salience_fyodor(), low_salience_fyodor_drift()];
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        // "Fyodor" alone is ambiguous — both entities' first tokens
        // fold to "fyodor" and both have "fyodor" as a long token.
        assert!(resolve_entity_id_fuzzy("Fyodor", &name_index, &token_index).is_none());
        let id = resolve_entity_id_with_salience(
            "Fyodor",
            &entities,
            &name_index,
            &token_index,
        )
        .expect("salience-aware fallback should snap to dominant");
        assert_eq!(id.as_str(), "entity-0001");
    }

    #[test]
    fn salience_resolver_bails_when_candidates_are_comparable() {
        // Two Ivans with comparable salience (0.8 vs 0.7 → ratio
        // 1.14, below SALIENCE_DOMINANCE_FACTOR=2.0). Build
        // canonicals that DON'T exact-match the query so the
        // strict resolver is forced to bail and the salience
        // fallback is actually exercised. Both canonicals share
        // "ivan" and "karamazov" as tokens.
        use super::super::atoms::{AtomId, ChunkRef, Entity};
        let contested = vec![
            Entity {
                id: AtomId::entity(1),
                canonical_name: "Ivan Fyodorovich Karamazov".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "".into(),
                salience: 0.8,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            Entity {
                id: AtomId::entity(2),
                canonical_name: "Ivan Petrovich Karamazov".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "".into(),
                salience: 0.7,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
        ];
        let name_index = build_name_index(&contested);
        let token_index = build_token_index(&contested);
        // Query shares "karamazov" token with both entities AND
        // first token "ivan" with both. Strict resolver bails
        // (karamazov appears in 2 entities → ambiguous). Salience
        // 0.8 vs 0.7 → 0.8 < 2.0 × 0.7 → fallback also bails.
        assert!(
            resolve_entity_id_with_salience(
                "Ivan Unknownovich Karamazov",
                &contested,
                &name_index,
                &token_index,
            )
            .is_none(),
            "comparable salience must not snap — the resolver stays silent"
        );
    }

    #[test]
    fn salience_resolver_respects_first_token_guard() {
        // A dominant-salience entity does NOT snap if its first
        // token differs from the query. A claim attributed to
        // "Ivan" must not land on Fyodor even though Fyodor is
        // the most-salient Karamazov.
        let fyodor = high_salience_fyodor();
        let (ivan, _) = two_contested_ivans();
        let entities = vec![fyodor, ivan];
        let name_index = build_name_index(&entities);
        let token_index = build_token_index(&entities);
        // "Ivan" (4 chars) is below min-token-length → None.
        assert!(resolve_entity_id_with_salience(
            "Ivan",
            &entities,
            &name_index,
            &token_index,
        )
        .is_none());
        // "Ivan Karamazov" shares "karamazov" with Fyodor but the
        // first_token_matches guard rejects Fyodor (fyodor ≠ ivan).
        // Ivan's canonical "Ivan Karamazov" matches perfectly via
        // the strict path — salience fallback isn't needed.
        let id = resolve_entity_id_with_salience(
            "Ivan Karamazov",
            &entities,
            &name_index,
            &token_index,
        )
        .expect("should snap to Ivan");
        assert_eq!(id.as_str(), "entity-0001"); // Ivan is entity-0001 in this fixture because he was listed first in two_contested_ivans
    }

    // ── Landing 2.C — relation_key dedup invariant ────────────

    #[test]
    fn relation_key_invariant_same_participants_different_labels_collapse_today() {
        // Pin the contract around the `relation_key`-based dedup at
        // line ~746: two RelationSketch inputs with the SAME resolved
        // participant set but DIFFERENT labels collapse to a single
        // Relation atom with whichever label arrived first. This
        // behaviour is deliberate — a fuzzy-snapped participant
        // list can collide two distinct-looking sketches onto the
        // same logical relation, and we want one atom, not two
        // duplicates with conflicting labels. When the fuzzy snap
        // becomes more permissive (Landing 2.A), this invariant
        // must still hold; if it ever changes the caller must
        // decide the policy deliberately rather than drifting.
        use crate::enrichment::pipeline::atlas::{
            EnrichmentDepth, RelationSketch,
        };
        use super::super::atoms::{AtomId, ChunkRef, Entity};

        let entities = vec![
            Entity {
                id: AtomId::entity(1),
                canonical_name: "Alyosha".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
            Entity {
                id: AtomId::entity(2),
                canonical_name: "Zossima".into(),
                aliases: vec![],
                entity_type: EntityType::Person,
                first_appearance: ChunkRef::new("sec_0001", None),
                description: "".into(),
                salience: 1.0,
                enrichment_depth: EnrichmentDepth::Extracted,
            },
        ];

        // Two sections each introducing a relation between the
        // same two participants with different prose labels.
        let sections = vec![
            SectionExtraction {
                section_id: "sec_0001".into(),
                enrichment_depth: EnrichmentDepth::Extracted,
                relations_introduced: vec![RelationSketch {
                    participants: vec!["Alyosha".into(), "Zossima".into()],
                    label: "Novice-elder bond".into(),
                    anchor: "knelt at the elder's feet".into(),
                }],
                ..Default::default()
            },
            SectionExtraction {
                section_id: "sec_0002".into(),
                enrichment_depth: EnrichmentDepth::Extracted,
                relations_introduced: vec![RelationSketch {
                    participants: vec!["Alyosha".into(), "Zossima".into()],
                    label: "Spiritual father-son".into(),
                    anchor: "blessed the novice".into(),
                }],
                ..Default::default()
            },
        ];

        let out = resolve_step_3b(&sections, &entities, &[]).unwrap();
        // Contract: one Relation atom, first label wins. Changing
        // this policy requires updating this test AND deciding the
        // merge strategy explicitly (e.g. keep both, concatenate
        // labels, promote to Interpreted depth with richer metadata).
        assert_eq!(
            out.relations.len(),
            1,
            "same-participants-different-labels must collapse to one atom \
             under the current dedup policy"
        );
        assert_eq!(out.relations[0].label, "Novice-elder bond");
    }

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
        use crate::enrichment::pipeline::atlas::{
            EnrichmentDepth, EntityStateSketch,
        };

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
        assert_eq!(traj.transitions[0].trigger_event.as_deref(), Some("event-0001"));
    }

    #[test]
    fn transition_trigger_stays_none_on_ambiguous_match() {
        // Two events in the window, both with Alyosha as participant
        // → we can't prove which is the trigger, so leave None rather
        // than pick one arbitrarily.
        use crate::enrichment::pipeline::atlas::{
            EnrichmentDepth, EntityStateSketch,
        };

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
            }],
            relations_introduced: vec![RelationSketch {
                participants: vec!["Alyosha".into(), "Unknown Person".into()], // (2) one unresolved
                label: "doomed partnership".into(),
                anchor: String::new(),
            }],
            relations_developed: vec![RelationStateSketch {
                participants: vec!["Ghost A".into(), "Ghost B".into()], // (3) both unresolved
                label: "phantom bond".into(),
                anchor: String::new(),
            }],
            claims: vec![ClaimSketch {
                content: "Faith is hard-won.".into(),
                discourse_act: DiscourseAct::Assert,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: Some("Someone Else".into()), // (4) unknown attribution
                anchor: String::new(),
            }],
            ..Default::default()
        }];

        let out = resolve_step_3b(&sections, &entities, &[]).unwrap();

        let kinds: Vec<PhaseFailureKind> =
            out.failures.iter().map(|f| f.kind).collect();
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
        assert!(out.failures.iter().any(|f| {
            f.subject.starts_with("sketch:entity_state:sec_0001#")
        }));
        assert!(out.failures.iter().any(|f| {
            f.subject.starts_with("sketch:relation_introduced:sec_0001#")
        }));
        assert!(out.failures.iter().any(|f| {
            f.subject.starts_with("sketch:relation_developed:sec_0001#")
        }));
        assert!(out.failures.iter().any(|f| {
            f.subject.starts_with("sketch:claim:sec_0001#")
        }));
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
            }],
            claims: vec![ClaimSketch {
                content: "Active love is harder than dreamt love.".into(),
                discourse_act: DiscourseAct::Argue,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: Some("Alyosha".into()),
                anchor: String::new(),
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
        use crate::enrichment::pipeline::atlas::{
            EnrichmentDepth, EntityStateSketch,
        };

        let entities = vec![
            single_entity(1, "Alyosha"),
            single_entity(2, "Ivan"),
        ];
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
                }],
                ..Default::default()
            },
        ];
        let out = resolve_step_3b(&sections, &entities, &events).unwrap();
        let alyosha_transitions: Vec<_> = out
            .edges
            .iter()
            .filter(|e| {
                e.edge_type == EdgeType::Transition
                    && e.source.as_str().starts_with("state-")
            })
            .collect();
        assert_eq!(alyosha_transitions.len(), 1);
        assert!(alyosha_transitions[0].trigger_event.is_none());
    }
}
