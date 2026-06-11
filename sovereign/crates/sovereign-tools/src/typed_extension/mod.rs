// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed-extension LLM pass over RAPTOR cluster summaries.
//!
//! Spec: `sovereign/docs/specs/TYPED_EXTENSION_PASS.md`.
//!
//! Runs inside `FolderTieredProvider::finalize_corpus` *after*
//! `run_vault_synthesis`. Reads `conv_raptor_nodes` (Pass A inputs)
//! and `vault_themes` (Pass B inputs), drives one LLM call per leaf /
//! theme against the existing argumentative typed-extension schema
//! (`corpus_engine::enrichment::pipeline::typed_schemas::argumentative`),
//! projects the parsed sketches through the existing resolver
//! (`corpus_engine::enrichment::atlas::resolution::resolve_type_extensions`),
//! rewrites the sequential atom ids to content-hash ids so re-runs are
//! idempotent, and writes `{atlas_dir}/atoms.json` + `atoms.meta.json`
//! via `corpus_engine::enrichment::atlas::writer::write_atlas_full`.
//!
//! The pass is bench-side only — no chat-side surface reads typed
//! atoms in v2. atoms.json materialises when ready; `sovereign enrich
//! eval` reads-or-skips on file presence. The manifest sidecar gates
//! re-extraction so unchanged RAPTOR state is a no-op.
//!
//! Failure semantics match `run_vault_synthesis`: best-effort, log
//! and return `Ok` so the per-note retrieval surface stays unblocked
//! even when this pass errors.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use corpus_engine::enrichment::atlas::atoms::{AtomId, Claim, Entity, Opposition, Position};
use corpus_engine::enrichment::atlas::edges::Edge;
use corpus_engine::enrichment::atlas::resolution::{
    resolve_type_extensions, TypeExtensionResolveOutput,
};
use corpus_engine::enrichment::atlas::writer::write_atlas_full;
use corpus_engine::enrichment::atlas::SourceCitation;
use corpus_engine::enrichment::pipeline::atlas::{
    EnrichmentDepth, SectionExtraction, TypeExtension,
};
use corpus_engine::error::{Error, Result};
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, ConvTieredReader, VaultThemeRow};
use sovereign_core::traits::InferenceProvider;
use sovereign_store::sqlite::SqliteStateStore;

mod manifest;
mod pass;

#[cfg(test)]
mod tests;

pub use manifest::{TypedExtensionManifest, MANIFEST_FILENAME, MANIFEST_SCHEMA_VERSION};

/// Identifies this orchestration in the manifest's `produced_by`
/// field. Bumped when behaviour changes in a way that should force
/// re-extraction across the whole vault even when input hashes match.
/// v2 (2026-06-11): Pass A feeds verbatim member-chunk excerpts for
/// small leaves — summaries alone compressed out the essays'
/// load-bearing binaries/concessions (opposition + concession axes
/// scored 0 against the obsidian golden on summary-only input).
/// v3 (2026-06-11): GLiNER Person mentions seed Entity atoms ahead of
/// resolution — proponents previously had NOTHING to resolve against
/// (the pass passed `existing_entities = &[]`, so every position
/// persisted `proponent_id: None` and the person axis scored 0).
/// v4 (2026-06-11): figure-bearing sentences recovered from beyond
/// the excerpt windows feed Pass A — quantitative evidence sits
/// mid-chunk and was paraphrased out of evidence labels.
pub const PRODUCED_BY: &str = "tiered_typed_extension_v4";

/// Pass A source recovery: only leaves with at most this many member
/// chunks get verbatim excerpts (big leaves already aggregate too
/// much text for excerpts to stay representative, and their summaries
/// compress less per chunk).
const PASS_A_MAX_MEMBER_CHUNKS_FOR_EXCERPTS: usize = 6;

/// Per-excerpt character budget. 6 excerpts × 700 chars ≈ 4.2KB
/// prefill on top of the summary — bounded, and the fast slot's
/// prefill is cheap relative to the decode.
const PASS_A_EXCERPT_CHARS: usize = 700;

/// Figure-sentence recovery (v4) bounds: per-chunk and per-leaf caps
/// plus a per-sentence char budget. Generic digit-bearing-sentence
/// detection — not tuned to any golden's values.
const PASS_A_FIGURE_SENTENCES_PER_CHUNK: usize = 3;
const PASS_A_MAX_FIGURE_SENTENCES: usize = 8;
const PASS_A_FIGURE_SENTENCE_CHARS: usize = 240;

