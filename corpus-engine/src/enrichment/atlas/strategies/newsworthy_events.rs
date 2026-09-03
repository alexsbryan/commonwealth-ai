// SPDX-License-Identifier: AGPL-3.0-or-later
//! `newsworthy_events` — per-bullet atlas extraction for
//! `wikipedia-newsworthy` (and any future portal-style corpus).
//!
//! The `structure_first` strategy treats each ingested doc as one
//! `Entity`-of-type-article. That shape is correct for the parent
//! `wikipedia` corpus (one article = one Albert Einstein) but
//! catastrophic for `wikipedia-newsworthy`, where each ingested
//! "article" IS a daily `Portal:Current_events` page whose body is
//! 20–40 bullets, each describing a distinct event. Running
//! structure_first on a portal page collapses it to a single
//! Entity whose `description` is the raw lead — useless surface in
//! the Atlas Viewer.
//!
//! This strategy works at the bullet grain because
//! `portal_event_bullet` chunker already emitted one chunk per
//! bullet, with `outgoing_links` rescoped to wikilinks that appear
//! inside that bullet's text. For each chunk we emit:
//!
//!   - One [`Event`] atom (description = bullet text, evidence =
//!     ChunkRef to the bullet, section_position = the portal date).
//!   - One placeholder [`Entity`] per wikilink target observed
//!     across all bullets (same shape `structure_first` uses for
//!     off-corpus links).
//!   - One `Involves` [`Edge`] per (event → wikilink target) pair.
//!
//! Atoms are keyed under `doc_id = source_doc_id` (the ISO date)
//! so a portal-day re-ingest replaces only that day's events;
//! placeholder entities live under the shared `_placeholders`
//! doc_id so day deletions don't churn the entity layer.
//!
//! Out of scope (deferred):
//!   - `occurred_on` first-class field on Event — the schema doesn't
//!     carry a date attribute yet. We encode the date into
//!     `section_position.section_id` (which IS the source_doc_id)
//!     so retrieval can group/filter by date string until a typed
//!     date field lands.
//!   - LLM-tier upgrades: linking participants via wikidata, classifying
//!     bullets into [`EventType`] variants beyond the default
//!     `Other("portal_bullet")`. Cheap to bolt on once classic
//!     extraction is proven.

use std::collections::{BTreeMap, HashMap};

use crate::enrichment::atlas::atoms::{
    AtomEnvelope, AtomId, ChunkRef, Entity, Event, SectionPosition,
};
use crate::enrichment::atlas::atoms_delta::AtomsDelta;
use crate::enrichment::atlas::edges::{Edge, EdgeId, EdgeProvenance, EdgeType};
use crate::enrichment::pipeline::atlas::{EnrichmentDepth, EntityType, EventType};
use crate::extractors::wikipedia_types::WikipediaChunkMetadata;
use crate::index::EnrichmentChunkRow;

/// Result of per-portal-day extraction. Mirrors
/// [`crate::enrichment::atlas::strategies::structure_first::StructureFirstDelta`]
/// so the host-side wire can swap one for the other behind a strategy
/// selector.
#[derive(Debug, Clone)]
pub struct NewsworthyEventsDelta {
    pub atoms_delta: AtomsDelta,
    pub edges: Vec<Edge>,
}

/// `entity_type` for wikilink placeholders. `Other("wikilink")` keeps
/// the type slot honest — we don't yet know whether the target is a
/// person, place, or concept. A later classifier tier can upgrade these
/// in place (the atom id is stable across re-extractions).
fn wikilink_entity_type() -> EntityType {
    EntityType::Other("wikilink".to_string())
}

/// `event_type` for portal bullets. `Other("portal_bullet")` distinguishes
/// these from LLM-classified events; downstream consumers can filter by
/// this string to scope to newsworthy-derived events specifically.
fn portal_bullet_event_type() -> EventType {
    EventType::Other("portal_bullet".to_string())
}

/// Short preview string for `ChunkRef::passage_preview` — first ~120
/// chars of the bullet. Mirrors `structure_first::preview`.
fn preview(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect::<String>() + "…"
    }
}

