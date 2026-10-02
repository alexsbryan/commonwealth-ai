// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon/desktop bootstrap for the shared GLiNER extractor. Moved out of
//! sovereign-tools' `enrichment_bootstrap` (2026-07-17) with the rest of the
//! GLiNER surface; the non-gliner folder-tiered helpers stay in sovereign-tools.

use std::sync::Arc;

use crate::labeled::{configured_model_id, load_labeled_extractor, LabeledEntityExtractor};

/// Load the shared GLiNER entity extractor once (the ONNX model is ~150 MB
/// for v1, ~795 MB for GLiNER2; one load only). `None` when the model isn't
/// installed — tiered ingest then falls back to RAPTOR-derived entities.
///
/// This is the NER served kind's loader (`sovereign_compute::ner::NER`,
/// pb-serving-ner). It no longer builds the per-chunk adapter: that is
/// corpus-engine's `GlinerChunkExtractor`, and the host that owns the
/// chunk-entity store builds it over this handle.
///
/// **Which generation runs is [`configured_model_id`]'s call, not this
/// function's** (P2.1). The handle is the generation-agnostic
/// [`LabeledEntityExtractor`] for the same reason: typing it as v1's
/// concrete `GlinerExtractor` would have silently dropped note-side NER
/// the moment the ingest path moved to GLiNER2.
pub fn load_gliner_extractor() -> Option<Arc<dyn LabeledEntityExtractor>> {
    let model_id = configured_model_id();
    if !crate::gliner_ner::probe_model_available(&model_id) {
        let root = crate::gliner_ner::models_root().join(&model_id);
        tracing::info!(
            model = %model_id,
            expected_path = %root.display(),
            "enrichment_bootstrap: GLiNER model not installed — per-chunk entity extraction disabled. Tiered ingest will use RAPTOR-derived entities only."
        );
        return None;
    }

    match load_labeled_extractor(&model_id, None) {
        Ok(extractor) => {
            tracing::info!(
                model = %model_id,
                generation = ?extractor.generation(),
                "enrichment_bootstrap: GLiNER extractor loaded (shared across engine + folder driver + NoteStore T2)"
            );
            Some(extractor)
        }
        Err(e) => {
            tracing::warn!(
                model = %model_id,
                error = %e,
                "enrichment_bootstrap: GLiNER load failed — tiered ingest will fall back to RAPTOR-only entities"
            );
            None
        }
    }
}
