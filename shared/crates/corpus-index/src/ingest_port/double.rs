// SPDX-License-Identifier: AGPL-3.0-or-later
//! The test double of this module's ports (FIVE_PROGRAMS "Where a
//! cross-program test lives", phase-b-47). A svrn test drives its own code
//! against it; the ports' behaviour is proven on ingest's implementors
//! (`impl LocalCorpusPort` / `CatalogIngestPort for CorpusEngine`), in
//! corpus-engine's tests.
//!
//! One struct serves [`LocalCorpusPort`], [`CatalogIngestPort`],
//! [`EnrichConfigPort`], [`PartitionMergePort`], the daemon's
//! [`IngestPort`](super::daemon::IngestPort) and their shared supertraits,
//! so `CorpusReadPort` has one set of handlers rather than one per port. A method a test can program has an `on_*`; every
//! other method, and a programmable one left unprogrammed, never answers
//! success-shaped (principle 6): a `Result` method returns an `Err` naming
//! itself, any other panics naming itself. A later row that drives another
//! method gives it an `on_*` here. Registrations through
//! [`IngestPluginPort`] are the one thing the double keeps: that is the
//! method's whole contract, and a test reads them back.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::enrich_config::{EnrichConfigPort, EnrichConfigSummary, WatchedEnrichConfig};
use super::merge::{MergePhaseProgress, PartitionMergePort, PartitionMergeReport, ProjectReport};
use super::{
    CatalogIngestPort, CatalogWork, CatalogWorkError, CatalogWorkIngested, CustomAcquirerFn,
    CustomExtractorFn, DocFetchFn, EmbeddingClusters, EnrichmentPassRoute, FieldModelError,
    IngestPluginPort, LocalCorpusPort, ProgressCallback, PromptFn, RecipeIngested,
    SourceFileProgress, WatchedUpdate, WatchedUpdateProgressFn,
};
use crate::fs_source::FsIndexSource;
use crate::index::CorpusIndex;
use crate::recipe::CatalogConfig;
use crate::source::{CorpusReadPort, IndexSource};
use crate::types::{BuiltinCorpus, EmbedFn, IncompleteIngest, IndexInfo};
use crate::{Error, Result};
use sovereign_contracts::daemon_wire::RecipeParameterSchema;

mod daemon;
pub use daemon::{RecipeHarnessDouble, SliceIngest};
mod leaf_backed;
pub use leaf_backed::leaf_backed_double;

fn unprogrammed(method: &str) -> String {
    format!("IngestPortDouble::{method}: not programmed by this test")
}

fn refuse(method: &str) -> Error {
    Error::Io(std::io::Error::other(unprogrammed(method)))
}

type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;
type OpenIndexFn = dyn Fn(&str) -> BoxFuture<Result<CorpusIndex>> + Send + Sync;
type RecipePathFn<T> = dyn Fn(PathBuf) -> BoxFuture<Result<T>> + Send + Sync;
type WatchedUpdateFn = dyn Fn(WatchedUpdate, DocFetchFn) -> BoxFuture<Result<()>> + Send + Sync;
type CorpusFn<T> = dyn Fn(&str) -> T + Send + Sync;
type ReindexFn = dyn Fn(&str, &[String]) + Send + Sync;
type SourceFileProgressFn = dyn Fn(&Path) -> Option<SourceFileProgress> + Send + Sync;
type RecipeVocabularyFn =
    dyn Fn(&Path) -> Result<Option<super::daemon::RecipeVocabulary>> + Send + Sync;
type MergePartitionsFn =
    dyn Fn(Vec<PathBuf>, PathBuf) -> BoxFuture<Result<IndexInfo>> + Send + Sync;
type CatalogWorkFn = dyn Fn(CatalogWork) -> BoxFuture<std::result::Result<CatalogWorkIngested, CatalogWorkError>>
    + Send
    + Sync;
type MergeIntoCanonicalFn =
    dyn Fn(PathBuf, String) -> BoxFuture<Result<PartitionMergeReport>> + Send + Sync;

