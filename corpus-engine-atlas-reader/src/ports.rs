// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas family's port: what svrn's atlas tools ask ingest to write or
//! derive (pb-ingest-dial-tools-atlas, phase-b-43 family (4)).
//!
//! The leaf still writes nothing: this module holds the trait and the plain
//! values its methods name, and ingest's crate implements it. Each method is
//! addressed by path, the way the free functions it stands for were. svrn
//! holds an `Arc<dyn AtlasPort>` handed to it by whoever composed ingest,
//! beside the ports in `corpus_index::ingest_port` (FIVE_PROGRAMS §2c).

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use understanding_vocab::atoms::AtomEnvelope;
use understanding_vocab::edges::Edge;

use crate::raptor_read::RaptorSummaryRow;
use crate::summary::AtlasSummary;

/// The atlas family's port. Ingest implements it once.
#[async_trait]
pub trait AtlasPort: Send + Sync {
    /// The atlas's `_summary.json`, computed and persisted when stale.
    /// `Ok(None)` when the atlas has no `atoms.json`.
    fn atlas_summary(&self, atlas_dir: &Path) -> io::Result<Option<AtlasSummary>>;

    /// Is the ANN seed table current for this atlas's atoms and population?
    fn ann_table_is_fresh(&self, atlas_dir: &Path) -> bool;

    /// Phase 7: detect the deterministic gaps over `atoms` and `edges` and
    /// write `gaps.json`. Returns the gap count and the written path.
    fn write_deterministic_gaps(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
        edges: &[Edge],
    ) -> io::Result<(usize, PathBuf)>;

    /// Phase 6, deterministic half: select the tension candidates over
    /// `atoms` and write `tension_candidates.json`. Returns the candidate
    /// count and the written path.
    fn write_tension_candidates(
        &self,
        atlas_dir: &Path,
        atoms: &[AtomEnvelope],
    ) -> io::Result<(usize, PathBuf)>;

    /// (Re)build `<corpus_dir>/raptor_summaries.lance` and its freshness
    /// sidecar from `rows`, stamped `source_version`. Returns rows written.
    async fn build_raptor_index(
        &self,
        corpus_dir: &Path,
        rows: &[RaptorSummaryRow],
        source_version: i64,
    ) -> corpus_index::Result<usize>;
}
