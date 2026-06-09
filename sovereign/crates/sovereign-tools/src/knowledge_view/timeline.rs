// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 3 — interaction-timeline assembly.
//!
//! Reads the atlas `atoms.json` + `edges.json` for a personal or
//! conversational corpus, joins the Involves edges' chunk_ids to
//! caller-supplied timestamps, and produces one
//! [`InteractionTimeline`] per entity that the relational and
//! strategic digests later format.
//!
//! The timeline is **computed on demand**, not stored — atoms and
//! edges are the source of truth, and timelines are a derived view.
//! That matches the requirements doc §3.4 ("not a stored data
//! structure — it is computed on demand from the atlas's atom and
//! edge graph").
//!
//! ATOS composition lives behind [`AtosLookup`]: for `Initiative`
//! entities, the assembler asks the caller "is there an ATOS project
//! or feature whose name matches this initiative?" The caller
//! resolves against the local FeatureStore + project state. When the
//! match is ambiguous or absent, the timeline's `atos_project` is
//! `None` — the digest renders the conversational context alone, no
//! "phase: n/a" filler.
//!
//! This module is intentionally pure: no SQLite handles, no async
//! I/O during assembly. The caller injects the chunk-timestamp
//! resolver as a closure, and the ATOS lookup as a small typed
//! interface. Tests exercise the assembly logic against in-memory
//! atoms without spinning up a state store.

use std::collections::BTreeMap;
use std::path::Path;

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, Entity};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType};
use corpus_engine::enrichment::atlas::writer::{read_atlas_atoms, read_atlas_edges, ATLAS_DIRNAME};
use corpus_engine::enrichment::pipeline::atlas::EntityType;

// ── Public types ────────────────────────────────────────────────

