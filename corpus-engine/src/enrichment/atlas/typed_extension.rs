// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed-extension pass's atlas write (pb-ingest-dial-tools-atlas).
//!
//! svrn's `typed_extension` drives the LLM calls and hands the responses
//! here through the atlas port; this module parses them into the engine's
//! `SectionExtraction`s, resolves them against the person seeds, rewrites
//! the ids to content hashes, and writes the atlas with its seed table.

use std::collections::HashMap;
use std::path::Path;

use corpus_engine_atlas_reader::citation::SourceCitation;
use corpus_engine_atlas_reader::ports::ArgumentativeResponse;
use corpus_index::error::{Error, Result};
use understanding_vocab::atoms::{AtomId, Claim, Entity, Opposition, Position};
use understanding_vocab::edges::Edge;

use super::ann_store::AtlasSeeding;
use super::resolution::{resolve_type_extensions, TypeExtensionResolveOutput};
use super::writer::write_atlas_full;
use crate::enrichment::pipeline::atlas::{
    ArgumentativeExtension, EnrichmentDepth, SectionExtraction, TypeExtension,
};
use crate::enrichment::pipeline::typed_schemas::argumentative::parse_phase1_argumentative;
use crate::types::EmbedFn;

/// Parse one response and, for a cross-leaf (Pass B) response, keep only
/// the oppositions and concessions.
pub(crate) fn parse_argumentative(
    response_text: &str,
    cross_leaf_only: bool,
) -> std::result::Result<ArgumentativeExtension, String> {
    let extension = parse_phase1_argumentative(response_text).map_err(|e| format!("{e}"))?;
    if !cross_leaf_only {
        return Ok(extension);
    }
    // Drop everything except oppositions + concessions.
    Ok(ArgumentativeExtension {
        positions: Vec::new(),
        mechanisms: Vec::new(),
        evidence_invocations: Vec::new(),
        oppositions: extension.oppositions,
        concessions: extension.concessions,
    })
}

/// Resolve, remap and write the typed atlas. Returns the atom count per
/// kind the manifest records.
pub(crate) fn write_typed_extension(
    corpus_id: &str,
    atlas_dir: &Path,
    responses: &[ArgumentativeResponse],
    person_seeds: Vec<Entity>,
    citations: &HashMap<String, SourceCitation>,
    embed_query: EmbedFn,
) -> Result<HashMap<String, u32>> {
    let mut sections: Vec<SectionExtraction> = Vec::with_capacity(responses.len());
    for r in responses {
        let extension = parse_argumentative(&r.response_text, r.cross_leaf_only).map_err(|e| {
            Error::Serialization(format!(
                "typed_extension: response for {} parsed at the call and not at the write: {e}",
                r.section_id
            ))
        })?;
        sections.push(synth_section(
            r.section_id.clone(),
            TypeExtension::Argumentative(extension),
        ));
    }

    let mut resolved = resolve_type_extensions(
        &sections,
        &person_seeds,          // proponent / supports resolution targets
        &[],                    // no existing positions
        &[],                    // no existing claims
        person_seeds.len() + 1, // next_entity_idx — seeds occupy 1..=N
        1,                      // next_claim_idx
        1,                      // next_position_idx
        1,                      // next_opposition_idx
        1,                      // next_edge_idx
    );
    // The seeds must also PERSIST (the resolver treats `existing_*`
    // as already-on-disk, but this atlas is written from scratch).
    // Prepending keeps them ahead of the remap walk so positions'
    // `proponent_id` references rewrite coherently.
    let mut all_entities = person_seeds;
    all_entities.append(&mut resolved.new_entities);
    resolved.new_entities = all_entities;

    // Rewrite sequential ids to content-hash ids so re-runs are
    // idempotent across machines and across re-extractions. Resolver
    // emits sequential ids; this walk produces a remap and rewrites
    // every edge endpoint + qualifier-update key through it. While
    // we're walking the atoms, also project every `ChunkRef` through
    // the `citations` lookup so `passage_preview` carries the
    // verbatim source sentence (glassbox source recovery — see
    // `SourceCitation` doc for the rationale).
    let (entities, positions, oppositions, claims, edges) =
        content_hash_remap(corpus_id, resolved, citations);

    let mut atoms_per_kind: HashMap<String, u32> = HashMap::new();
    atoms_per_kind.insert(
        "mechanism".into(),
        entities
            .iter()
            .filter(|e| e.concept_kind.as_deref() == Some("mechanism"))
            .count() as u32,
    );
    atoms_per_kind.insert("named_position".into(), positions.len() as u32);
    atoms_per_kind.insert(
        "evidence".into(),
        claims
            .iter()
            .filter(|c| c.claim_kind.as_deref() == Some("evidence"))
            .count() as u32,
    );
    atoms_per_kind.insert("opposition".into(), oppositions.len() as u32);
    atoms_per_kind.insert(
        "concession".into(),
        claims
            .iter()
            .filter(|c| c.claim_kind.as_deref() == Some("concession"))
            .count() as u32,
    );

    std::fs::create_dir_all(atlas_dir).map_err(|e| {
        Error::Serialization(format!(
            "typed_extension: create atlas dir {}: {e}",
            atlas_dir.display()
        ))
    })?;

    write_atlas_full(
        atlas_dir,
        &entities,
        &[], // events
        &[], // states
        &[], // relations
        &claims,
        &[], // questions
        &[], // configurations
        &[], // argument_reconstructions
        &positions,
        &oppositions,
        &edges,
        &std::collections::BTreeMap::new(), // trajectories
        // ontology-v1 P0.3, now structural (ei-3-index): a freshly written
        // atlas grounds without an operator command, because the seed table is
        // part of the write rather than a step someone remembers. The daemon
        // holds the embed provider, and the QUERY-side adapter keeps the table
        // in the vector space `atlas_navigate_ann` queries it in.
        //
        // Fail-hard, like the v2 store beside it: an atlas with atoms and no
        // seed table loads, enumerates, reports nothing wrong, and cannot
        // ground. `atoms.json` is on disk before this runs, so the recovery is
        // `svrn atlas backfill-ann <corpus>`; this pass is detached after
        // `Complete` (`conv_tiered_provider::post_finalize_corpus`), so the
        // failure surfaces there rather than in the user's turn.
        &AtlasSeeding::With(embed_query),
    )
    .map_err(|e| {
        Error::Serialization(format!(
            "typed_extension: write_atlas_full ({}): {e}",
            atlas_dir.display()
        ))
    })?;

    Ok(atoms_per_kind)
}

