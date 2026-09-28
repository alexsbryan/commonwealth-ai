// SPDX-License-Identifier: AGPL-3.0-or-later
//! Installed-index read port: list them, open one. Keeps callers off corpus-engine.

use std::path::Path;

use async_trait::async_trait;

use crate::index::CorpusIndex;
use crate::recipe::CatalogConfig;
use crate::types::{BuiltinCorpus, IndexInfo};
use crate::Result;

/// Read access to the indexes installed on this machine.
#[async_trait]
pub trait IndexSource: Send + Sync {
    /// Installed and searchable (`indexes_built`); drops are reported, not silent.
    async fn usable_indexes(&self) -> Result<Vec<IndexInfo>>;

    /// Open `path`. Implementors MUST serve from a handle cache — reopen is ~5s on wikipedia.
    async fn open_index(&self, path: &Path) -> Result<CorpusIndex>;
}

/// The whole read surface the svrn turn path calls on the engine: `IndexSource`
/// plus embed, the unfiltered listing, open-by-id, the index root, the
/// foreground lease and the registry catalog. ONE port — list/open stay
/// `IndexSource`'s (five-programs-24 fork (1), fp-64).
#[async_trait]
pub trait CorpusReadPort: IndexSource {
    /// Embed `text` with the model the indexes were built with.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Every installed index, `indexes_built` or not.
    async fn installed_indexes(&self) -> Result<Vec<IndexInfo>>;

    /// Open the index for `corpus_id` under [`Self::index_dir`] (cached, as `open_index`).
    async fn open_index_for_corpus(&self, corpus_id: &str) -> Result<CorpusIndex>;

    /// The directory every corpus index lives under.
    fn index_dir(&self) -> &Path;

    /// "A person is waiting" until dropped; `None` when no signal is installed.
    fn foreground_lease(&self) -> Option<corpus_engine_yield::ForegroundLease>;

    /// The registry catalog, one row per entry.
    fn builtin_corpora(&self) -> Vec<BuiltinCorpus>;

    /// The directory the recipes live under (the `[authority]` blocks the
    /// sec_facts tool reads).
    fn recipes_dir(&self) -> &Path;

    /// `corpus_id`'s recipe `[catalog]` block; `None` when the recipe has
    /// none or does not resolve.
    async fn catalog_config(&self, corpus_id: &str) -> Option<CatalogConfig>;

    /// Installed corpora that carry field-model tables — what the
    /// epistemic tools consult.
    async fn enriched_corpus_ids(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for info in self.installed_indexes().await? {
            if let Ok(index) = CorpusIndex::open(&info.path).await {
                if index.has_field_model_tables().await {
                    out.push(info.corpus_id);
                }
            }
        }
        Ok(out)
    }
}
