// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon/desktop bootstrap for the shared GLiNER extractor. Moved out of
//! sovereign-tools' `enrichment_bootstrap` (2026-07-17) with the rest of the
//! GLiNER surface; the non-gliner folder-tiered helpers stay in sovereign-tools.

use std::path::Path;
use std::sync::Arc;

use corpus_engine::enrichment::tiered::ChunkEntityExtractor;

use crate::chunk_extractor::GlinerChunkExtractor;
use crate::labeled::{configured_model_id, load_labeled_extractor, LabeledEntityExtractor};

/// Load the shared GLiNER per-chunk entity extractor once (the ONNX model
/// is ~150 MB for v1, ~795 MB for GLiNER2; one load only). Returns the raw
/// handle (for a NoteStore T2 `GlinerFn` adapter, when the caller wires
/// notes) alongside the trait-object wrapper (for the engine's tiered
/// runner and the folder driver). Both `None` when the model isn't
/// installed or the state store can't be opened — tiered ingest then falls
/// back to RAPTOR-derived entities.
///
/// **Which generation runs is [`configured_model_id`]'s call, not this
/// function's** (P2.1). The raw handle is the generation-agnostic
/// [`LabeledEntityExtractor`] for the same reason: typing it as v1's
/// concrete `GlinerExtractor` would have silently dropped note-side NER
/// the moment the ingest path moved to GLiNER2.
pub fn load_gliner_extractor(
    data_dir: &Path,
) -> (
    Option<Arc<dyn LabeledEntityExtractor>>,
    Option<Arc<dyn ChunkEntityExtractor>>,
) {
    let model_id = configured_model_id();
    if !crate::gliner_ner::probe_model_available(&model_id) {
        let root = crate::gliner_ner::models_root().join(&model_id);
        tracing::info!(
            model = %model_id,
            expected_path = %root.display(),
            "enrichment_bootstrap: GLiNER model not installed — per-chunk entity extraction disabled. Tiered ingest will use RAPTOR-derived entities only."
        );
        return (None, None);
    }

    let store_path = data_dir.join("sovereign.db");
    let store = match sovereign_store::sqlite::SqliteStateStore::open(&store_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::warn!(
                store_path = %store_path.display(),
                error = %e,
                "enrichment_bootstrap: cannot open state store for entity extractor — skipping"
            );
            return (None, None);
        }
    };

    match load_labeled_extractor(&model_id, None) {
        Ok(extractor) => {
            tracing::info!(
                model = %model_id,
                generation = ?extractor.generation(),
                "enrichment_bootstrap: GLiNER extractor loaded (shared across engine + folder driver + NoteStore T2)"
            );
            let chunk_entity_extractor =
                Arc::new(GlinerChunkExtractor::new(store, Arc::clone(&extractor)))
                    as Arc<dyn ChunkEntityExtractor>;
            (Some(extractor), Some(chunk_entity_extractor))
        }
        Err(e) => {
            tracing::warn!(
                model = %model_id,
                error = %e,
                "enrichment_bootstrap: GLiNER load failed — tiered ingest will fall back to RAPTOR-only entities"
            );
            (None, None)
        }
    }
}
