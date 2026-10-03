// SPDX-License-Identifier: AGPL-3.0-or-later
//! Typed-extension LLM pass over RAPTOR cluster summaries.
//!
//! Spec: `svrn/docs/specs/TYPED_EXTENSION_PASS.md`.
//!
//! Runs inside `FolderTieredProvider::finalize_corpus` *after*
//! `run_vault_synthesis`. Reads `conv_raptor_nodes` (Pass A inputs)
//! and `vault_themes` (Pass B inputs), drives one LLM call per leaf /
//! theme against ingest's argumentative typed-extension schema, and hands
//! the responses to ingest through the atlas port
//! (`AtlasPort::write_typed_extension`), which projects them through the
//! resolver, rewrites the sequential atom ids to content-hash ids so re-runs
//! are idempotent, and writes `{atlas_dir}/atoms.json` + `atoms.meta.json`.
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

use corpus_engine_atlas_reader::citation::SourceCitation;
use corpus_engine_atlas_reader::ports::{ArgumentativeResponse, AtlasPort};
use corpus_index::error::{Error, Result};
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, ConvTieredReader, VaultThemeRow};
use sovereign_core::traits::InferenceProvider;
use sovereign_store::sqlite::SqliteStateStore;

mod harvest;
mod manifest;
mod pass;

#[cfg(test)]
mod tests;

pub use manifest::{TypedExtensionManifest, MANIFEST_FILENAME, MANIFEST_SCHEMA_VERSION};

use harvest::{
    build_person_seed_entities, collect_member_quotes_for_theme, member_source_for_leaf,
};

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
    atlas: &dyn AtlasPort,
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
    let index = corpus_index::index::CorpusIndex::open(atlas_dir.parent().unwrap_or(atlas_dir))
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

    let mut sections: Vec<ArgumentativeResponse> = Vec::with_capacity(leaves.len() + themes.len());
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
            atlas,
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
        match pass::pass_b_one_theme_with_excerpts(
            atlas,
            corpus_id,
            theme,
            &member_quotes,
            inference,
        )
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

    // Resolve, content-hash, count and write: ingest's, through the port.
    // The embedder is the QUERY-side adapter, so the seed table shares the
    // vector space `atlas_navigate_ann` queries it in.
    let atoms_per_kind = atlas.write_typed_extension(
        corpus_id,
        atlas_dir,
        &sections,
        person_seeds,
        &citations,
        sovereign_core::embed_fn::inference_to_embed_query_fn(Arc::clone(inference)),
    )?;

    let total_atoms: u32 = atoms_per_kind.values().copied().sum();

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
