// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`IngestPort`] and [`RecipeHarnessPort`] on the double
//! (pb-ingest-dial-daemon-tests-slot). The daemon's tests that only SLOT a
//! corpus handle hold this instead of an engine; the same rule as the
//! parent's holds: nothing answers success-shaped unless a test programmed
//! it (principle 6).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine_yield::{ForegroundSignal, YieldHook};
use sovereign_contracts::daemon_wire::{RecipeDryRunReport, RecipeParameterSchema};

use super::{refuse, unprogrammed, IngestPortDouble};
use crate::corpus::Corpus;
use crate::index::CorpusIndex;
use crate::ingest_port::cancel::CancellationRegistry;
use crate::ingest_port::daemon::{
    ArticleStats, CorpusDiskStatus, HarnessRunCardView, IndexOpener, IngestPort, IngestResult,
    InstallRefusal, NewsworthyHostFactory, PreparedInstall, RecipeHarnessPort, RecipeSharing,
    RecipeVocabulary, RegistryListing, SourceFileRecord,
};
use crate::ingest_port::ProgressCallback;
use crate::Result;

fn io_refuse(method: &str) -> std::io::Error {
    std::io::Error::other(unprogrammed(method))
}

pub(super) type RecipeTextFn<T> = dyn Fn(&str) -> Result<T> + Send + Sync;
pub(super) type DryRunFn = dyn Fn(&Path, usize, bool) -> Result<RecipeDryRunReport> + Send + Sync;
pub(super) type PackCanonicalFn =
    dyn Fn(&Path, Box<dyn std::io::Write + Send>, i32) -> Result<u64> + Send + Sync;

/// Programming for the daemon port's own methods.
impl IngestPortDouble {
    /// Program `corpus_status_rows` to answer `rows`.
    pub fn with_corpus_status_rows(mut self, rows: serde_json::Value) -> Self {
        self.corpus_status_rows = Some(rows);
        self
    }

    /// Program `recipe_corpus_id`; `f` gets the recipe's TOML text.
    pub fn on_recipe_corpus_id(
        mut self,
        f: impl Fn(&str) -> Result<String> + Send + Sync + 'static,
    ) -> Self {
        self.recipe_corpus_id = Some(Box::new(f));
        self
    }

    /// Program `install_local_recipe`; `f` gets the recipe's TOML text.
    pub fn on_install_local_recipe(
        mut self,
        f: impl Fn(&str) -> Result<PathBuf> + Send + Sync + 'static,
    ) -> Self {
        self.install_local_recipe = Some(Box::new(f));
        self
    }

    /// Program `recipe_parameter_schema`; `f` gets the corpus id.
    pub fn on_recipe_parameter_schema(
        mut self,
        f: impl Fn(&str) -> Result<RecipeParameterSchema> + Send + Sync + 'static,
    ) -> Self {
        self.recipe_parameter_schema = Some(Box::new(f));
        self
    }

    /// Program `dry_run_recipe`; `f` gets the staged recipe path, the
    /// sample size and the offline flag.
    pub fn on_dry_run_recipe(
        mut self,
        f: impl Fn(&Path, usize, bool) -> Result<RecipeDryRunReport> + Send + Sync + 'static,
    ) -> Self {
        self.dry_run_recipe = Some(Box::new(f));
        self
    }

    /// Program `in_progress_ingestions` to answer `ids`.
    pub fn with_in_progress_ingestions(mut self, ids: Vec<String>) -> Self {
        self.in_progress_ingestions = Some(ids);
        self
    }

    /// Program `corpora_with_stranded_partitions` to answer `ids`.
    pub fn with_stranded_partitions(mut self, ids: Vec<String>) -> Self {
        self.stranded_partitions = Some(ids);
        self
    }

    /// Program `pack_canonical`; `f` gets the canonical path, the writer
    /// and the compression level.
    pub fn on_pack_canonical(
        mut self,
        f: impl Fn(&Path, Box<dyn std::io::Write + Send>, i32) -> Result<u64> + Send + Sync + 'static,
    ) -> Self {
        self.pack_canonical = Some(Box::new(f));
        self
    }
}

#[async_trait]
impl IngestPort for IngestPortDouble {
    fn recipes_dir(&self) -> &Path {
        self.record("recipes_dir");
        match &self.recipes_dir {
            Some(dir) => dir,
            None => panic!("{}", unprogrammed("recipes_dir")),
        }
    }

    fn partition_path(&self, _corpus_id: &str) -> PathBuf {
        panic!("{}", unprogrammed("partition_path"))
    }

    fn canonical_path(&self, corpus_id: &str) -> PathBuf {
        self.record("canonical_path");
        let listing = self.listing.as_ref();
        match listing.and_then(|source| Corpus::named(source.index_dir(), corpus_id)) {
            Some(corpus) => corpus.root(),
            None => panic!("{}", unprogrammed("canonical_path")),
        }
    }

