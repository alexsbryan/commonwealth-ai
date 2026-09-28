// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's write ports, beside the read port in [`crate::source`].
//!
//! One narrow port per svrn tool family, each naming only what that family
//! calls (phase-b-33 item 7). Ingest's engine implements every one, and the
//! composition root that runs both programs hands them to svrn (FIVE_PROGRAMS
//! §2c; phase-b-30 Group 2, F5 (b)). svrn with no ingest composed holds none
//! and says so by name.

use async_trait::async_trait;
use sovereign_contracts::daemon_wire::IngestProgress;

use crate::source::CorpusReadPort;
use crate::Error;

/// Thread-safe ingest progress callback. `Sync` because an ingest holds an
/// `&Option<ProgressCallback>` across `.await` points.
pub type ProgressCallback = Box<dyn Fn(IngestProgress) + Send + Sync>;

/// One catalog work to ingest: the catalog's content recipe, pointed at the
/// work's download url and written as `staging_corpus_id`.
#[derive(Debug, Clone)]
pub struct CatalogWork {
    /// The catalog's `[catalog] content_recipe` id.
    pub content_recipe: String,
    /// The catalog corpus; the ingested corpus's parent unless the content
    /// recipe declares its own.
    pub catalog_corpus_id: String,
    /// The work's url (`download_url_template` with the id substituted).
    pub download_url: String,
    /// The corpus the ingest writes.
    pub staging_corpus_id: String,
    /// `Some(target)` folds the staging corpus into the catalog's shared
    /// `target_corpus_id` and removes it; `None` keeps it as the work's corpus.
    pub shared_target: Option<String>,
}

/// What a catalog work's ingest produced.
#[derive(Debug, Clone, Copy)]
pub struct CatalogWorkIngested {
    /// Chunks the work added (after a shared-target append, the appended count).
    pub chunks_created: u64,
    /// The content recipe opted out of automatic enrichment
    /// (`[enrichment] enabled = false`).
    pub opts_out_of_auto_enrichment: bool,
}

/// Which half of a catalog work's ingest failed.
#[derive(Debug)]
pub enum CatalogWorkError {
    /// The content recipe did not load.
    ContentRecipeLoad(Error),
    /// The ingest, or the shared-target append, failed.
    Ingest(Error),
}

/// The catalog family's port (`wikipedia_fetch` and the on-demand catalog
/// ingest): read the catalog, ingest one work.
#[async_trait]
pub trait CatalogIngestPort: CorpusReadPort {
    /// Ingest `work`; progress events go to `progress`.
    async fn ingest_catalog_work(
        &self,
        work: &CatalogWork,
        progress: Option<ProgressCallback>,
    ) -> std::result::Result<CatalogWorkIngested, CatalogWorkError>;
}