/// Extract per-bullet atoms + edges from a slice of `wikipedia-newsworthy`
/// chunks. The caller is responsible for scoping `chunks` to the docs
/// it actually wants extracted (one date for the per-tick incremental
/// path; the whole corpus for a full rebuild).
///
/// `corpus_id` MUST match the atlas's owning corpus so atom ids hash
/// stably across re-extractions.
pub fn extract_atoms_for_portal_chunks(
    chunks: &[EnrichmentChunkRow],
    corpus_id: &str,
) -> NewsworthyEventsDelta {
    let event_type = portal_bullet_event_type();
    let entity_type = wikilink_entity_type();

    // ── Pass 1: per-bullet Events, grouped by source_doc (date) ───────
    //
    // `upserted_docs_map` is keyed by source_doc_id so the apply path
    // replaces a portal day's events atomically when the day is
    // re-ingested at a new revid.
    let mut upserted_docs_map: BTreeMap<String, Vec<AtomEnvelope>> = BTreeMap::new();
    // event_id → its bullet's wikilink targets (Vec of target_title).
    // Edge construction in pass 3 walks this to attach Involves edges.
    let mut event_to_targets: Vec<(AtomId, Vec<String>)> = Vec::new();
    // target_title → (first_chunk_id, first_link_text) for placeholder
    // construction in pass 2. BTreeMap for deterministic iteration.
    let mut placeholder_seeds: BTreeMap<String, (u64, String)> = BTreeMap::new();

    for chunk in chunks {
        let Some(source_doc_id) = chunk.source_doc_id.as_deref() else {
            continue;
        };
        // The portal-bullet chunker emits one chunk per bullet, so
        // each chunk's content IS the event description. Trim only —
        // don't strip further, the extractor's strip_wikitext pass
        // already cleaned the markup.
        let description = chunk.content.trim();
        if description.is_empty() {
            continue;
        }

        // section_id = source_doc_id so date-scoped retrieval reads
        // SectionPosition.section_id directly without parsing the
        // chunk metadata. Same value as the doc_id grouping key.
        let event_id =
            AtomId::event_content_hash(description, &event_type, source_doc_id, corpus_id);

        // Pull wikilinks out of the chunk's metadata. Missing or
        // malformed metadata isn't fatal — the bullet still becomes
        // an event, just without participants.
        let mut wikilink_targets: Vec<String> = Vec::new();
        if let Some(meta_raw) = chunk.metadata_raw.as_deref() {
            if let Ok(meta) = serde_json::from_str::<WikipediaChunkMetadata>(meta_raw) {
                for link in &meta.outgoing_links {
                    let target = link.target_title.trim();
                    if target.is_empty() || is_meta_namespace(target) {
                        continue;
                    }
                    wikilink_targets.push(target.to_string());
                    placeholder_seeds
                        .entry(target.to_string())
                        .or_insert((chunk.id, link.link_text.clone()));
                }
            }
        }

        // participants populated in pass 3 once placeholder atom_ids
        // exist. Stash an empty Vec for now and emit the Event atom.
        let event = Event {
            attributes: Default::default(),
            id: event_id.clone(),
            description: description.to_string(),
            event_type: event_type.clone(),
            participants: Vec::new(),
            evidence: vec![ChunkRef::new(
                chunk.id.to_string(),
                Some(preview(description, 120)),
            )],
            section_position: SectionPosition::section(source_doc_id.to_string()),
            causal_antecedents: Vec::new(),
            enrichment_depth: EnrichmentDepth::Structural,
        };
        upserted_docs_map
            .entry(source_doc_id.to_string())
            .or_default()
            .push(AtomEnvelope::Event(event));
        event_to_targets.push((event_id, wikilink_targets));
    }

    // ── Pass 2: placeholder Entity atoms for every unique wikilink ───
    //
    // Same shape `structure_first` uses for off-corpus links. The
    // `_placeholders` doc_id segregates them so per-day re-extraction
    // doesn't churn the entity layer (a day rotating out drops only
    // its Event atoms; the wikilink Entities survive until no day
    // references them).
    let mut target_to_atom: HashMap<String, AtomId> =
        HashMap::with_capacity(placeholder_seeds.len());
    let mut placeholder_atoms: Vec<AtomEnvelope> = Vec::with_capacity(placeholder_seeds.len());
    for (target, (first_chunk_id, link_text)) in &placeholder_seeds {
        let atom_id = AtomId::entity_content_hash(target, &entity_type, corpus_id);
        target_to_atom.insert(target.clone(), atom_id.clone());
        placeholder_atoms.push(AtomEnvelope::Entity(Entity {
            id: atom_id,
            canonical_name: target.clone(),
            aliases: if link_text != target {
                vec![link_text.clone()]
            } else {
                Vec::new()
            },
            entity_type: entity_type.clone(),
            first_appearance: ChunkRef::new(first_chunk_id.to_string(), None),
            description: String::new(),
            defining_quote: None,
            salience: 0.0,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        }));
    }

    // ── Pass 3: Involves edges Event → Entity, plus participants ─────
    //
    // We collected event_id → target_title pairs in pass 1; now that
    // placeholder atom ids exist, materialise the edges and back-fill
    // `participants` on each Event atom (in-place mutation by walking
    // upserted_docs_map and matching by id).
    let mut edges: Vec<Edge> = Vec::new();
    let mut next_edge_idx = 1usize;
    let mut event_participants: HashMap<AtomId, Vec<AtomId>> =
        HashMap::with_capacity(event_to_targets.len());
    for (event_id, targets) in &event_to_targets {
        let mut participant_ids: Vec<AtomId> = Vec::with_capacity(targets.len());
        for target in targets {
            let Some(target_atom) = target_to_atom.get(target) else {
                continue;
            };
            participant_ids.push(target_atom.clone());
            edges.push(Edge {
                id: EdgeId::new(next_edge_idx),
                edge_type: EdgeType::Involves,
                source: event_id.clone(),
                target: target_atom.clone(),
                evidence: Vec::new(),
                trigger_event: None,
                sub_question: None,
                confidence: 1.0,
                provenance: EdgeProvenance::WikilinkStructural,
            });
            next_edge_idx += 1;
        }
        event_participants.insert(event_id.clone(), participant_ids);
    }
    // Back-fill participants on every Event we emitted.
    for envelopes in upserted_docs_map.values_mut() {
        for env in envelopes.iter_mut() {
            if let AtomEnvelope::Event(event) = env {
                if let Some(parts) = event_participants.get(&event.id) {
                    event.participants = parts.clone();
                }
            }
        }
    }

    let mut upserted_docs: Vec<(String, Vec<AtomEnvelope>)> =
        upserted_docs_map.into_iter().collect();
    if !placeholder_atoms.is_empty() {
        upserted_docs.push(("_placeholders".to_string(), placeholder_atoms));
    }

    NewsworthyEventsDelta {
        atoms_delta: AtomsDelta {
            added: Vec::new(),
            removed_doc_ids: Vec::new(),
            upserted_docs,
            added_edges: edges.clone(),
        },
        edges,
    }
}