    fn corpus_is_installed(&self, _corpus_id: &str) -> bool {
        panic!("{}", unprogrammed("corpus_is_installed"))
    }

    fn in_progress_ingestions(&self) -> Vec<String> {
        self.record("in_progress_ingestions");
        match &self.in_progress_ingestions {
            Some(ids) => ids.clone(),
            None => panic!("{}", unprogrammed("in_progress_ingestions")),
        }
    }

    fn corpora_with_stranded_partitions(&self) -> Vec<String> {
        self.record("corpora_with_stranded_partitions");
        match &self.stranded_partitions {
            Some(ids) => ids.clone(),
            None => panic!("{}", unprogrammed("corpora_with_stranded_partitions")),
        }
    }

    fn cancel_registry(&self) -> CancellationRegistry {
        panic!("{}", unprogrammed("cancel_registry"))
    }

    fn has_source_manifest(&self, _corpus_id: &str) -> bool {
        panic!("{}", unprogrammed("has_source_manifest"))
    }

    fn remaining_source_files(&self, _corpus_id: &str) -> Result<Vec<SourceFileRecord>> {
        self.record("remaining_source_files");
        Err(refuse("remaining_source_files"))
    }

    fn count_jsonl_articles(&self, _corpus_id: &str) -> Result<u64> {
        self.record("count_jsonl_articles");
        Err(refuse("count_jsonl_articles"))
    }

    fn jsonl_source_shard_count(&self, _corpus_id: &str) -> Result<usize> {
        self.record("jsonl_source_shard_count");
        Err(refuse("jsonl_source_shard_count"))
    }

    fn estimate_article_pos(
        &self,
        _corpus_id: &str,
        _committed_iter_pos: u64,
        _sample_size: usize,
    ) -> Result<Option<u64>> {
        self.record("estimate_article_pos");
        Err(refuse("estimate_article_pos"))
    }

    fn corpus_processed_shards(&self, _corpus_id: &str) -> Vec<usize> {
        panic!("{}", unprogrammed("corpus_processed_shards"))
    }

    fn corpus_committed_iter_pos(&self, _corpus_id: &str) -> u64 {
        panic!("{}", unprogrammed("corpus_committed_iter_pos"))
    }

    fn corpus_disk_status(&self, _corpus_id: &str) -> CorpusDiskStatus {
        panic!("{}", unprogrammed("corpus_disk_status"))
    }

    fn cached_article_stats(&self, _corpus_id: &str) -> Option<ArticleStats> {
        panic!("{}", unprogrammed("cached_article_stats"))
    }

    fn compute_article_stats(&self, _corpus_id: &str) -> Option<ArticleStats> {
        panic!("{}", unprogrammed("compute_article_stats"))
    }

    fn corpus_status_rows(&self) -> std::io::Result<serde_json::Value> {
        self.record("corpus_status_rows");
        match &self.corpus_status_rows {
            Some(rows) => Ok(rows.clone()),
            None => Err(io_refuse("corpus_status_rows")),
        }
    }

    async fn open_index_transient(&self, _path: &Path) -> Result<CorpusIndex> {
        self.record("open_index_transient");
        Err(refuse("open_index_transient"))
    }

    async fn diagnose_indexes(&self) -> String {
        panic!("{}", unprogrammed("diagnose_indexes"))
    }

    fn reprocess_skeleton_failures(&self, _index: &CorpusIndex) -> Result<(usize, usize)> {
        self.record("reprocess_skeleton_failures");
        Err(refuse("reprocess_skeleton_failures"))
    }

    fn pack_canonical(
        &self,
        canonical_path: &Path,
        writer: Box<dyn std::io::Write + Send>,
        compression_level: i32,
    ) -> Result<u64> {
        self.record("pack_canonical");
        match &self.pack_canonical {
            Some(f) => f(canonical_path, writer, compression_level),
            None => Err(refuse("pack_canonical")),
        }
    }

    fn set_yield_hook(&self, _hook: Arc<dyn YieldHook>) {
        self.record("set_yield_hook");
        if !self.yield_hooks_ok {
            panic!("{}", unprogrammed("set_yield_hook"))
        }
    }

    fn set_foreground_signal(&self, _signal: Arc<dyn ForegroundSignal>) {
        self.record("set_foreground_signal");
        if !self.yield_hooks_ok {
            panic!("{}", unprogrammed("set_foreground_signal"))
        }
    }

    async fn ingest_with_overrides(
        &self,
        _recipe_id: &str,
        _file_indices: Option<Vec<usize>>,
        _article_range: Option<(u64, u64)>,
        _output_path: &Path,
        _progress: Option<ProgressCallback>,
        _unit_id: Option<u32>,
    ) -> Result<IngestResult> {
        self.record("ingest_with_overrides");
        Err(refuse("ingest_with_overrides"))
    }

    async fn expand_corpus_to_full(
        &self,
        _corpus_id: &str,
        _progress: Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        self.record("expand_corpus_to_full");
        Err(refuse("expand_corpus_to_full"))
    }