/// The ingest ports' double a svrn test programs.
#[derive(Default)]
pub struct IngestPortDouble {
    calls: Mutex<Vec<&'static str>>,
    acquirers: Mutex<Vec<String>>,
    extractors: Mutex<Vec<String>>,
    index_dir: Option<PathBuf>,
    recipes_dir: Option<PathBuf>,
    recipe_vocabulary: Option<Box<RecipeVocabularyFn>>,
    corpus_status_rows: Option<serde_json::Value>,
    recipe_corpus_id: Option<Box<daemon::RecipeTextFn<String>>>,
    validate_recipe_toml:
        Option<Box<daemon::RecipeTextFn<crate::ingest_port::daemon::RecipeValidation>>>,
    install_local_recipe: Option<Box<daemon::RecipeTextFn<PathBuf>>>,
    recipe_parameter_schema: Option<Box<daemon::RecipeTextFn<RecipeParameterSchema>>>,
    dry_run_recipe: Option<Box<daemon::DryRunFn>>,
    open_index_for_corpus: Option<Box<OpenIndexFn>>,
    embed_fn: Option<EmbedFn>,
    ingest_recipe_path: Option<Box<RecipePathFn<RecipeIngested>>>,
    ensure_empty_index: Option<Box<RecipePathFn<()>>>,
    apply_watched_update: Option<Box<WatchedUpdateFn>>,
    remove_corpus_everything: Option<Box<CorpusFn<Result<()>>>>,
    ingest_in_flight: Option<Box<CorpusFn<bool>>>,
    cancel_corpus_ingest: Option<Box<CorpusFn<bool>>>,
    reindex_changed_sources_tiered: Option<Box<ReindexFn>>,
    source_file_progress: Option<Box<SourceFileProgressFn>>,
    atlas_teardown_ok: bool,
    installed_indexes: Option<Vec<IndexInfo>>,
    listing: Option<FsIndexSource>,
    builtin_corpora: Option<Vec<BuiltinCorpus>>,
    registry_listings: Option<Vec<(String, super::daemon::RegistryListing)>>,
    incomplete_ingests: Option<Vec<IncompleteIngest>>,
    foreground_signal: Option<Arc<dyn corpus_engine_yield::ForegroundSignal>>,
    yield_hooks_ok: bool,
    no_foreground_signal: bool,
    enrich_configs: Option<Vec<(String, EnrichConfigSummary)>>,
    watched_config_root: Option<PathBuf>,
    watched_writes: Mutex<Vec<(String, String, PathBuf)>>,
    merge_partitions: Option<Box<MergePartitionsFn>>,
    finalize_canonical: Option<Box<CorpusFn<Result<()>>>>,
    merge_into_canonical: Option<Box<MergeIntoCanonicalFn>>,
    in_progress_ingestions: Option<Vec<String>>,
    stranded_partitions: Option<Vec<String>>,
    pack_canonical: Option<Box<daemon::PackCanonicalFn>>,
    unpack_canonical: Option<Box<daemon::UnpackCanonicalFn>>,
    texts_digest: Option<Box<daemon::TextsDigestFn>>,
    catalog_configs: Option<Vec<(String, CatalogConfig)>>,
    ingest_catalog_work: Option<Box<CatalogWorkFn>>,
    partition_path: Option<Box<CorpusFn<PathBuf>>>,
    prepare_registry_install: Option<Box<daemon::PrepareInstallFn>>,
    prepare_recipe_install: Option<Box<daemon::PrepareRecipeInstallFn>>,
    corpus_disk_status: Option<Box<CorpusFn<super::daemon::CorpusDiskStatus>>>,
    cached_article_stats: Option<Box<CorpusFn<Option<super::daemon::ArticleStats>>>>,
    compute_article_stats: Option<Box<CorpusFn<Option<super::daemon::ArticleStats>>>>,
    cancel_registry: Option<super::cancel::CancellationRegistry>,
    ingest_with_overrides: Option<Box<daemon::SliceIngestFn>>,
}

impl IngestPortDouble {
    /// A double with nothing programmed.
    pub fn new() -> Self {
        Self::default()
    }

