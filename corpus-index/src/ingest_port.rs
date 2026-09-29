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

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::source::CorpusReadPort;
use crate::{Error, Result};

#[cfg(any(test, feature = "test-doubles"))]
pub mod double;

pub mod tiered;

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

/// Runtime-registered acquirer closure. Receives the custom acquirer
/// `params` blob from the recipe and the per-ingest `download_dir`;
/// returns the local path that the extractor should read (typically a
/// JSONL file).
///
/// Uses `Pin<Box<dyn Future>>` rather than the engine's static-dispatch
/// `Acquirer` trait because it must be object-safe: the registry stores heterogeneous
/// implementations keyed by `kind` string.
///
/// Progress reporting is intentionally omitted here. The
/// `ProgressCallback` type is not `Clone`, and KnowledgeView-style
/// acquirers (SQLite → JSONL) finish in sub-second time, so a progress
/// bar buys nothing. If a future custom acquirer needs progress, the
/// closure can emit it via its own side channel.
pub type CustomAcquirerFn = Arc<
    dyn Fn(serde_json::Value, PathBuf) -> Pin<Box<dyn Future<Output = Result<PathBuf>> + Send>>
        + Send
        + Sync,
>;

/// Closure type for a runtime-registered per-file text extractor.
///
/// Recipe sets `extract = { type = "custom", kind = "<key>", extension = "<ext>" }`;
/// the engine walks the acquired directory, collects files with the
/// configured extension, and calls this closure on each to produce
/// `ExtractedDoc.content`. The registered implementation typically
/// lives in `sovereign-tools` so corpus-engine stays free of heavy
/// per-format dependencies (pdf-extract, lopdf, libreoffice, …).
///
/// Returning `Ok("")` skips the file (treated as empty). Returning
/// `Err(_)` propagates as a per-file extraction failure that bubbles
/// through the standard ingest error path.
pub type CustomExtractorFn = Arc<dyn Fn(&Path) -> Result<String> + Send + Sync>;

/// The plugin family's port: svrn's acquirers and extractors (the SEC and
/// KnowledgeView acquirers, the PDF extractor) register with ingest here.
pub trait IngestPluginPort: Send + Sync {
    /// Resolve recipes with `acquire = { type = "custom", kind }` to `acquirer`.
    fn register_acquirer(&self, kind: &str, acquirer: CustomAcquirerFn);

    /// Resolve recipes with `extract = { type = "custom", kind }` to `extractor`.
    fn register_extractor(&self, kind: &str, extractor: CustomExtractorFn);
}

// ─── The local-corpus family (local_corpus, watched, knowledge_view) ────

/// A single-message prompt, answered by enrichment's inference (the
/// EnrichBulk slot). The cluster labeller's one LLM call.
pub type PromptFn =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

/// A recipe ingest's outcome.
#[derive(Debug, Clone)]
pub struct RecipeIngested {
    /// The corpus the recipe wrote.
    pub corpus_id: String,
    /// Chunks it created.
    pub chunks_created: u64,
}

/// A corpus's source-file manifest, counted: how many files the ingest
/// finished of how many it planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFileProgress {
    /// Files whose status is `Complete`.
    pub done: usize,
    /// Files in the manifest.
    pub total: usize,
}

/// Where a corpus's declared enrichment type routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentPassRoute {
    /// The registered pass's id.
    pub pass_id: String,
    /// The pass is the atlas pass.
    pub is_atlas: bool,
}

/// One HDBSCAN cluster over a corpus's chunk embeddings.
#[derive(Debug, Clone)]
pub struct EmbeddingCluster {
    /// Cluster id (`-1` is noise and never appears here).
    pub id: i32,
    /// Chunks assigned to it.
    pub size: usize,
    /// Mean embedding of its chunks.
    pub centroid: Vec<f32>,
    /// The chunks nearest its centroid.
    pub central_chunks: Vec<u64>,
}

/// A clustering of a corpus's chunk embeddings.
#[derive(Debug, Clone)]
pub struct EmbeddingClusters {
    /// chunk_id → cluster id (`-1` = HDBSCAN noise).
    pub assignments: std::collections::HashMap<u64, i32>,
    /// The clusters, noise excluded.
    pub clusters: Vec<EmbeddingCluster>,
}

/// One watched-folder sweep's delta, applied as a new corpus version.
#[derive(Debug, Clone)]
pub struct WatchedUpdate {
    /// The corpus to update.
    pub corpus_id: String,
    /// The new version's label.
    pub version: String,
    /// doc_id → content hash, for every document in the new version.
    pub entries: std::collections::HashMap<String, String>,
    /// Documents to add.
    pub new_documents: Vec<String>,
    /// Documents to replace.
    pub updated_documents: Vec<String>,
    /// Documents to delete.
    pub deleted_documents: Vec<String>,
}

/// The three sequential stages of a watched update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchedUpdateStage {
    /// Removing deleted documents.
    Deletions,
    /// Replacing updated documents.
    Updates,
    /// Adding new documents.
    Additions,
}

/// Fetch one document's text by doc_id, for a watched update.
pub type DocFetchFn =
    Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