/// One row of the relational/strategic digest's source data.
#[derive(Debug, Clone)]
pub struct InteractionTimeline {
    pub entity_id: String,
    pub entity_name: String,
    pub entity_kind: TimelineEntityKind,
    /// Organisational affiliation for `Person` entities.
    pub affiliation: Option<String>,
    /// Role / title for `Person` entities.
    pub role: Option<String>,
    /// For `Initiative` entities — names (not IDs) of the resolved
    /// participant atoms, in the order the resolver emitted them.
    /// Names are surfaced in the digest because the user thinks of
    /// participants by name, not by atom id.
    pub participants: Vec<String>,
    /// Chronological list of mentions, oldest first.
    pub interactions: Vec<Interaction>,
    /// ATOS project / feature link for `Initiative` entities. Always
    /// `None` for Person and Organization.
    pub atos_project: Option<AtosLink>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineEntityKind {
    Person,
    Organization,
    Initiative,
}

/// One mention of an entity in the source corpus.
#[derive(Debug, Clone)]
pub struct Interaction {
    /// Unix epoch seconds, sourced via the caller's
    /// chunk-timestamp resolver. `None` when the resolver couldn't
    /// find the chunk_id (e.g. the source row was deleted between
    /// the enrichment run and the timeline assembly).
    pub timestamp: Option<i64>,
    pub source_chunk_id: String,
}

/// Composition surface for ATOS state. The assembler hands an
/// initiative's normalised name to the caller; the caller answers
/// with `Some(AtosLink)` when there's a confident match against a
/// project name (from `.sovereign/project.toml`) or a provisioned
/// feature id, and `None` otherwise.
///
/// Trait, not a struct, so callers can wire whatever ATOS reader
/// shape fits their context — sync against an in-memory ProjectState
/// + FeatureStore handle, async against a remote, or a stub in tests.
pub trait AtosLookup {
    fn lookup(&self, initiative_name_normalised: &str) -> Option<AtosLink>;
}

#[derive(Debug, Clone)]
pub struct AtosLink {
    pub kind: AtosLinkKind,
    /// For `Project` — the project name. For `Feature` — the
    /// feature id (e.g. "knowledge-view-relational").
    pub id: String,
    /// Current phase index, when the project / feature publishes one
    /// (1-based, from PHASES.md / feature_milestones).
    pub current_phase: Option<u32>,
    /// Total phases declared, when known.
    pub total_phases: Option<u32>,
    pub charter_status: CharterStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtosLinkKind {
    Project,
    Feature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharterStatus {
    /// Spec / charter on disk matches the last-approved hash.
    Clean,
    /// Spec / charter has drifted from the approved hash. Surfaces
    /// in the strategic digest with a "(drift)" annotation per
    /// requirements §4.3.
    Drifted,
    /// No approval baseline recorded — equivalent to Clean for
    /// digest purposes; the assembler returns this when the caller
    /// can't find an approval row.
    Unapproved,
}

/// A no-op lookup. Used when ATOS composition is intentionally
/// disabled (tests, headless servers without a project root).
pub struct NoAtosLookup;
impl AtosLookup for NoAtosLookup {
    fn lookup(&self, _: &str) -> Option<AtosLink> {
        None
    }
}

// ── Assembly ────────────────────────────────────────────────────

/// Read the atlas at `corpus_index_dir/atlas/` and assemble one
/// timeline per Person / Organization / Initiative entity. Skips
/// other `EntityType` variants (Concept, Work, Place, Other) — the
/// personal/conversational pipeline only emits the three relational
/// kinds, but a future `multi`-corpus could mix them.
///
/// Returns an empty vec when the atlas directory is absent — the
/// caller treats "no atoms yet" as "render the digest empty",
/// rather than an error. Genuine I/O errors (atlas exists but
/// atoms.json is corrupt) propagate.
pub fn assemble_timelines_from_atlas(
    corpus_index_dir: &Path,
    chunk_timestamp: &dyn Fn(&str) -> Option<i64>,
    atos: &dyn AtosLookup,
) -> std::io::Result<Vec<InteractionTimeline>> {
    let atlas_dir = corpus_index_dir.join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        return Ok(Vec::new());
    }
    let atoms_path = atlas_dir.join("atoms.json");
    let edges_path = atlas_dir.join("edges.json");
    if !atoms_path.exists() || !edges_path.exists() {
        return Ok(Vec::new());
    }
    let atoms_file = read_atlas_atoms(&atlas_dir)?;
    let edges_file = read_atlas_edges(&atlas_dir)?;
    Ok(assemble(
        &atoms_file.atoms,
        &edges_file.edges,
        chunk_timestamp,
        atos,
    ))
}

/// In-memory variant — the workhorse the file-based path delegates to.
/// Pure function: no I/O, no allocations beyond the result vec.
pub fn assemble(
    atoms: &[AtomEnvelope],
    edges: &[Edge],
    chunk_timestamp: &dyn Fn(&str) -> Option<i64>,
    atos: &dyn AtosLookup,
) -> Vec<InteractionTimeline> {
    // Index entities by atom id for participant-name lookup.
    let mut by_id: BTreeMap<String, &Entity> = BTreeMap::new();
    for atom in atoms {
        if let AtomEnvelope::Entity(e) = atom {
            by_id.insert(e.id.as_str().to_string(), e);
        }
    }

    // For each entity in the relational kinds, collect Involves
    // edges whose target points at it.
    let mut timelines: Vec<InteractionTimeline> = Vec::new();
    for atom in atoms {
        let AtomEnvelope::Entity(entity) = atom else {
            continue;
        };
        let Some(kind) = entity_kind(&entity.entity_type) else {
            continue;
        };

        // Walk edges. Involves edges from the entity-extraction
        // phase land with target = entity.id and source =
        // "chunk-<id>" (see entity_extraction.rs::merge_responses).
        let mut interactions: Vec<Interaction> = Vec::new();
        for edge in edges {
            if edge.edge_type != EdgeType::Involves {
                continue;
            }
            if edge.target.as_str() != entity.id.as_str() {
                continue;
            }
            // Prefer the edge's evidence ChunkRef — that's the
            // canonical chunk_id the resolver populated. Fall back
            // to stripping the "chunk-" prefix off the source atom
            // id when evidence is empty (older atlas runs).
            let chunk_id = if let Some(ev) = edge.evidence.first() {
                ev.chunk_id.clone()
            } else {
                edge.source
                    .as_str()
                    .strip_prefix("chunk-")
                    .unwrap_or(edge.source.as_str())
                    .to_string()
            };
            let ts = chunk_timestamp(&chunk_id);
            interactions.push(Interaction {
                timestamp: ts,
                source_chunk_id: chunk_id,
            });
        }

        // Sort oldest-first. None timestamps sink to the end so
        // they don't interleave with grounded interactions.
        interactions.sort_by(|a, b| match (a.timestamp, b.timestamp) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.source_chunk_id.cmp(&b.source_chunk_id),
        });

        // Resolve initiative participant atom ids → names.
        let participants: Vec<String> = entity
            .participants
            .iter()
            .filter_map(|aid| by_id.get(aid.as_str()).map(|e| e.canonical_name.clone()))
            .collect();

        // ATOS link — initiatives only.
        let atos_project = if matches!(kind, TimelineEntityKind::Initiative) {
            atos.lookup(&fold_name(&entity.canonical_name))
        } else {
            None
        };

        timelines.push(InteractionTimeline {
            entity_id: entity.id.as_str().to_string(),
            entity_name: entity.canonical_name.clone(),
            entity_kind: kind,
            affiliation: entity.affiliation.clone(),
            role: entity.role.clone(),
            participants,
            interactions,
            atos_project,
        });
    }

