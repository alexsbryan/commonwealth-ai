// SPDX-License-Identifier: AGPL-3.0-or-later
//! Installed-index read port: list them, open one. Keeps callers off corpus-engine.

use std::path::Path;

use async_trait::async_trait;

use crate::index::CorpusIndex;
use crate::types::IndexInfo;
use crate::Result;

/// Read access to the indexes installed on this machine.
#[async_trait]
pub trait IndexSource: Send + Sync {
    /// Installed and searchable (`indexes_built`); drops are reported, not silent.
    async fn usable_indexes(&self) -> Result<Vec<IndexInfo>>;

    /// Open `path`. Implementors MUST serve from a handle cache — reopen is ~5s on wikipedia.
    async fn open_index(&self, path: &Path) -> Result<CorpusIndex>;
}