    /// The methods called so far, by name, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// The kinds registered through [`IngestPluginPort::register_acquirer`].
    pub fn registered_acquirers(&self) -> Vec<String> {
        self.acquirers.lock().expect("acquirers lock").clone()
    }

    /// The kinds registered through [`IngestPluginPort::register_extractor`].
    pub fn registered_extractors(&self) -> Vec<String> {
        self.extractors.lock().expect("extractors lock").clone()
    }

    fn record(&self, method: &'static str) {
        self.calls.lock().expect("calls lock").push(method);
    }

    /// Program [`CorpusReadPort::index_dir`].
    pub fn with_index_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.index_dir = Some(dir.into());
        self
    }

    /// Program the daemon port's `recipes_dir`.
    pub fn with_recipes_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.recipes_dir = Some(dir.into());
        self
    }

    /// Program the daemon port's `recipe_vocabulary`; `f` gets the
    /// recipe's path.
    pub fn on_recipe_vocabulary(
        mut self,
        f: impl Fn(&Path) -> Result<Option<super::daemon::RecipeVocabulary>> + Send + Sync + 'static,
    ) -> Self {
        self.recipe_vocabulary = Some(Box::new(f));
        self
    }

    /// Program [`CorpusReadPort::open_index_for_corpus`] to open
    /// `<index_dir>/<corpus_id>` with the leaf's own [`CorpusIndex::open`],
    /// the call ingest's engine makes for it (minus its handle cache).
    /// Needs [`Self::with_index_dir`] first.
    pub fn opening_indexes_under_index_dir(mut self) -> Self {
        let dir = self
            .index_dir
            .clone()
            .expect("IngestPortDouble: with_index_dir before opening_indexes_under_index_dir");
        self.open_index_for_corpus = Some(Box::new(move |corpus_id| {
            let path = dir.join(corpus_id);
            Box::pin(async move { CorpusIndex::open(&path).await })
        }));
        self
    }

    /// Program [`CorpusReadPort::installed_indexes`],
    /// [`IndexSource::usable_indexes`] and [`IndexSource::open_index`] to
    /// read `<index_dir>` with the leaf's own [`FsIndexSource`], the reader
    /// ingest's engine delegates all three to, and the daemon port's
    /// `canonical_path` to name `<index_dir>/<id>` with the leaf's
    /// `Corpus::root`, as the engine's does.
    /// Needs [`Self::with_index_dir`] first.
    pub fn listing_indexes_under_index_dir(mut self) -> Self {
        let dir = self
            .index_dir
            .clone()
            .expect("IngestPortDouble: with_index_dir before listing_indexes_under_index_dir");
        self.listing = Some(FsIndexSource::new(dir));
        self
    }

    /// Program [`CorpusReadPort::embed`] and [`LocalCorpusPort::embed_fn`]
    /// with `embed`.
    pub fn with_embed_fn(mut self, embed: EmbedFn) -> Self {
        self.embed_fn = Some(embed);
        self
    }

    /// Program [`LocalCorpusPort::ingest_recipe_path`]; `f` gets the
    /// recipe's path.
    pub fn on_ingest_recipe_path(
        mut self,
        f: impl Fn(PathBuf) -> BoxFuture<Result<RecipeIngested>> + Send + Sync + 'static,
    ) -> Self {
        self.ingest_recipe_path = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::ensure_empty_index`]; `f` gets the
    /// recipe's path.
    pub fn on_ensure_empty_index(
        mut self,
        f: impl Fn(PathBuf) -> BoxFuture<Result<()>> + Send + Sync + 'static,
    ) -> Self {
        self.ensure_empty_index = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::apply_watched_update`]; `f` gets the
    /// update and the caller's document fetch.
    pub fn on_apply_watched_update(
        mut self,
        f: impl Fn(WatchedUpdate, DocFetchFn) -> BoxFuture<Result<()>> + Send + Sync + 'static,
    ) -> Self {
        self.apply_watched_update = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::remove_corpus_everything`].
    pub fn on_remove_corpus_everything(
        mut self,
        f: impl Fn(&str) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.remove_corpus_everything = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::ingest_in_flight`] and
    /// [`LocalCorpusPort::cancel_corpus_ingest`] from one answer: an ingest
    /// is registered for a corpus when `in_flight` says so, and a cancel
    /// reports that same answer.
    pub fn on_ingest_in_flight(
        mut self,
        in_flight: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        let in_flight = Arc::new(in_flight);
        let cancel = Arc::clone(&in_flight);
        self.ingest_in_flight = Some(Box::new(move |id| in_flight(id)));
        self.cancel_corpus_ingest = Some(Box::new(move |id| cancel(id)));
        self
    }

    /// Program [`LocalCorpusPort::cancel_corpus_ingest`] alone, for a test
    /// whose cancel must reach the ingest it programmed; `f` answers
    /// whether one was signalled.
    pub fn on_cancel_corpus_ingest(
        mut self,
        f: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.cancel_corpus_ingest = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::reindex_changed_sources_tiered`]; `f` gets
    /// the corpus and the changed sources.
    pub fn on_reindex_changed_sources_tiered(
        mut self,
        f: impl Fn(&str, &[String]) + Send + Sync + 'static,
    ) -> Self {
        self.reindex_changed_sources_tiered = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::source_file_progress`]; `f` gets the
    /// corpus dir.
    pub fn on_source_file_progress(
        mut self,
        f: impl Fn(&Path) -> Option<SourceFileProgress> + Send + Sync + 'static,
    ) -> Self {
        self.source_file_progress = Some(Box::new(f));
        self
    }

    /// Program [`LocalCorpusPort::atlas_teardown`] to succeed, as the
    /// engine's does on a corpus with no atlas dir.
    pub fn tearing_down_absent_atlases(mut self) -> Self {
        self.atlas_teardown_ok = true;
        self
    }

    /// Program [`CorpusReadPort::installed_indexes`] to list `indexes`.
    pub fn with_installed_indexes(mut self, indexes: Vec<IndexInfo>) -> Self {
        self.installed_indexes = Some(indexes);
        self
    }

    /// Program [`CorpusReadPort::builtin_corpora`] to list `corpora`, as the
    /// engine lists its registry snapshot's catalogue.
    pub fn with_builtin_corpora(mut self, corpora: Vec<BuiltinCorpus>) -> Self {
        self.builtin_corpora = Some(corpora);
        self
    }

    /// Program the daemon port's `registry_listing` to answer `listings`
    /// (by id) and `None` for any other id.
    pub fn with_registry_listings(
        mut self,
        listings: Vec<(String, super::daemon::RegistryListing)>,
    ) -> Self {
        self.registry_listings = Some(listings);
        self
    }

    /// Program [`CorpusReadPort::catalog_config`] to answer `configs` (by
    /// catalog corpus id) and `Ok(None)` for any other corpus.
    pub fn with_catalog_configs(mut self, configs: Vec<(String, CatalogConfig)>) -> Self {
        self.catalog_configs = Some(configs);
        self
    }

    /// Program [`CatalogIngestPort::ingest_catalog_work`]; `f` gets the
    /// work (progress is not replayed).
    pub fn on_ingest_catalog_work(
        mut self,
        f: impl Fn(CatalogWork) -> BoxFuture<std::result::Result<CatalogWorkIngested, CatalogWorkError>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.ingest_catalog_work = Some(Box::new(f));
        self
    }

    /// Program [`CorpusReadPort::incomplete_ingests`] to list `ingests`.
    pub fn with_incomplete_ingests(mut self, ingests: Vec<IncompleteIngest>) -> Self {
        self.incomplete_ingests = Some(ingests);
        self
    }

    /// Program [`CorpusReadPort::foreground_lease`] to hand out a lease on
    /// `signal`, as the engine does once the daemon installs one.
    pub fn with_foreground_signal(
        mut self,
        signal: Arc<dyn corpus_engine_yield::ForegroundSignal>,
    ) -> Self {
        self.foreground_signal = Some(signal);
        self
    }

    /// Program [`CorpusReadPort::foreground_lease`] to hand out none, as the
    /// engine does before the daemon installs a signal.
    pub fn without_foreground_signal(mut self) -> Self {
        self.no_foreground_signal = true;
        self
    }

    /// Program the daemon port's `set_yield_hook` and
    /// `set_foreground_signal` to accept what the daemon installs at start;
    /// the double keeps neither (a lease still needs
    /// [`Self::with_foreground_signal`]).
    pub fn accepting_yield_hooks(mut self) -> Self {
        self.yield_hooks_ok = true;
        self
    }

    /// Program [`EnrichConfigPort::load`] to answer `configs` (by corpus id)
    /// and `Ok(None)` for any other corpus.
    pub fn with_enrich_configs(mut self, configs: Vec<(String, EnrichConfigSummary)>) -> Self {
        self.enrich_configs = Some(configs);
        self
    }

    /// Program [`EnrichConfigPort::write_watched`] to record each write and
    /// answer `<root>/<corpus_id>/config.json`, touching no disk.
    pub fn writing_watched_configs_under(mut self, root: impl Into<PathBuf>) -> Self {
        self.watched_config_root = Some(root.into());
        self
    }

    /// Each [`EnrichConfigPort::write_watched`] so far: corpus id, pipeline
    /// id, source path.
    pub fn watched_writes(&self) -> Vec<(String, String, PathBuf)> {
        self.watched_writes
            .lock()
            .expect("watched writes lock")
            .clone()
    }

    /// Program [`PartitionMergePort::merge_partitions`]; `f` gets the
    /// partition dirs, in the order the caller passed them, and the output.
    pub fn on_merge_partitions(
        mut self,
        f: impl Fn(Vec<PathBuf>, PathBuf) -> BoxFuture<Result<IndexInfo>> + Send + Sync + 'static,
    ) -> Self {
        self.merge_partitions = Some(Box::new(f));
        self
    }

    /// Program [`PartitionMergePort::finalize_canonical`]; `f` gets the
    /// corpus id.
    pub fn on_finalize_canonical(
        mut self,
        f: impl Fn(&str) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.finalize_canonical = Some(Box::new(f));
        self
    }

    /// Program [`PartitionMergePort::merge_partitions_into_canonical`]; `f`
    /// gets the index dir and the corpus id (progress is not replayed).
    pub fn on_merge_partitions_into_canonical(
        mut self,
        f: impl Fn(PathBuf, String) -> BoxFuture<Result<PartitionMergeReport>> + Send + Sync + 'static,
    ) -> Self {
        self.merge_into_canonical = Some(Box::new(f));
        self
    }
}