/// Same exclusion list `structure_first` uses for off-corpus link
/// placeholders. Keeps Help:/Wikipedia:/Template:/Portal: out of the
/// entity layer so the graph stays focused on real-world subjects.
/// (Inlined rather than re-exported to keep strategies independent.)
fn is_meta_namespace(title: &str) -> bool {
    const META_PREFIXES: &[&str] = &[
        "Help:",
        "Wikipedia:",
        "Template:",
        "Portal:",
        "Category:",
        "File:",
        "Image:",
        "User:",
        "User talk:",
        "Special:",
        "Talk:",
        "Draft:",
        "MediaWiki:",
        "Module:",
        "Book:",
        "TimedText:",
    ];
    META_PREFIXES.iter().any(|p| title.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::wikipedia_types::WikiLink;

    fn portal_chunk(
        id: u64,
        date_iso: &str,
        content: &str,
        outgoing: Vec<(&str, &str)>,
    ) -> EnrichmentChunkRow {
        let meta = WikipediaChunkMetadata {
            section_name: "Lead".into(),
            section_path: vec![],
            section_depth: 0,
            section_type: "lead".into(),
            citation_needed_count: None,
            pov_count: None,
            clarification_needed_count: None,
            update_count: None,
            is_flagged_stable: None,
            outgoing_links: outgoing
                .into_iter()
                .map(|(t, lt)| WikiLink {
                    target_title: t.into(),
                    link_text: lt.into(),
                })
                .collect(),
            revision_id: None,
            wikidata_qid: None,
            page_id: None,
        };
        EnrichmentChunkRow {
            id,
            content: content.into(),
            title: Some(format!("Portal:Current_events/{date_iso}")),
            url: None,
            metadata_raw: Some(serde_json::to_string(&meta).unwrap()),
            source_doc_id: Some(date_iso.into()),
        }
    }

    #[test]
    fn one_event_atom_per_bullet() {
        let chunks = vec![
            portal_chunk(1, "2026-05-23", "Iran ceasefire talks progress.", vec![]),
            portal_chunk(2, "2026-05-23", "Tunisian protests erupt.", vec![]),
        ];
        let delta = extract_atoms_for_portal_chunks(&chunks, "wikipedia-newsworthy");
        // Both Events live under the same source_doc_id grouping.
        assert_eq!(delta.atoms_delta.upserted_docs.len(), 1);
        let (doc_id, atoms) = &delta.atoms_delta.upserted_docs[0];
        assert_eq!(doc_id, "2026-05-23");
        assert_eq!(atoms.len(), 2);
        assert!(atoms.iter().all(|a| matches!(a, AtomEnvelope::Event(_))));
    }

    #[test]
    fn wikilinks_emit_placeholder_entities_and_involves_edges() {
        let chunks = vec![portal_chunk(
            1,
            "2026-05-23",
            "Iran and Pakistan negotiate ceasefire.",
            vec![("Iran", "Iran"), ("Pakistan", "Pakistan")],
        )];
        let delta = extract_atoms_for_portal_chunks(&chunks, "wikipedia-newsworthy");
        // 1 day's events + 1 _placeholders group.
        assert_eq!(delta.atoms_delta.upserted_docs.len(), 2);
        let placeholders = delta
            .atoms_delta
            .upserted_docs
            .iter()
            .find(|(d, _)| d == "_placeholders")
            .expect("placeholders group present");
        assert_eq!(placeholders.1.len(), 2);
        assert_eq!(delta.edges.len(), 2);
        assert!(delta
            .edges
            .iter()
            .all(|e| e.edge_type == EdgeType::Involves));
    }

    #[test]
    fn re_extraction_yields_stable_atom_ids() {
        let chunks = vec![portal_chunk(
            1,
            "2026-05-23",
            "Iran ceasefire.",
            vec![("Iran", "Iran")],
        )];
        let a = extract_atoms_for_portal_chunks(&chunks, "wikipedia-newsworthy");
        let b = extract_atoms_for_portal_chunks(&chunks, "wikipedia-newsworthy");
        let a_ids: Vec<&AtomId> = a
            .atoms_delta
            .upserted_docs
            .iter()
            .flat_map(|(_, atoms)| atoms.iter().map(|env| env.id()))
            .collect();
        let b_ids: Vec<&AtomId> = b
            .atoms_delta
            .upserted_docs
            .iter()
            .flat_map(|(_, atoms)| atoms.iter().map(|env| env.id()))
            .collect();
        assert_eq!(a_ids, b_ids);
    }

    #[test]
    fn meta_namespace_wikilinks_excluded() {
        let chunks = vec![portal_chunk(
            1,
            "2026-05-23",
            "Iran ceasefire.",
            vec![
                ("Iran", "Iran"),
                ("Portal:Current_events/2026_May_22", "yesterday"),
                ("File:Some.svg", "img"),
            ],
        )];
        let delta = extract_atoms_for_portal_chunks(&chunks, "wikipedia-newsworthy");
        // Only "Iran" should produce a placeholder.
        let placeholders = delta
            .atoms_delta
            .upserted_docs
            .iter()
            .find(|(d, _)| d == "_placeholders")
            .expect("placeholders group present");
        assert_eq!(placeholders.1.len(), 1);
        assert_eq!(delta.edges.len(), 1);
    }

    #[test]
    fn empty_bullets_are_skipped() {
        let chunks = vec![portal_chunk(1, "2026-05-23", "   ", vec![])];
        let delta = extract_atoms_for_portal_chunks(&chunks, "wikipedia-newsworthy");
        assert!(delta.atoms_delta.upserted_docs.is_empty());
        assert!(delta.edges.is_empty());
    }
}