/// Walk `resolved` and rewrite every atom + edge id from the
/// resolver's sequential `entity-NNNN` / `claim-NNNN` / `position-NNNN`
/// / `opposition-NNNN` shape to the matching content-hash id via
/// `AtomId::*_content_hash`. Returns the rewritten atoms + edges
/// ready for `write_atlas_full`.
///
/// Edges reference their endpoints by `AtomId`, so we build a remap
/// keyed on the original ids and rewrite each edge's `source` / `target`
/// through it.
pub(crate) fn content_hash_remap(
    corpus_id: &str,
    mut resolved: TypeExtensionResolveOutput,
    citations: &HashMap<String, SourceCitation>,
) -> (
    Vec<Entity>,
    Vec<Position>,
    Vec<Opposition>,
    Vec<Claim>,
    Vec<Edge>,
) {
    // Apply primary-source citations to every ChunkRef the resolver
    // emitted BEFORE the content-hash rewrite. The walk is a single
    // call into corpus-engine atlas's lifted helper — no inline
    // repetition of the per-collection iteration.
    super::resolution::apply_citations_to_resolved(&mut resolved, citations);

    let TypeExtensionResolveOutput {
        new_entities,
        entity_qualifier_updates: _, // we don't have existing entities; resolver only emits these
        // for fuzzy-merged existing concepts, of which we have none.
        new_claims,
        new_positions,
        new_oppositions,
        new_edges,
        failures: _, // already surfaced via soft_failures upstream
    } = resolved;

    let mut id_remap: HashMap<AtomId, AtomId> = HashMap::new();

    // Entities — Concept-kinded mechanism atoms in our pass.
    let mut entities_out = Vec::with_capacity(new_entities.len());
    for mut entity in new_entities {
        let new_id =
            AtomId::entity_content_hash(&entity.canonical_name, &entity.entity_type, corpus_id);
        id_remap.insert(entity.id.clone(), new_id.clone());
        entity.id = new_id;
        entities_out.push(entity);
    }

    // Positions. Entities remapped first (above) so `proponent_id`
    // — which references a (possibly GLiNER-seeded) Entity by its
    // sequential id — rewrites to the entity's content-hash id here.
    // Without this rewrite the persisted position points at an id
    // that no longer exists and the eval renders proponent as "".
    let mut positions_out = Vec::with_capacity(new_positions.len());
    for mut position in new_positions {
        let new_id =
            AtomId::position_content_hash(&position.canonical_name, &position.stance, corpus_id);
        id_remap.insert(position.id.clone(), new_id.clone());
        position.id = new_id;
        if let Some(prop) = position.proponent_id.take() {
            position.proponent_id = Some(id_remap.get(&prop).cloned().unwrap_or(prop));
        }
        positions_out.push(position);
    }

    // Oppositions.
    let mut oppositions_out = Vec::with_capacity(new_oppositions.len());
    for mut opposition in new_oppositions {
        let new_id = AtomId::opposition_content_hash(&opposition.canonical_label, corpus_id);
        id_remap.insert(opposition.id.clone(), new_id.clone());
        opposition.id = new_id;
        oppositions_out.push(opposition);
    }

    // Claims (evidence + concession).
    let mut claims_out = Vec::with_capacity(new_claims.len());
    for mut claim in new_claims {
        let new_id = AtomId::claim_content_hash(
            &claim.content,
            &claim.discourse_act,
            &claim.epistemic_status,
            corpus_id,
        );
        id_remap.insert(claim.id.clone(), new_id.clone());
        claim.id = new_id;
        claims_out.push(claim);
    }

    // Rewrite edges through the remap. Endpoints that don't appear in
    // the remap are kept as-is — those come from fuzzy-merge edges
    // pointing at existing atoms (none in this pass) and so wouldn't
    // appear here, but defensive pass-through keeps the function
    // total even if resolver shape evolves.
    let edges_out: Vec<Edge> = new_edges
        .into_iter()
        .map(|mut edge| {
            if let Some(new) = id_remap.get(&edge.source) {
                edge.source = new.clone();
            }
            if let Some(new) = id_remap.get(&edge.target) {
                edge.target = new.clone();
            }
            edge
        })
        .collect();

    (
        entities_out,
        positions_out,
        oppositions_out,
        claims_out,
        edges_out,
    )
}

/// Helper used by both passes: wrap an `ArgumentativeExtension` in a
/// synthetic `SectionExtraction` so it can be fed to
/// `resolve_type_extensions`. The resolver only reads
/// `type_extensions` + `section_id` + `enrichment_depth` for our
/// purposes, so the other fields stay at their `Default` zero-values.
pub(crate) fn synth_section(section_id: String, extension: TypeExtension) -> SectionExtraction {
    SectionExtraction {
        section_id,
        enrichment_depth: EnrichmentDepth::Extracted,
        type_extensions: vec![extension],
        ..Default::default()
    }
}