#[async_trait]
impl PartitionMergePort for IngestPortDouble {
    async fn merge_partitions(&self, partitions: &[PathBuf], output: &Path) -> Result<IndexInfo> {
        self.record("merge_partitions");
        match &self.merge_partitions {
            Some(f) => f(partitions.to_vec(), output.to_path_buf()).await,
            None => Err(refuse("merge_partitions")),
        }
    }

    async fn finalize_canonical(&self, _canonical: &CorpusIndex, corpus_id: &str) -> Result<()> {
        self.record("finalize_canonical");
        match &self.finalize_canonical {
            Some(f) => f(corpus_id),
            None => Err(refuse("finalize_canonical")),
        }
    }

    async fn merge_partitions_into_canonical(
        &self,
        index_dir: &Path,
        corpus_id: &str,
        _progress: Option<Arc<dyn Fn(MergePhaseProgress) + Send + Sync>>,
    ) -> Result<PartitionMergeReport> {
        self.record("merge_partitions_into_canonical");
        match &self.merge_into_canonical {
            Some(f) => f(index_dir.to_path_buf(), corpus_id.to_string()).await,
            None => Err(refuse("merge_partitions_into_canonical")),
        }
    }

    async fn project_alignment(&self, _canonical: &Path, _home: &Path) -> Result<ProjectReport> {
        self.record("project_alignment");
        Err(refuse("project_alignment"))
    }
}

