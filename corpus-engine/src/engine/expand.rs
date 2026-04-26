//! Expand an already-installed corpus by relaxing its filter scope.
//!
//! `expand_corpus` is the in-place growth path for layered corpora —
//! e.g. promoting `wikipedia` from "Core" (top-100K by pageview rank ∪
//! Vital Articles) to the full 6.7M-article dump without rebuilding the
//! existing index.
//!
//! ## Mechanics
//!
//! 1. Load the recipe and overlay the new (relaxed) filter pipeline.
//!    The `[filter_override]` block is persisted to `_corpus_meta.json`
//!    so a restart mid-expansion resumes with the right scope rather
//!    than reverting to the recipe's narrower filter.
//! 2. Open the existing index and read every distinct
//!    `source_doc_id` already present. This becomes the "skipset" for
//!    the expansion run.
//! 3. Re-run the ingest pipeline with `iter_pos` reset to 0 (the
//!    iterator order may differ post-filter-change so the
//!    position-based resume cursor is meaningless here) and the
//!    skipset wired into the doc loop. The filter pipeline drops
//!    docs the new scope rejects; the skipset drops docs the old scope
//!    already indexed. Only **net-new** documents reach embedding.
//! 4. After ingest completes, rebuild the IVF-PQ vector index — the
//!    centroids trained at the original scale become suboptimal once
//!    millions of new vectors land. Rebuild runs as a background
//!    phase; the index remains searchable throughout.
//! 5. Clear `[filter_override]` and update `[scope]` to reflect the
//!    new pipeline. If the new pipeline is empty, mark
//!    `expandable=false`.
//!
//! ## Why a separate path?
//!
//! Hooking delta detection directly into `ingest()` would force every
//! recipe — including code corpora and one-shot SQLite views — to
//! pay the cost of "is there already an index for this corpus" and
//! pick a strategy. Concentrating the delta logic in `expand_corpus`
//! keeps the standard `ingest()` linear and unchanged for the 99% of
//! cases.

use std::sync::Arc;

use crate::engine::CorpusEngine;
use crate::error::{Error, Result};
use crate::filters::{compute_signature, ComposeMode, FilterConfig};
use crate::index::{CorpusIndex, FilterOverride, ScopeMeta};
use crate::progress::ProgressCallback;
use crate::recipe::FilterModeConfig;
use crate::types::IngestResult;

