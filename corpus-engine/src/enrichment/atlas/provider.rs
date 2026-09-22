// SPDX-License-Identifier: AGPL-3.0-or-later
//! The walk provider's CLASS-COMPOSITE opener.
//!
//! `AtlasProvider` (the trait), the atom-class graph provider and the
//! declaration vocabulary moved to the `corpus-engine-atlas-reader` leaf
//! (FIVE_PROGRAMS §12 decision 1); they are re-exported below at the
//! historical paths. This file keeps what only the ENGINE can do: composing
//! the two backend classes. `wikipedia_columnar` — the wiki-class provider —
//! is corpus-engine's, so the "try atom-class, else wiki-class" decision
//! lives here and the leaf never names it (a corpus with neither store
//! errors naming BOTH absences, ARCH §18.3).

pub use std::sync::Arc;

pub use corpus_engine_atlas_reader::provider::{AtlasProvider, NavigationSource};

use corpus_engine_atlas_reader::context::{
    open_and_attach_ann_seed_table, open_ann_seed_table, AtlasGraph,
};
use corpus_engine_atlas_reader::store::run_blocking;

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
        crate::enrichment::atlas::context::read_section_rows(&atlas_dir),
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

    if !crate::wikipedia_graph_present(indexes_dir, atlas_corpus_id) {
        return Err(format!(
            "no store the walk can read for `{atlas_corpus_id}`: {atom_err}; and no wiki-class \
             store (articles.lance + edges.lance) under {}",
            atlas_dir.display()
        ));
    }

    match crate::WikiAtlasProvider::open(&atlas_dir, atlas_corpus_id).await {
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