impl EnrichConfigPort for IngestPortDouble {
    fn load(&self, corpus_id: &str) -> Result<Option<EnrichConfigSummary>> {
        self.record("enrich_config_load");
        let configs = self
            .enrich_configs
            .as_ref()
            .ok_or_else(|| refuse("enrich_config_load"))?;
        Ok(configs
            .iter()
            .find(|(id, _)| id == corpus_id)
            .map(|(_, summary)| summary.clone()))
    }

    fn write_watched(&self, config: &WatchedEnrichConfig<'_>) -> Result<PathBuf> {
        self.record("write_watched");
        let root = self
            .watched_config_root
            .as_ref()
            .ok_or_else(|| refuse("write_watched"))?;
        self.watched_writes
            .lock()
            .expect("watched writes lock")
            .push((
                config.corpus_id.to_string(),
                config.pipeline_id.to_string(),
                config.source_path.to_path_buf(),
            ));
        Ok(root.join(config.corpus_id).join("config.json"))
    }
}

#[async_trait]
impl IndexSource for IngestPortDouble {
    async fn usable_indexes(&self) -> Result<Vec<IndexInfo>> {
        self.record("usable_indexes");
        match &self.listing {
            Some(source) => source.usable_indexes().await,
            None => Err(refuse("usable_indexes")),
        }
    }