impl CorpusEngine {
    /// Expand `corpus_id` to a relaxed filter scope (or remove the
    /// filter entirely with `new_filters` empty + `mode = Any`).
    ///
    /// Idempotent: calling with the same scope twice is a no-op (the
    /// signature comparison detects no change). Resumable: a restart
    /// mid-expansion picks up from where it left off thanks to the
    /// persisted `[filter_override]` block.
    pub async fn expand_corpus(
        &self,
        corpus_id: &str,
        new_filters: Vec<FilterConfig>,
        new_mode: ComposeMode,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        // ── 1. Load recipe and identify the existing index ───────
        let mut recipe = self.load_recipe(corpus_id).await?;

        let index_path = self.partition_path(corpus_id);
        if !index_path.exists() {
            return Err(Error::IndexNotFound(format!(
                "expand_corpus: no installed index for `{corpus_id}` at {}",
                index_path.display()
            )));
        }

        let new_signature = compute_signature(&new_filters, new_mode);
        let existing_index = CorpusIndex::open(&index_path).await?;
        let existing_scope = existing_index.read_scope().ok().flatten();
        let existing_signature = existing_scope
            .as_ref()
            .map(|s| s.filter_signature.clone())
            .unwrap_or_default();
        let existing_chunks = existing_index.chunk_count().await.unwrap_or(0);

        if new_signature == existing_signature && existing_chunks > 0 {
            tracing::info!(
                corpus = corpus_id,
                signature = %new_signature,
                "expand_corpus: scope unchanged, nothing to do"
            );
            return Ok(IngestResult {
                corpus_id: corpus_id.to_string(),
                chunks_created: existing_chunks,
                index_size_bytes: 0,
                duration_secs: 0,
                docs_skipped: 0,
            });
        }

        // ── 2. Snapshot already-indexed source_doc_ids ───────────
        //
        // For Wikipedia Core (~150K accepted articles → ~5M chunks)
        // this loads ~150K strings (~5–10 MB) in a few seconds. The
        // cost is amortised over the multi-minute expansion.
        let already_indexed = if existing_chunks > 0 {
            tracing::info!(
                corpus = corpus_id,
                existing_chunks,
                "expand_corpus: snapshotting already-indexed source_doc_ids"
            );
            let set = existing_index.list_indexed_source_doc_ids().await?;
            tracing::info!(
                corpus = corpus_id,
                already_indexed = set.len(),
                "expand_corpus: skipset built"
            );
            Some(Arc::new(set))
        } else {
            None
        };

        // ── 3. Persist filter_override + overlay on recipe ───────
        existing_index.write_filter_override(Some(FilterOverride {
            filters: new_filters.clone(),
            mode: new_mode,
        }))?;

        recipe.filters = new_filters.clone();
        recipe.filter_mode = FilterModeConfig { mode: new_mode };

        // ── 4. Run the pipeline with skipset + reset iter_pos ────
        //
        // We intentionally call the inner method directly rather than
        // public `ingest()` so we can pass the skipset and so the
        // caller's `_downloads` / `_corpus_meta.json` resume cursor is
        // bypassed (it's keyed to the prior filter scope's iteration
        // order).
        if let Err(e) = existing_index.update_committed_iter_pos(0) {
            tracing::warn!(
                corpus = corpus_id,
                "expand_corpus: failed to reset committed_iter_pos: {e}"
            );
        }

        // Drop the read handle before re-running ingest, which opens
        // its own write handle to the same LanceDB table.
        drop(existing_index);

        let summary = self
            .run_expansion_ingest(&recipe, &index_path, &progress, already_indexed)
            .await?;

        // ── 5. Rebuild vector index (IVF-PQ centroids) ───────────
        //
        // Search remains live during this phase — LanceDB tolerates
        // concurrent reads. Surfaced to the UI as the `optimizing_index`
        // progress phase.
        let index = CorpusIndex::open(&index_path).await?;
        if index.chunk_count().await.unwrap_or(0) >= 256 {
            tracing::info!(corpus = corpus_id, "expand_corpus: rebuilding IVF-PQ");
            if let Err(e) = index.rebuild_vector_index().await {
                // Don't fail the whole expansion on rebuild error —
                // search degrades but stays functional.
                tracing::warn!(
                    corpus = corpus_id,
                    "expand_corpus: rebuild_vector_index failed (search may have degraded recall): {e}"
                );
            }
        }

        // ── 6. Clear override; update scope ──────────────────────
        index.write_filter_override(None)?;
        let new_scope = if new_filters.is_empty() {
            None
        } else {
            Some(ScopeMeta {
                filter_descriptions: vec![format!(
                    "expanded scope ({} filters, mode={:?})",
                    new_filters.len(),
                    new_mode
                )],
                filter_signature: new_signature,
                expandable: true,
            })
        };
        index.write_scope(new_scope)?;

        Ok(summary)
    }

    /// Convenience wrapper for the common case: relax the filter
    /// entirely (e.g. promote Wikipedia Core → Wikipedia Full).
    pub async fn expand_corpus_to_full(
        &self,
        corpus_id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        self.expand_corpus(corpus_id, Vec::new(), ComposeMode::Any, progress)
            .await
    }
}

// ---------------------------------------------------------------------------
// Glue helper — `ingest_inner_with_skipset` lives in `ingest.rs` and
// is `pub(crate)` so this module can call it directly without an
// indirection layer.
// ---------------------------------------------------------------------------

impl CorpusEngine {
    async fn run_expansion_ingest(
        &self,
        recipe: &crate::recipe::Recipe,
        index_path: &std::path::Path,
        progress: &Option<ProgressCallback>,
        already_indexed: Option<Arc<std::collections::HashSet<String>>>,
    ) -> Result<IngestResult> {
        self.ingest_inner_with_skipset(recipe, index_path, progress, None, already_indexed)
            .await
    }
}