    timelines
}

fn entity_kind(t: &EntityType) -> Option<TimelineEntityKind> {
    match t {
        EntityType::Person => Some(TimelineEntityKind::Person),
        EntityType::Institution => Some(TimelineEntityKind::Organization),
        EntityType::Initiative => Some(TimelineEntityKind::Initiative),
        // Concept / Work / Place / Other don't participate in the
        // relational+strategic digest.
        _ => None,
    }
}

fn fold_name(s: &str) -> String {
    s.trim().to_lowercase()
}

// ── Convenience helpers ──────────────────────────────────────────

/// Most-recent timestamp across an entity's interactions, or `None`
/// when every interaction is ungrounded. Used by the digest's
/// recency-decay ranking — entities that have only ungrounded
/// mentions sink to the bottom.
pub fn last_seen_at(timeline: &InteractionTimeline) -> Option<i64> {
    timeline
        .interactions
        .iter()
        .filter_map(|i| i.timestamp)
        .max()
}

/// Count of grounded interactions within `[since, now]`. The digest
/// uses this for the frequency component of the ranking — see
/// requirements §4.2 / §4.3.
pub fn interactions_within(timeline: &InteractionTimeline, since: i64, now: i64) -> usize {
    timeline
        .interactions
        .iter()
        .filter_map(|i| i.timestamp)
        .filter(|t| *t >= since && *t <= now)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::enrichment::atlas::atoms::{AtomId, ChunkRef};
    use corpus_engine::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
    use corpus_engine::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType};