/// Summary of what `run_typed_extension` did in one finalize_corpus
/// call. Mostly diagnostic — the durable record is the
/// `atoms.meta.json` sidecar on disk.
#[derive(Debug, Clone, Default)]
pub struct TypedExtractionReport {
    pub status: ExtractionStatus,
    pub pass_a_calls: u32,
    pub pass_b_calls: u32,
    pub atoms_per_kind: HashMap<String, u32>,
    /// Per-leaf / per-theme failures that were tolerated. The atoms
    /// file still writes; this enumerates which inputs produced no
    /// typed output.
    pub soft_failures: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractionStatus {
    /// No work to do — manifest already matches the current RAPTOR +
    /// vault_themes state.
    SkippedManifestMatch,
    /// No level-0 RAPTOR leaves above the tiny-stub threshold and no
    /// vault themes. Nothing to extract.
    SkippedNoInputs,
    /// Pass(es) ran; atoms.json + manifest written.
    #[default]
    Wrote,
    /// Pass(es) ran, but every leaf / theme produced no atoms. atoms.json
    /// still rewritten (with an empty atoms vec) so the manifest gate
    /// can advance on the next call.
    WroteEmpty,
}

/// Public entry. Best-effort: caller logs failures and treats them as
/// non-fatal. Idempotent: a second call with no upstream changes
/// returns [`ExtractionStatus::SkippedManifestMatch`] without LLM
/// traffic.
pub async fn run_typed_extension(
    corpus_id: &str,
    store: &Arc<SqliteStateStore>,
    inference: &Arc<dyn InferenceProvider>,
    atlas_dir: &Path,
) -> Result<TypedExtractionReport> {
    let started_at = std::time::Instant::now();
    // Bring trait methods into scope explicitly so we don't depend on
    // SqliteStateStore's inherent-method shadowing — keeps this code
    // working if/when the store moves to trait-only dispatch.
    let _: &dyn ConvTieredReader = store.as_ref();

    // ── Gather inputs ────────────────────────────────────────────
    let source_doc_ids = store
        .list_ready_source_doc_ids_for_corpus(corpus_id)
        .await
        .map_err(|e| {
            Error::Database(format!(
                "typed_extension: list ready source_doc_ids ({corpus_id}): {e}"
            ))
        })?;

    let mut leaves: Vec<ConvRaptorNodeRow> = Vec::new();
    for doc_id in &source_doc_ids {
        let nodes = match store.list_conv_raptor_nodes(corpus_id, doc_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    doc = %doc_id,
                    error = %e,
                    "typed_extension: per-doc raptor fetch failed; skipping doc"
                );
                continue;
            }
        };
        for node in nodes.into_iter().filter(|n| n.level == 0) {
            if pass::leaf_is_extractable(&node) {
                leaves.push(node);
            }
        }
    }

    let themes: Vec<VaultThemeRow> = store
        .list_vault_themes_for_corpus(corpus_id)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                corpus = corpus_id,
                error = %e,
                "typed_extension: vault_themes fetch failed; running Pass A only"
            );
            Vec::new()
        });

    if leaves.is_empty() && themes.is_empty() {
        tracing::debug!(
            corpus = corpus_id,
            "typed_extension: no leaves or themes; nothing to extract"
        );
        return Ok(TypedExtractionReport {
            status: ExtractionStatus::SkippedNoInputs,
            ..Default::default()
        });
    }

    // ── Manifest gate ────────────────────────────────────────────
    let prev_manifest = TypedExtensionManifest::load(atlas_dir).ok().flatten();
    let raptor_hash = manifest::hash_raptor_leaves(&leaves);
    let themes_hash = manifest::hash_vault_themes(&themes);
    if let Some(prev) = prev_manifest.as_ref() {
        if prev.raptor_nodes_hash == raptor_hash
            && prev.vault_themes_hash == themes_hash
            && prev.produced_by == PRODUCED_BY
        {
            tracing::info!(
                corpus = corpus_id,
                leaves = leaves.len(),
                themes = themes.len(),
                "typed_extension: manifest matches; skipping extraction"
            );
            return Ok(TypedExtractionReport {
                status: ExtractionStatus::SkippedManifestMatch,
                pass_a_calls: prev.pass_a_calls,
                pass_b_calls: prev.pass_b_calls,
                atoms_per_kind: prev.atoms_per_kind.clone(),
                soft_failures: Vec::new(),
            });
        }
    }

    tracing::info!(
        corpus = corpus_id,
        leaves = leaves.len(),
        themes = themes.len(),
        "typed_extension: starting extraction"
    );

    // ── Pass A — per-leaf ────────────────────────────────────────
    // Source recovery for small leaves: the corpus index lives one
    // level above the atlas dir; open it once (best-effort — on any
    // failure Pass A degrades to summary+quote-span input, the v1
    // behavior).
    let index = corpus_engine::index::CorpusIndex::open(
        atlas_dir.parent().unwrap_or(atlas_dir),
    )
    .await
    .map_err(|e| {
        tracing::warn!(
            corpus = corpus_id,
            error = %e,
            "typed_extension: corpus index open failed; Pass A runs without member excerpts"
        );
        e
    })
    .ok();

    let mut sections: Vec<SectionExtraction> = Vec::with_capacity(leaves.len() + themes.len());
    let mut soft_failures: Vec<String> = Vec::new();
    let mut pass_a_calls: u32 = 0;
    // Section-id → primary-source citation. Populated as each Pass
    // A/B call returns; consulted in the post-resolver projection
    // pass to fill in `ChunkRef.passage_preview` on every atom +
    // edge endpoint. Without this lookup the resolver leaves
    // previews as `None` and atoms ground only at chunk granularity,
    // not at verbatim-sentence granularity.
    let mut citations: HashMap<String, SourceCitation> = HashMap::new();
    for leaf in &leaves {
        pass_a_calls += 1;
        let (member_excerpts, figure_sentences) = match index.as_ref() {
            Some(ix) => member_source_for_leaf(ix, leaf).await,
            None => (Vec::new(), Vec::new()),
        };
        match pass::pass_a_one_leaf(
            corpus_id,
            leaf,
            &member_excerpts,
            &figure_sentences,
            inference,
        )
        .await
        {
            Ok(Some((section, citation))) => {
                citations.insert(section.section_id.clone(), citation);
                sections.push(section);
            }
            Ok(None) => {
                // Empty extension — no atoms produced. Not a failure.
            }
            Err(reason) => {
                soft_failures.push(format!("pass_a:{}: {reason}", leaf.node_id));
            }
        }
    }

    // ── Pass B — per-vault-theme ─────────────────────────────────
    // Index level-0 leaves by source_doc_id so each theme can pull
    // verbatim excerpts from its contributing notes — same source-
    // recovery discipline as Pass A. Without this, Pass B would only
    // see the cross-leaf theme paraphrase and reproduce the prior
    // run's verbose oppositions / paraphrased concession names.
    let mut leaves_by_doc: HashMap<String, Vec<&ConvRaptorNodeRow>> = HashMap::new();
    for leaf in &leaves {
        leaves_by_doc
            .entry(leaf.conv_uuid.clone())
            .or_default()
            .push(leaf);
    }

    let mut pass_b_calls: u32 = 0;
    for theme in &themes {
        pass_b_calls += 1;
        let member_quotes = collect_member_quotes_for_theme(theme, &leaves_by_doc);
        match pass::pass_b_one_theme_with_excerpts(corpus_id, theme, &member_quotes, inference)
            .await
        {
            Ok(Some((section, citation))) => {
                citations.insert(section.section_id.clone(), citation);
                sections.push(section);
            }
            Ok(None) => {}
            Err(reason) => {
                soft_failures.push(format!("pass_b:{}: {reason}", theme.theme_id));
            }
        }
    }

    // ── Project + write ──────────────────────────────────────────
    // Person Entity seeds from GLiNER `chunk_entities` (v3). Two
    // axes depend on these: the bench's person axis reads Person
    // entities straight off atoms.json, and position `proponent_id`
    // resolution needs Entity atoms to resolve AGAINST — with the
    // previous `existing_entities = &[]`, every proponent the model
    // emitted ("Hardin", "Ostrom") died with an UnresolvedEntityName
    // failure. Deterministic, zero LLM cost; noise-gated inside the
    // builder (multi-token canonicals, digit guard, surname
    // subsumption).
    let mut person_rows: Vec<sovereign_core::conv_tiered::ChunkEntityRow> = Vec::new();
    for doc_id in &source_doc_ids {
        match store.list_chunk_entities_for_conv(corpus_id, doc_id).await {
            Ok(rows) => person_rows.extend(rows),
            Err(e) => tracing::debug!(
                corpus = corpus_id,
                doc = %doc_id,
                error = %e,
                "typed_extension: chunk_entities fetch failed; person seeding degrades"
            ),
        }
    }
    let person_seeds = build_person_seed_entities(&person_rows);
    tracing::info!(
        corpus = corpus_id,
        mentions = person_rows.len(),
        seeds = person_seeds.len(),
        "typed_extension: GLiNER person seeds built"
    );

    let mut resolved = resolve_type_extensions(
        &sections,
        &person_seeds, // proponent / supports resolution targets
        &[],           // no existing positions
        &[],           // no existing claims
        person_seeds.len() + 1, // next_entity_idx — seeds occupy 1..=N
        1,             // next_claim_idx
        1,             // next_position_idx
        1,             // next_opposition_idx
        1,             // next_edge_idx
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
        content_hash_remap(corpus_id, resolved, &citations);

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

    let total_atoms: u32 = atoms_per_kind.values().copied().sum();

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
    )
    .map_err(|e| {
        Error::Serialization(format!(
            "typed_extension: write_atlas_full ({}): {e}",
            atlas_dir.display()
        ))
    })?;

    let extracted_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let manifest = TypedExtensionManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        produced_by: PRODUCED_BY.to_string(),
        raptor_nodes_hash: raptor_hash,
        vault_themes_hash: themes_hash,
        extracted_at_unix,
        pass_a_calls,
        pass_b_calls,
        atoms_per_kind: atoms_per_kind.clone(),
    };
    if let Err(e) = manifest.write(atlas_dir) {
        tracing::warn!(
            corpus = corpus_id,
            error = %e,
            "typed_extension: manifest write failed; next run will redo extraction"
        );
    }

    let status = if total_atoms == 0 {
        ExtractionStatus::WroteEmpty
    } else {
        ExtractionStatus::Wrote
    };

    tracing::info!(
        corpus = corpus_id,
        pass_a_calls,
        pass_b_calls,
        total_atoms,
        soft_failures = soft_failures.len(),
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "typed_extension: complete"
    );

    Ok(TypedExtractionReport {
        status,
        pass_a_calls,
        pass_b_calls,
        atoms_per_kind,
        soft_failures,
    })
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
fn content_hash_remap(
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
    corpus_engine::enrichment::atlas::resolution::apply_citations_to_resolved(
        &mut resolved,
        citations,
    );

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

/// Hard cap on member-leaf quotes forwarded into one Pass B body.
/// Pass B's job is cross-leaf oppositions + concessions; the
/// excerpts are source recovery, not a replacement for the theme
/// summary itself. 6 quotes keeps the prompt under ~2KB above the
/// theme summary even when each is the full 320-char cap.
const PASS_B_QUOTE_CAP_PER_THEME: usize = 6;

/// Pull verbatim quote spans (text + chunk_id) from the leaves that
/// contributed to a `theme`. The vault_themes row carries
/// `member_source_doc_ids_json` (the notes whose RAPTOR leaves
/// clustered into this theme). Looks them up in the already-loaded
/// `leaves_by_doc` index, flattens each leaf's `quote_spans_json`
/// via `pass::parse_quote_spans`, and returns the first
/// [`PASS_B_QUOTE_CAP_PER_THEME`] spans.
///
/// Returns an empty vec when the theme has no resolvable members —
/// Pass B still runs on the theme summary alone in that case, just
/// without the source-recovery handles AND without a `chunk:<id>`
/// citation handle (atoms fall back to `theme:<theme_id>`).
/// Build Person Entity seeds from GLiNER chunk-entity mentions.
///
/// Noise gates (GLiNER emits ~5 mentions per chunk, most of them
/// generic role words):
/// - `label == "Person"` and extractor score ≥ 0.5 only.
/// - No digits in the name (the wikilink/date trap — `[[2024-01-15]]`
///   must NEVER surface as a Person; see the vault-port invariant).
/// - The canonical form must be MULTI-TOKEN ("Elinor Ostrom") —
///   single-token mentions ("user", "Margaret", "CEO") only survive
///   by SUBSUMPTION: a single-token name that appears as a whole
///   word inside exactly one multi-token name folds into it as an
///   alias ("Ostrom" → "Elinor Ostrom"), merging counts. Ambiguous
///   or host-less single tokens are dropped.
///
/// Canonical = the most frequent multi-token surface form. Returns
/// entities with sequential ids starting at 1 (caller offsets the
/// resolver's `next_entity_idx` accordingly).
fn build_person_seed_entities(
    rows: &[sovereign_core::conv_tiered::ChunkEntityRow],
) -> Vec<Entity> {
    use corpus_engine::enrichment::atlas::atoms::ChunkRef;
    use corpus_engine::enrichment::pipeline::atlas::EntityType;

    fn fold(s: &str) -> String {
        s.trim().to_lowercase()
    }

    // folded form → (count, best surface form, first chunk_id)
    let mut by_form: HashMap<String, (usize, String, u64)> = HashMap::new();
    for r in rows {
        if r.label != "Person" || r.score < 0.5 {
            continue;
        }
        let text = r.text.trim();
        if text.len() < 3 || text.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let key = fold(text);
        let entry = by_form
            .entry(key)
            .or_insert_with(|| (0, text.to_string(), r.chunk_id));
        entry.0 += 1;
    }

    let multi: Vec<(String, usize, String, u64)> = by_form
        .iter()
        .filter(|(k, _)| k.split_whitespace().count() >= 2)
        .map(|(k, (n, surface, chunk))| (k.clone(), *n, surface.clone(), *chunk))
        .collect();

    // Subsume single-token forms into a UNIQUE multi-token host.
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new(); // host key → alias surfaces
    let mut extra_counts: HashMap<String, usize> = HashMap::new();
    for (k, (n, surface, _)) in &by_form {
        if k.split_whitespace().count() >= 2 {
            continue;
        }
        let mut hosts = multi
            .iter()
            .filter(|(mk, ..)| mk.split_whitespace().any(|w| w == k));
        match (hosts.next(), hosts.next()) {
            (Some((host_key, ..)), None) => {
                aliases.entry(host_key.clone()).or_default().push(surface.clone());
                *extra_counts.entry(host_key.clone()).or_default() += n;
            }
            _ => {} // host-less or ambiguous single token → dropped
        }
    }

    let mut out: Vec<Entity> = Vec::new();
    let mut ordered = multi;
    // Deterministic output order: by descending mention count, then name.
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (i, (key, count, surface, chunk_id)) in ordered.into_iter().enumerate() {
        let total = count + extra_counts.get(&key).copied().unwrap_or(0);
        out.push(Entity {
            id: AtomId::entity(i + 1),
            canonical_name: surface,
            aliases: aliases.get(&key).cloned().unwrap_or_default(),
            entity_type: EntityType::Person,
            first_appearance: ChunkRef::new(format!("chunk:{chunk_id}"), None),
            description: String::new(),
            defining_quote: None,
            // Mention-count-scaled, capped — a seed is corroborated
            // NER signal, not an LLM-judged extraction.
            salience: (0.3 + 0.05 * total as f64).min(0.8) as f32,
            enrichment_depth: EnrichmentDepth::Structural,
            affiliation: None,
            role: None,
            participants: Vec::new(),
            provenance: Default::default(),
            attributes: serde_json::Map::new(),
            concept_kind: None,
        });
    }
    out
}

/// Verbatim member-chunk source recovery for a SMALL Pass A leaf:
/// `(excerpts, figure_sentences)`.
///
/// The leaf summary paraphrases; for an essay leaf of a few chunks
/// the paraphrase compresses out the named binaries / concession
/// phrasings the typed atoms must reproduce verbatim to resolve
/// downstream (measured 2026-06-11). Leaves with more than
/// [`PASS_A_MAX_MEMBER_CHUNKS_FOR_EXCERPTS`] members get none —
/// excerpts of a 20-chunk leaf are no longer representative, and the
/// summary compresses less per chunk there.
///
/// `figure_sentences` (v4) are digit-bearing sentences drawn from the
/// FULL chunk text BEYOND each excerpt window — quantitative evidence
/// (figures, dollar amounts, percentages) tends to sit mid-chunk,
/// past the positional excerpt cut, and an evidence atom whose label
/// paraphrases away the figure loses its identity. The detector is
/// generic (any digit-bearing sentence), deliberately NOT tuned to
/// any bench golden's particular values (overfitting audit,
/// 2026-06-11). Best-effort: any parse or fetch failure returns
/// empty vecs (the v1 input shape).
async fn member_source_for_leaf(
    index: &corpus_engine::index::CorpusIndex,
    leaf: &ConvRaptorNodeRow,
) -> (Vec<String>, Vec<String>) {
    let member_ids: Vec<u64> = leaf
        .direct_member_chunk_ids_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    if member_ids.is_empty() || member_ids.len() > PASS_A_MAX_MEMBER_CHUNKS_FOR_EXCERPTS {
        return (Vec::new(), Vec::new());
    }
    let mut chunks = match index.get_chunks(&member_ids).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                node = %leaf.node_id,
                error = %e,
                "typed_extension: member chunk fetch failed; no excerpts"
            );
            return (Vec::new(), Vec::new());
        }
    };
    // get_chunks returns rows in storage order; keep document order.
    chunks.sort_by_key(|c| c.id);

    let mut excerpts = Vec::new();
    let mut figures = Vec::new();
    for c in &chunks {
        let excerpt: String = c.content.chars().take(PASS_A_EXCERPT_CHARS).collect();
        let tail: String = c.content.chars().skip(PASS_A_EXCERPT_CHARS).collect();
        figures.extend(figure_sentences_from(&tail, PASS_A_FIGURE_SENTENCES_PER_CHUNK));
        let mut text = excerpt;
        if !tail.is_empty() {
            text.push('…');
        }
        if !text.trim().is_empty() {
            excerpts.push(text);
        }
    }
    figures.truncate(PASS_A_MAX_FIGURE_SENTENCES);
    (excerpts, figures)
}

