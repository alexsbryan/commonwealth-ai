// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon/desktop bootstrap for the shared GLiNER extractor. Moved out of
//! sovereign-tools' `enrichment_bootstrap` (2026-07-17) with the rest of the
//! GLiNER surface; the non-gliner folder-tiered helpers stay in sovereign-tools.

use std::path::Path;
use std::sync::Arc;

use corpus_engine::enrichment::tiered::ChunkEntityExtractor;

use crate::chunk_extractor::GlinerChunkExtractor;
use crate::gliner_ner::GlinerExtractor;

/// Load the shared GLiNER per-chunk entity extractor once (the ONNX model
/// is ~150 MB; one load only). Returns the raw handle (for a NoteStore T2
/// `GlinerFn` adapter, when the caller wires notes) alongside the
/// trait-object wrapper (for the engine's tiered runner and the folder
/// driver). Both `None` when the model isn't installed or the state store
/// can't be opened — tiered ingest then falls back to RAPTOR-derived
/// entities.
pub fn load_gliner_extractor(
    data_dir: &Path,
) -> (
    Option<Arc<GlinerExtractor>>,
    Option<Arc<dyn ChunkEntityExtractor>>,
) {
    let mut gliner_raw: Option<Arc<GlinerExtractor>> = None;
    let chunk_entity_extractor: Option<Arc<dyn ChunkEntityExtractor>> = {
        let model_id = crate::gliner_ner::DEFAULT_MODEL_ID;
        if crate::gliner_ner::probe_model_available(model_id) {
            let store_path = data_dir.join("sovereign.db");
            match sovereign_store::sqlite::SqliteStateStore::open(&store_path) {
                Ok(store_for_extractor) => match GlinerExtractor::new_default() {
                    Ok(ex) => {
                        tracing::info!(
                            model = model_id,
                            "enrichment_bootstrap: GLiNER extractor loaded (shared across engine + folder driver + NoteStore T2)"
                        );
                        let ex_arc = Arc::new(ex);
                        gliner_raw = Some(Arc::clone(&ex_arc));
                        Some(Arc::new(GlinerChunkExtractor::new(
                            Arc::new(store_for_extractor),
                            ex_arc,
                        )) as Arc<dyn ChunkEntityExtractor>)
                    }
                    Err(e) => {
                        tracing::warn!(
                            model = model_id,
                            error = %e,
                            "enrichment_bootstrap: GLiNER load failed — tiered ingest will fall back to RAPTOR-only entities"
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        store_path = %store_path.display(),
                        error = %e,
                        "enrichment_bootstrap: cannot open state store for entity extractor — skipping"
                    );
                    None
                }
            }
        } else {
            let root = crate::gliner_ner::models_root().join(model_id);
            tracing::info!(
                model = model_id,
                expected_path = %root.display(),
                "enrichment_bootstrap: GLiNER model not installed — per-chunk entity extraction disabled. Tiered ingest will use RAPTOR-derived entities only."
            );
            None
        }
    };
    (gliner_raw, chunk_entity_extractor)
}