    fn person(idx: usize, name: &str, affiliation: Option<&str>, role: Option<&str>) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new("chunk-0".to_string(), None),
            description: String::new(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: affiliation.map(|s| s.into()),
            role: role.map(|s| s.into()),
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn org(idx: usize, name: &str) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: EntityType::Institution,
            first_appearance: ChunkRef::new("chunk-0".to_string(), None),
            description: String::new(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: None,
            role: None,
            participants: Vec::new(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn initiative(idx: usize, name: &str, participant_ids: &[AtomId]) -> Entity {
        Entity {
            id: AtomId::entity(idx),
            canonical_name: name.into(),
            aliases: Vec::new(),
            entity_type: EntityType::Initiative,
            first_appearance: ChunkRef::new("chunk-0".to_string(), None),
            description: String::new(),
            salience: 0.7,
            enrichment_depth: EnrichmentDepth::extracted_default(),
            affiliation: None,
            role: None,
            participants: participant_ids.to_vec(),
            defining_quote: None,
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }
    }

    fn involves_edge(idx: usize, target: &AtomId, chunk_id: &str) -> Edge {
        Edge {
            id: EdgeId::new(idx),
            edge_type: EdgeType::Involves,
            source: AtomId::from_raw(format!("chunk-{}", chunk_id)),
            target: target.clone(),
            evidence: vec![ChunkRef::new(chunk_id.to_string(), None)],
            trigger_event: None,
            sub_question: None,
            confidence: 1.0,
            provenance: EdgeProvenance::Derived,
        }
    }

    #[test]
    fn empty_atlas_produces_no_timelines() {
        let atoms: Vec<AtomEnvelope> = Vec::new();
        let edges: Vec<Edge> = Vec::new();
        let ts = |_id: &str| -> Option<i64> { None };
        let result = assemble(&atoms, &edges, &ts, &NoAtosLookup);
        assert!(result.is_empty());
    }

    #[test]
    fn assembly_produces_one_timeline_per_relational_entity() {
        let atoms = vec![
            AtomEnvelope::Entity(person(1, "Sarah Chen", Some("Acme"), Some("VP Eng"))),
            AtomEnvelope::Entity(org(2, "Acme Corp")),
            AtomEnvelope::Entity(initiative(
                3,
                "Q3 enterprise push",
                &[AtomId::entity(1), AtomId::entity(2)],
            )),
        ];
        let edges = vec![
            involves_edge(1, &AtomId::entity(1), "100"),
            involves_edge(2, &AtomId::entity(1), "200"),
            involves_edge(3, &AtomId::entity(2), "100"),
            involves_edge(4, &AtomId::entity(3), "200"),
        ];
        let ts = |id: &str| -> Option<i64> { id.parse::<i64>().ok() };
        let timelines = assemble(&atoms, &edges, &ts, &NoAtosLookup);

        assert_eq!(timelines.len(), 3);

        let sarah = timelines
            .iter()
            .find(|t| t.entity_kind == TimelineEntityKind::Person)
            .unwrap();
        assert_eq!(sarah.entity_name, "Sarah Chen");
        assert_eq!(sarah.affiliation.as_deref(), Some("Acme"));
        assert_eq!(sarah.role.as_deref(), Some("VP Eng"));
        assert_eq!(sarah.interactions.len(), 2);
        // Sorted oldest-first: 100 < 200.
        assert_eq!(sarah.interactions[0].timestamp, Some(100));
        assert_eq!(sarah.interactions[1].timestamp, Some(200));
        assert!(sarah.atos_project.is_none(), "people never get ATOS link");

        let init = timelines
            .iter()
            .find(|t| t.entity_kind == TimelineEntityKind::Initiative)
            .unwrap();
        assert_eq!(init.entity_name, "Q3 enterprise push");
        // Participant names resolved from atom ids.
        assert!(init.participants.contains(&"Sarah Chen".to_string()));
        assert!(init.participants.contains(&"Acme Corp".to_string()));
    }

    #[test]
    fn ungrounded_interactions_sort_to_the_end() {
        let atoms = vec![AtomEnvelope::Entity(person(1, "A", None, None))];
        let edges = vec![
            involves_edge(1, &AtomId::entity(1), "missing"),
            involves_edge(2, &AtomId::entity(1), "200"),
            involves_edge(3, &AtomId::entity(1), "100"),
        ];
        let ts = |id: &str| -> Option<i64> {
            if id == "missing" {
                None
            } else {
                id.parse::<i64>().ok()
            }
        };
        let timelines = assemble(&atoms, &edges, &ts, &NoAtosLookup);
        let inters = &timelines[0].interactions;
        assert_eq!(inters.len(), 3);
        assert_eq!(inters[0].timestamp, Some(100));
        assert_eq!(inters[1].timestamp, Some(200));
        assert_eq!(inters[2].timestamp, None);
    }

    #[test]
    fn last_seen_and_window_helpers_use_only_grounded_timestamps() {
        let mut tl = InteractionTimeline {
            entity_id: "x".into(),
            entity_name: "X".into(),
            entity_kind: TimelineEntityKind::Person,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            interactions: vec![
                Interaction {
                    timestamp: Some(100),
                    source_chunk_id: "a".into(),
                },
                Interaction {
                    timestamp: Some(500),
                    source_chunk_id: "b".into(),
                },
                Interaction {
                    timestamp: None,
                    source_chunk_id: "c".into(),
                },
            ],
            atos_project: None,
        };
        assert_eq!(last_seen_at(&tl), Some(500));
        assert_eq!(interactions_within(&tl, 0, 200), 1);
        assert_eq!(interactions_within(&tl, 0, 1000), 2);

        // No grounded — None.
        tl.interactions.retain(|i| i.timestamp.is_none());
        assert_eq!(last_seen_at(&tl), None);
        assert_eq!(interactions_within(&tl, 0, 1000), 0);
    }

    #[test]
    fn initiative_atos_link_consults_lookup_with_normalised_name() {
        struct StubLookup {
            calls: std::cell::RefCell<Vec<String>>,
        }
        impl AtosLookup for StubLookup {
            fn lookup(&self, name: &str) -> Option<AtosLink> {
                self.calls.borrow_mut().push(name.to_string());
                if name == "api migration" {
                    Some(AtosLink {
                        kind: AtosLinkKind::Project,
                        id: "api-migration".into(),
                        current_phase: Some(2),
                        total_phases: Some(4),
                        charter_status: CharterStatus::Drifted,
                    })
                } else {
                    None
                }
            }
        }
        let stub = StubLookup {
            calls: std::cell::RefCell::new(Vec::new()),
        };

        let atoms = vec![
            AtomEnvelope::Entity(initiative(1, "API Migration", &[])),
            AtomEnvelope::Entity(initiative(2, "Q3 Push", &[])),
        ];
        let edges: Vec<Edge> = Vec::new();
        let ts = |_id: &str| -> Option<i64> { None };
        let timelines = assemble(&atoms, &edges, &ts, &stub);

        // Both initiatives queried; the lookup folded the name to
        // lowercase before passing it.
        let calls = stub.calls.borrow();
        assert!(calls.contains(&"api migration".into()));
        assert!(calls.contains(&"q3 push".into()));

        let api = timelines
            .iter()
            .find(|t| t.entity_name == "API Migration")
            .unwrap();
        let link = api.atos_project.as_ref().unwrap();
        assert_eq!(link.id, "api-migration");
        assert_eq!(link.current_phase, Some(2));
        assert_eq!(link.charter_status, CharterStatus::Drifted);

        let q3 = timelines
            .iter()
            .find(|t| t.entity_name == "Q3 Push")
            .unwrap();
        assert!(q3.atos_project.is_none());
    }

    #[test]
    fn non_relational_entities_are_filtered_out() {
        // Concept / Work / Place atoms shouldn't surface in the
        // relational + strategic timelines. A Concept atom from a
        // hypothetical multi-domain corpus is the canonical example.
        let mut concept = person(1, "Compatibilism", None, None);
        concept.entity_type = EntityType::Concept;
        let atoms = vec![
            AtomEnvelope::Entity(concept),
            AtomEnvelope::Entity(person(2, "Sarah", None, None)),
        ];
        let edges = vec![involves_edge(1, &AtomId::entity(2), "10")];
        let ts = |id: &str| -> Option<i64> { id.parse().ok() };
        let timelines = assemble(&atoms, &edges, &ts, &NoAtosLookup);
        assert_eq!(timelines.len(), 1);
        assert_eq!(timelines[0].entity_name, "Sarah");
    }
}