    async fn resume_interrupted_conversation_enrichment(&self) -> usize {
        panic!(
            "{}",
            unprogrammed("resume_interrupted_conversation_enrichment")
        )
    }

    async fn prepare_registry_install(
        self: Arc<Self>,
        _corpus_id: &str,
        _parameters: &BTreeMap<String, serde_json::Value>,
    ) -> std::result::Result<PreparedInstall, InstallRefusal> {
        self.record("prepare_registry_install");
        Err(InstallRefusal::RecipeNotFound(unprogrammed(
            "prepare_registry_install",
        )))
    }

    async fn recipe_sharing(&self, _corpus_id: &str) -> Result<RecipeSharing> {
        self.record("recipe_sharing");
        Err(refuse("recipe_sharing"))
    }

    fn registry_listing(&self, _id: &str) -> Option<RegistryListing> {
        panic!("{}", unprogrammed("registry_listing"))
    }

    fn recipe_corpus_id(&self, toml_text: &str) -> Result<String> {
        self.record("recipe_corpus_id");
        match &self.recipe_corpus_id {
            Some(f) => f(toml_text),
            None => Err(refuse("recipe_corpus_id")),
        }
    }

    fn install_local_recipe(&self, toml_text: &str) -> Result<PathBuf> {
        self.record("install_local_recipe");
        match &self.install_local_recipe {
            Some(f) => f(toml_text),
            None => Err(refuse("install_local_recipe")),
        }
    }

    async fn recipe_parameter_schema(&self, corpus_id: &str) -> Result<RecipeParameterSchema> {
        self.record("recipe_parameter_schema");
        match &self.recipe_parameter_schema {
            Some(f) => f(corpus_id),
            None => Err(refuse("recipe_parameter_schema")),
        }
    }

    async fn dry_run_recipe(
        &self,
        recipe_path: &Path,
        sample_size: usize,
        offline: bool,
    ) -> Result<RecipeDryRunReport> {
        self.record("dry_run_recipe");
        match &self.dry_run_recipe {
            Some(f) => f(recipe_path, sample_size, offline),
            None => Err(refuse("dry_run_recipe")),
        }
    }

    async fn test_recipe_report(
        &self,
        _recipe_path: &Path,
        _sample_size: usize,
        _offline: bool,
    ) -> Result<serde_json::Value> {
        self.record("test_recipe_report");
        Err(refuse("test_recipe_report"))
    }

    fn recipe_vocabulary(&self, recipe_path: &Path) -> Result<Option<RecipeVocabulary>> {
        self.record("recipe_vocabulary");
        match &self.recipe_vocabulary {
            Some(f) => f(recipe_path),
            None => Err(refuse("recipe_vocabulary")),
        }
    }

    fn recipe_enrichment_domain(&self, _recipe_path: &Path) -> Option<String> {
        panic!("{}", unprogrammed("recipe_enrichment_domain"))
    }

    fn spawn_newsworthy_watcher(
        self: Arc<Self>,
        _host: NewsworthyHostFactory,
        _shutdown: tokio::sync::watch::Receiver<bool>,
        _force_tick: tokio::sync::mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        panic!("{}", unprogrammed("spawn_newsworthy_watcher"))
    }

    async fn apply_newsworthy_incremental(
        &self,
        _open: IndexOpener,
        _indexes_dir: PathBuf,
        _corpus_id: String,
        _role: &'static str,
        _doc_ids: Vec<String>,
    ) -> std::result::Result<(), String> {
        self.record("apply_newsworthy_incremental");
        Err(unprogrammed("apply_newsworthy_incremental"))
    }
}

type HarnessRunFn =
    dyn Fn(&Path, &Path, bool) -> std::result::Result<HarnessRunCardView, String> + Send + Sync;

/// The recipe harness's double: an unprogrammed run refuses, naming itself.
/// A daemon test that only slots a harness holds this; one that drives a
/// run programs [`Self::answering`] and proves the run on the harness's own
/// implementor.
#[derive(Default)]
pub struct RecipeHarnessDouble {
    run: Option<Box<HarnessRunFn>>,
}

impl RecipeHarnessDouble {
    /// A run answers `f(harness_root, index_dir, enrich)`.
    pub fn answering(
        f: impl Fn(&Path, &Path, bool) -> std::result::Result<HarnessRunCardView, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            run: Some(Box::new(f)),
        }
    }
}

#[async_trait]
impl RecipeHarnessPort for RecipeHarnessDouble {
    async fn run_recipe_harness(
        &self,
        _recipe_toml: &str,
        harness_root: &Path,
        _sample_size: usize,
        enrich: bool,
        index_dir: &Path,
        _notice: &(dyn for<'s> Fn(&'s str) + Sync),
    ) -> std::result::Result<HarnessRunCardView, String> {
        match &self.run {
            Some(f) => f(harness_root, index_dir, enrich),
            None => {
                Err("RecipeHarnessDouble::run_recipe_harness: not programmed by this test".into())
            }
        }
    }
}