    async fn open_index(&self, path: &Path) -> Result<CorpusIndex> {
        self.record("open_index");
        match &self.listing {
            Some(source) => IndexSource::open_index(source, path).await,
            None => Err(refuse("open_index")),
        }
    }
}

#[async_trait]
impl CorpusReadPort for IngestPortDouble {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.record("embed");
        match &self.embed_fn {
            Some(embed) => embed(text).await,
            None => Err(refuse("embed")),
        }
    }

    async fn installed_indexes(&self) -> Result<Vec<IndexInfo>> {
        self.record("installed_indexes");
        match (&self.listing, &self.installed_indexes) {
            (Some(source), _) => source.installed_indexes().await,
            (None, Some(indexes)) => Ok(indexes.clone()),
            (None, None) => Err(refuse("installed_indexes")),
        }
    }

    async fn open_index_for_corpus(&self, corpus_id: &str) -> Result<CorpusIndex> {
        self.record("open_index_for_corpus");
        match &self.open_index_for_corpus {
            Some(f) => f(corpus_id).await,
            None => Err(refuse("open_index_for_corpus")),
        }
    }

    fn index_dir(&self) -> &Path {
        self.record("index_dir");
        match &self.index_dir {
            Some(dir) => dir,
            None => panic!("{}", unprogrammed("index_dir")),
        }
    }

    fn foreground_lease(&self) -> Option<corpus_engine_yield::ForegroundLease> {
        self.record("foreground_lease");
        match &self.foreground_signal {
            Some(signal) => Some(corpus_engine_yield::ForegroundLease::acquire(Arc::clone(
                signal,
            ))),
            None if self.no_foreground_signal => None,
            None => panic!("{}", unprogrammed("foreground_lease")),
        }
    }

    fn builtin_corpora(&self) -> Vec<BuiltinCorpus> {
        self.record("builtin_corpora");
        match &self.builtin_corpora {
            Some(corpora) => corpora.clone(),
            None => panic!("{}", unprogrammed("builtin_corpora")),
        }
    }

    fn incomplete_ingests(&self) -> Vec<IncompleteIngest> {
        self.record("incomplete_ingests");
        match &self.incomplete_ingests {
            Some(ingests) => ingests.clone(),
            None => panic!("{}", unprogrammed("incomplete_ingests")),
        }
    }

    fn declared_authority_tool(&self, _corpus_id: &str) -> Option<String> {
        panic!("{}", unprogrammed("declared_authority_tool"))
    }

    async fn catalog_config(&self, corpus_id: &str) -> Result<Option<CatalogConfig>> {
        self.record("catalog_config");
        let configs = self
            .catalog_configs
            .as_ref()
            .ok_or_else(|| refuse("catalog_config"))?;
        Ok(configs
            .iter()
            .find(|(id, _)| id == corpus_id)
            .map(|(_, config)| config.clone()))
    }

    async fn enriched_corpus_ids(&self) -> Result<Vec<String>> {
        self.record("enriched_corpus_ids");
        Err(refuse("enriched_corpus_ids"))
    }
}

