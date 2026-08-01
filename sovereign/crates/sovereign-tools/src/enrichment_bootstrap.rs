// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared construction of the in-process tiered-enrichment stack.
//!
//! Folder / Obsidian / conversation enrichment runs through a
//! [`FolderTieredProvider`] over the canonical state store
//! (`<data_dir>/sovereign.db`) plus an optional shared GLiNER chunk-entity
//! extractor. Two consumers must wire this stack identically or folder
//! enrichment silently diverges:
//!
//!   * the standalone daemon (`sovereign-cli-daemon` bootstrap), and
//!   * the desktop app's embedded daemon (`sovereign-desktop` state init).
//!
//! Before this module the desktop wired *neither* the engine-side tiered
//! provider nor the folder driver's [`TieredDeps`], so
//! `LocalCorpusManager::enable_enrichment` fell back to the legacy
//! `sovereign-cli enrich` subprocess — which does not exist in a shipped
//! bundle (exit 127) and, even in a dev tree, left the enrichment state
//! file stranded at `Starting`. Centralising the construction here gives
//! both daemons the same in-process path.
//!
//! All three helpers open the state store read/write; SQLite WAL makes the
//! extra handles safe alongside the manager's own store handle. A store
//! that can't be opened is non-fatal — the caller degrades (RAPTOR-only
//! entities, or the legacy fallback) rather than failing boot.

use std::path::Path;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;

use corpus_engine::enrichment::tiered::{ChunkEntityExtractor, TieredEnrichmentProvider};

use crate::conv_tiered_provider::{FolderTieredProvider, IndexDirResolver, StaticIndexDirResolver};
use crate::local_corpus::watched::enrich::TieredDeps;

/// Construct the `FolderTieredProvider` over the canonical state store,
/// resolving per-corpus index dirs under `<data_dir>/indexes`. This is the
/// engine-side tiered provider (wired via
/// `CorpusEngine::with_tiered_provider`) AND the provider the folder driver
/// holds inside [`TieredDeps`]. `None` when the store can't be opened —
/// tiered enrichment then degrades to dispatch-plan-only / legacy fallback.
///
/// `FolderTieredProvider` is the sole provider (the conversation-only
/// `ConvTieredProvider` was deleted 2026-07-30, zero construction sites): its
/// `finalize_corpus` override runs the vault-wide synthesis pass needed for
/// `vault_themes`, and its `enrich_conversation` accepts an arbitrary
/// `conv_uuid` so it serves both conv corpora (chat-uuid grouping) and
/// folder corpora (`source_doc_id` grouping).
pub fn build_folder_tiered_provider(
    data_dir: &Path,
    provider: Arc<dyn InferenceProvider>,
) -> Option<Arc<dyn TieredEnrichmentProvider>> {
    let db_path = data_dir.join("sovereign.db");
    match sovereign_store::sqlite::SqliteStateStore::open(&db_path) {
        Ok(store) => {
            let store_arc = Arc::new(store);
            let indexes_root = data_dir.join("indexes");
            let resolver: Arc<dyn IndexDirResolver> =
                Arc::new(StaticIndexDirResolver { indexes_root });
            // Memory corpora (vault notes + imported conversations) build
            // EXTRACTIVE trees by default (T1 P1.1, flipped 2026-07-31):
            // summaries are verbatim centroid-ranked member sentences, so
            // nothing the AI writes into the memory tier can assert what
            // the source doesn't. Flip condition met by the production-seam
            // A/B on the sep banks — |B−A| = −0.0125 summarize (band
            // ±0.025) and 0.0000 obscure (band ±0.0167), rawindex guard
            // 0.0000 (runs/prodAB, 2026-07-31). Attached documents keep
            // abstractive (fluency is the product there) — their path pins
            // the mode explicitly in `document_asset`.
            let prov = FolderTieredProvider::new(store_arc, provider)
                .with_index_dir_resolver(resolver)
                .with_summary_mode(crate::raptor_atlas::SummaryMode::Extractive);
            Some(Arc::new(prov) as Arc<dyn TieredEnrichmentProvider>)
        }
        Err(e) => {
            tracing::warn!(
                db_path = %db_path.display(),
                error = %e,
                "enrichment_bootstrap: cannot open state store — folder enrichment will fall back to the legacy subprocess"
            );
            None
        }
    }
}

/// Build the folder-driver [`TieredDeps`]: the shared
/// [`build_folder_tiered_provider`] plus the (optional) GLiNER extractor.
/// Installed on the `LocalCorpusManager` via `set_tiered_deps` so
/// `enable_enrichment` routes through `start_tiered_build` instead of the
/// legacy subprocess. `None` (legacy fallback) when the state store can't
/// be opened.
pub fn build_folder_tiered_deps(
    data_dir: &Path,
    provider: Arc<dyn InferenceProvider>,
    chunk_entity_extractor: Option<Arc<dyn ChunkEntityExtractor>>,
) -> Option<TieredDeps> {
    let tiered_provider = build_folder_tiered_provider(data_dir, provider)?;
    tracing::info!(
        "enrichment_bootstrap: folder tiered deps constructed — FolderTieredProvider wired"
    );
    Some(TieredDeps {
        tiered_provider,
        gliner_extractor: chunk_entity_extractor,
    })
}