/// Digit-bearing sentences from `text`, up to `cap`, each truncated
/// to [`PASS_A_FIGURE_SENTENCE_CHARS`]. Sentence boundary = `.`/`!`/`?`
/// followed by whitespace (or end of text) — a bare `.` split would
/// sever decimal figures ("$224.8") mid-number, mangling exactly the
/// values this recovery exists to carry. Naive beyond that — good
/// enough for recall; the LLM re-reads the sentence anyway.
fn figure_sentences_from(text: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && out.len() < cap {
        let b = bytes[i];
        let at_boundary = matches!(b, b'.' | b'!' | b'?')
            && (i + 1 >= bytes.len() || bytes[i + 1].is_ascii_whitespace());
        if at_boundary || i + 1 == bytes.len() {
            let end = (i + 1).min(bytes.len());
            if let Some(raw) = text.get(start..end) {
                let s = raw.trim();
                if s.len() >= 20 && s.chars().any(|c| c.is_ascii_digit()) {
                    let mut sentence: String =
                        s.chars().take(PASS_A_FIGURE_SENTENCE_CHARS).collect();
                    if s.chars().count() > PASS_A_FIGURE_SENTENCE_CHARS {
                        sentence.push('…');
                    }
                    out.push(sentence);
                }
            }
            start = end;
        }
        i += 1;
    }
    out
}

fn collect_member_quotes_for_theme(
    theme: &VaultThemeRow,
    leaves_by_doc: &HashMap<String, Vec<&ConvRaptorNodeRow>>,
) -> Vec<pass::ParsedQuoteSpan> {
    let member_doc_ids: Vec<String> =
        serde_json::from_str(&theme.member_source_doc_ids_json).unwrap_or_default();
    if member_doc_ids.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<pass::ParsedQuoteSpan> = Vec::new();
    'outer: for doc_id in &member_doc_ids {
        let Some(doc_leaves) = leaves_by_doc.get(doc_id) else {
            continue;
        };
        for leaf in doc_leaves {
            for span in pass::parse_quote_spans(&leaf.quote_spans_json) {
                out.push(span);
                if out.len() >= PASS_B_QUOTE_CAP_PER_THEME {
                    break 'outer;
                }
            }
        }
    }
    out
}