impl IngestPluginPort for IngestPortDouble {
    fn register_acquirer(&self, kind: &str, _acquirer: CustomAcquirerFn) {
        self.record("register_acquirer");
        self.acquirers
            .lock()
            .expect("acquirers lock")
            .push(kind.to_string());
    }

    fn register_extractor(&self, kind: &str, _extractor: CustomExtractorFn) {
        self.record("register_extractor");
        self.extractors
            .lock()
            .expect("extractors lock")
            .push(kind.to_string());
    }
}

#[async_trait]
impl CatalogIngestPort for IngestPortDouble {
    async fn ingest_catalog_work(
        &self,
        work: &CatalogWork,
        _progress: Option<ProgressCallback>,
    ) -> std::result::Result<CatalogWorkIngested, CatalogWorkError> {
        self.record("ingest_catalog_work");
        match &self.ingest_catalog_work {
            Some(f) => f(work.clone()).await,
            None => Err(CatalogWorkError::Ingest(refuse("ingest_catalog_work"))),
        }
    }
}

#[async_trait]
impl LocalCorpusPort for IngestPortDouble {
    async fn ingest_recipe_path(
        &self,
        recipe_path: &Path,
        _progress: Option<ProgressCallback>,
    ) -> Result<RecipeIngested> {
        self.record("ingest_recipe_path");
        match &self.ingest_recipe_path {
            Some(f) => f(recipe_path.to_path_buf()).await,
            None => Err(refuse("ingest_recipe_path")),
        }
    }

    async fn ensure_empty_index(&self, recipe_path: &Path) -> Result<()> {
        self.record("ensure_empty_index");
        match &self.ensure_empty_index {
            Some(f) => f(recipe_path.to_path_buf()).await,
            None => Err(refuse("ensure_empty_index")),
        }
    }

    fn cancel_corpus_ingest(&self, corpus_id: &str) -> bool {
        self.record("cancel_corpus_ingest");
        match &self.cancel_corpus_ingest {
            Some(f) => f(corpus_id),
            None => panic!("{}", unprogrammed("cancel_corpus_ingest")),
        }
    }

    fn ingest_in_flight(&self, corpus_id: &str) -> bool {
        self.record("ingest_in_flight");
        match &self.ingest_in_flight {
            Some(f) => f(corpus_id),
            None => panic!("{}", unprogrammed("ingest_in_flight")),
        }
    }

    fn remove_corpus_everything(&self, corpus_id: &str) -> Result<()> {
        self.record("remove_corpus_everything");
        match &self.remove_corpus_everything {
            Some(f) => f(corpus_id),
            None => Err(refuse("remove_corpus_everything")),
        }
    }

    fn atlas_teardown(&self, _index_dir: &Path, _corpus_id: &str) -> std::io::Result<()> {
        self.record("atlas_teardown");
        if self.atlas_teardown_ok {
            return Ok(());
        }
        Err(std::io::Error::other(unprogrammed("atlas_teardown")))
    }

    fn source_file_progress(&self, corpus_dir: &Path) -> Option<SourceFileProgress> {
        self.record("source_file_progress");
        match &self.source_file_progress {
            Some(f) => f(corpus_dir),
            None => panic!("{}", unprogrammed("source_file_progress")),
        }
    }

    async fn reindex_changed_sources_tiered(&self, corpus_id: &str, source_doc_ids: &[String]) {
        self.record("reindex_changed_sources_tiered");
        match &self.reindex_changed_sources_tiered {
            Some(f) => f(corpus_id, source_doc_ids),
            None => panic!("{}", unprogrammed("reindex_changed_sources_tiered")),
        }
    }