/// Progress of a watched update: `(stage, done, total)`.
pub type WatchedUpdateProgressFn = Box<dyn Fn(WatchedUpdateStage, usize, usize) + Send + Sync>;

/// Which half of a knowledge view's field-model enrichment failed.
#[derive(Debug)]
pub enum FieldModelError {
    /// The field-model engine could not be built from the recipe (including
    /// an engine with no enrichment inference configured).
    Construct(Error),
    /// The enrichment itself failed.
    Enrich(Error),
}

/// The local-corpus family's port: what `local_corpus` (with `watched`) and
/// `knowledge_view` ask ingest to do, beyond reading.
#[async_trait]
pub trait LocalCorpusPort: CorpusReadPort + IngestPluginPort {
    /// Ingest the recipe TOML at `recipe_path`; progress events go to
    /// `progress`.
    async fn ingest_recipe_path(
        &self,
        recipe_path: &Path,
        progress: Option<ProgressCallback>,
    ) -> Result<RecipeIngested>;

    /// Create the recipe's corpus as an empty index, ingesting nothing.
    async fn ensure_empty_index(&self, recipe_path: &Path) -> Result<()>;

    /// Signal a running ingest of `corpus_id` to stop; `true` when one was
    /// registered.
    fn cancel_corpus_ingest(&self, corpus_id: &str) -> bool;

    /// An ingest of `corpus_id` is registered with the engine right now.
    fn ingest_in_flight(&self, corpus_id: &str) -> bool;

    /// Remove every trace of `corpus_id` from disk.
    fn remove_corpus_everything(&self, corpus_id: &str) -> Result<()>;

    /// Remove `index_dir/<corpus_id>/atlas/`; idempotent on a missing dir.
    fn atlas_teardown(&self, index_dir: &Path, corpus_id: &str) -> std::io::Result<()>;

    /// The source-file manifest under `corpus_dir`, counted; `None` when
    /// there is none or it does not load.
    fn source_file_progress(&self, corpus_dir: &Path) -> Option<SourceFileProgress>;

    /// Re-run the tiered entity pass over the named sources of `corpus_id`.
    async fn reindex_changed_sources_tiered(&self, corpus_id: &str, source_doc_ids: &[String]);

    /// Apply a watched-folder delta, fetching each added or updated
    /// document's text through `fetch`. Takes the port by `Arc`: the
    /// engine's updater holds its engine for the update's length.
    async fn apply_watched_update(
        self: Arc<Self>,
        update: &WatchedUpdate,
        fetch: DocFetchFn,
        progress: WatchedUpdateProgressFn,
    ) -> Result<()>;

    /// The enrichment type `corpus_id`'s recipe declares; `Ok(None)` when it
    /// declares none, `Err` when the recipe does not load.
    async fn recipe_enrichment_type(&self, corpus_id: &str) -> Result<Option<String>>;

    /// The registered enrichment pass an enrichment type routes to.
    fn enrichment_pass_route(&self, enrichment_type: &str) -> Option<EnrichmentPassRoute>;

    /// Enrichment's single-message prompt function over `inference`.
    fn prompt_fn(
        &self,
        inference: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    ) -> PromptFn;

    /// HDBSCAN over `index`'s chunk embeddings. `on_step` gets each
    /// clustering step's name as it starts.
    async fn cluster_embeddings(
        &self,
        index: &crate::index::CorpusIndex,
        min_cluster_size: usize,
        on_step: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<EmbeddingClusters>;

    /// The engine's query embedder.
    fn embed_fn(&self) -> crate::types::EmbedFn;

    /// Run the field-model enrichment the recipe document declares over
    /// `index`, with the engine's own embedder and enrichment inference.
    /// Returns the run's stats, rendered for the log.
    async fn enrich_field_model(
        &self,
        index: &crate::index::CorpusIndex,
        recipe: &serde_json::Value,
    ) -> std::result::Result<String, FieldModelError>;
}

/// A folder corpus's entity delta.
#[derive(Debug, Clone, Copy)]
pub struct EntityDelta {
    /// Mentions persisted.
    pub mentions: usize,
    /// Chunks refused whole for exceeding the per-chunk input bound (already
    /// reported where the operator looks).
    pub refused_over_cap: usize,
}

/// The watched-folder driver's tiered build: ingest's tiered provider and
/// entity extractor, composed at boot.
#[async_trait]
pub trait FolderTieredPort: Send + Sync {
    /// Re-enrich the named sources of `corpus_id`.
    async fn reenrich_sources(&self, corpus_id: &str, source_doc_ids: &[String]) -> Result<()>;

    /// An entity extractor is composed.
    fn has_entity_extractor(&self) -> bool;

    /// Extract the corpus's entity delta; `None` when no entity extractor is
    /// composed.
    async fn extract_entity_delta(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> Option<Result<EntityDelta>>;

    /// Build the folder's RAPTOR trees and motif index, one per source
    /// document.
    async fn run_folder_tiered_enrichment(&self, corpus_id: &str, index_path: &Path) -> Result<()>;
}
