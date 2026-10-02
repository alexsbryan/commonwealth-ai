// SPDX-License-Identifier: AGPL-3.0-or-later
//! The walk provider's CLASS-COMPOSITE opener — the ONE place that decides
//! which store a corpus's walk reads.
//!
//! It composes the two backend classes, the atom-class graph
//! ([`AtlasGraph`]) and the wiki-class provider
//! ([`crate::wikipedia_columnar::WikiAtlasProvider`]): "try atom-class, else
//! wiki-class", and a corpus with neither store errors naming BOTH absences
//! (ARCH §18.3). Both classes are raw reads, so the opener lives in this leaf
//! (FIVE_PROGRAMS §12 decision 1) and any program walks an atlas without
//! linking corpus-engine, which re-exports it at its historical path
//! (phase-b pb-corpus-mcp-reads).

use std::sync::Arc;

use crate::context::{open_and_attach_ann_seed_table, open_ann_seed_table, AtlasGraph};
use crate::provider::AtlasProvider;
use crate::store::run_blocking;

pub async fn open_walk_provider(
    indexes_dir: &std::path::Path,
    atlas_corpus_id: &str,
) -> Result<Arc<dyn AtlasProvider>, String> {
    let atlas_dir = indexes_dir
        .join(atlas_corpus_id)
        .join(understanding_vocab::read::ATLAS_DIRNAME);
    let started = std::time::Instant::now();

    let atom_err = match AtlasGraph::load_from_disk(
        atlas_corpus_id,
        &atlas_dir,
        crate::context::read_section_rows(&atlas_dir),
    ) {
        Ok(g) => {
            let g = open_and_attach_ann_seed_table(atlas_corpus_id, &atlas_dir, g).await;
            tracing::info!(
                corpus = atlas_corpus_id,
                backend = "atom-class",
                seed_table = g.has_ann_seed_table(),
                load_ms = started.elapsed().as_millis(),
                "walk provider: opened"
            );
            return Ok(Arc::new(g) as Arc<dyn AtlasProvider>);
        }
        Err(e) => e,
    };

    if !crate::wikipedia_columnar::wikipedia_graph_present(indexes_dir, atlas_corpus_id) {
        return Err(format!(
            "no store the walk can read for `{atlas_corpus_id}`: {atom_err}; and no wiki-class \
             store (articles.lance + edges.lance) under {}",
            atlas_dir.display()
        ));
    }

    match crate::wikipedia_columnar::WikiAtlasProvider::open(&atlas_dir, atlas_corpus_id).await {
        Ok(p) => {
            let ann = open_ann_seed_table(atlas_corpus_id, &atlas_dir).await;
            let p = match ann {
                Some(a) => p.with_ann_seed_table(a),
                None => p,
            };
            tracing::info!(
                corpus = atlas_corpus_id,
                backend = "wiki-class",
                atoms = p.atom_count(),
                edges = p.edge_count(),
                seed_table = p.has_ann_seed_table(),
                load_ms = started.elapsed().as_millis(),
                "walk provider: opened"
            );
            Ok(Arc::new(p) as Arc<dyn AtlasProvider>)
        }
        Err(e) => Err(format!(
            "wiki-class store present but unusable for `{atlas_corpus_id}`: {e}"
        )),
    }
}

/// [`open_walk_provider`] for a sync caller, bridged through the atlas
/// module's ONE async-from-sync bridge. Lifecycle time only — corpus load,
/// never the hot query path.
pub fn open_walk_provider_blocking(
    indexes_dir: &std::path::Path,
    atlas_corpus_id: &str,
) -> Result<Arc<dyn AtlasProvider>, String> {
    run_blocking(open_walk_provider(indexes_dir, atlas_corpus_id))
}