    async fn apply_watched_update(
        self: Arc<Self>,
        update: &WatchedUpdate,
        fetch: DocFetchFn,
        _progress: WatchedUpdateProgressFn,
    ) -> Result<()> {
        self.record("apply_watched_update");
        match &self.apply_watched_update {
            Some(f) => f(update.clone(), fetch).await,
            None => Err(refuse("apply_watched_update")),
        }
    }

    async fn recipe_enrichment_type(&self, _corpus_id: &str) -> Result<Option<String>> {
        self.record("recipe_enrichment_type");
        Err(refuse("recipe_enrichment_type"))
    }

    fn enrichment_pass_route(&self, _enrichment_type: &str) -> Option<EnrichmentPassRoute> {
        panic!("{}", unprogrammed("enrichment_pass_route"))
    }

    fn prompt_fn(
        &self,
        _inference: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    ) -> PromptFn {
        panic!("{}", unprogrammed("prompt_fn"))
    }

    async fn cluster_embeddings(
        &self,
        _index: &CorpusIndex,
        _min_cluster_size: usize,
        _on_step: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<EmbeddingClusters> {
        self.record("cluster_embeddings");
        Err(refuse("cluster_embeddings"))
    }

    fn embed_fn(&self) -> EmbedFn {
        self.record("embed_fn");
        match &self.embed_fn {
            Some(embed) => Arc::clone(embed),
            None => panic!("{}", unprogrammed("embed_fn")),
        }
    }

    async fn enrich_field_model(
        &self,
        _index: &CorpusIndex,
        _recipe: &serde_json::Value,
    ) -> std::result::Result<String, FieldModelError> {
        self.record("enrich_field_model");
        Err(FieldModelError::Enrich(refuse("enrich_field_model")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unprogrammed_result_method_errs_naming_itself() {
        let d = IngestPortDouble::new();
        let Err(err) = d.open_index_for_corpus("c").await else {
            panic!("an unprogrammed open answered Ok");
        };
        assert!(
            err.to_string()
                .contains("IngestPortDouble::open_index_for_corpus"),
            "{err}"
        );
        assert_eq!(d.calls(), vec!["open_index_for_corpus"]);
    }

    #[test]
    #[should_panic(expected = "IngestPortDouble::index_dir: not programmed")]
    fn an_unprogrammed_plain_method_panics_naming_itself() {
        IngestPortDouble::new().index_dir();
    }

    #[test]
    fn without_a_foreground_signal_no_lease_is_handed_out() {
        let d = IngestPortDouble::new().without_foreground_signal();
        assert!(d.foreground_lease().is_none());
        assert_eq!(d.calls(), vec!["foreground_lease"]);
    }

    #[tokio::test]
    async fn an_unprogrammed_daemon_port_method_errs_naming_itself() {
        use super::super::daemon::IngestPort;
        let Err(err) = IngestPortDouble::new().recipe_sharing("c").await else {
            panic!("an unprogrammed recipe_sharing answered Ok");
        };
        assert!(
            err.to_string().contains("IngestPortDouble::recipe_sharing"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_listing_mode_reads_the_index_dir_with_the_leaf_reader() {
        let dir = tempfile::tempdir().unwrap();
        let index = CorpusIndex::create(&dir.path().join("c"), "c", "C", "m", 4, true, "MIT")
            .await
            .unwrap();
        index.mark_ingestion_complete().unwrap();
        let d = IngestPortDouble::new()
            .with_index_dir(dir.path())
            .listing_indexes_under_index_dir();
        let listed = d.installed_indexes().await.unwrap();
        let ids: Vec<&str> = listed.iter().map(|i| i.corpus_id.as_str()).collect();
        assert_eq!(ids, vec!["c"]);
        assert_eq!(d.calls(), vec!["installed_indexes"]);
    }

    #[test]
    fn registrations_are_kept_by_kind() {
        let d = IngestPortDouble::new();
        let acquirer: CustomAcquirerFn = Arc::new(|_, dir| Box::pin(async move { Ok(dir) }));
        d.register_acquirer("sec_edgar", acquirer);
        assert_eq!(d.registered_acquirers(), vec!["sec_edgar".to_string()]);
    }
}
