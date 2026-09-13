// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether a corpus can serve retrieval, decided from its own meta in one place.

use super::IndexMeta;

/// `indexes_built` alone was not the truth. It is a second write that callers
/// had to remember after `build_indexes` returned, and four did not (catalog
/// install, catalog ingest, the harness runner, `write.rs`'s partial rebuilds),
/// so corpora with every sub-index built were refused as "not finished
/// building" — `wikipedia-fetched` and `commonwealth-ai-architecture` on the
/// 2026-09-13 chaos soak. All three sub-phase checkpoints set, with no ingest
/// writing, records the same fact. A stopped ingest that never built anything
/// (all false) stays unsearchable, and `reset_for_resume` clears all four.
pub(super) fn indexes_searchable(meta: &IndexMeta) -> bool {
    meta.indexes_built
        || (meta.vector_index_built
            && meta.content_fts_built
            && meta.title_fts_built
            && !meta.ingestion_in_progress)
}
