// SPDX-License-Identifier: AGPL-3.0-or-later
//! The test double of this module's ports (FIVE_PROGRAMS "Where a
//! cross-program test lives", phase-b-47). A svrn test drives its own code
//! against it; the ports' behaviour is proven on ingest's implementors
//! (`impl LocalCorpusPort` / `CatalogIngestPort for CorpusEngine`), in
//! corpus-engine's tests.
//!
//! One struct serves [`LocalCorpusPort`], [`CatalogIngestPort`] and their
//! shared supertraits, so `CorpusReadPort` has one set of handlers rather
//! than one per port. A method a test can program has an `on_*`; every
//! other method, and a programmable one left unprogrammed, never answers
//! success-shaped (principle 6): a `Result` method returns an `Err` naming
//! itself, any other panics naming itself. A later row that drives another
//! method gives it an `on_*` here. Registrations through
//! [`IngestPluginPort`] are the one thing the double keeps: that is the
//! method's whole contract, and a test reads them back.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    CatalogIngestPort, CatalogWork, CatalogWorkError, CatalogWorkIngested, CustomAcquirerFn,
    CustomExtractorFn, DocFetchFn, EmbeddingClusters, EnrichmentPassRoute, FieldModelError,
    IngestPluginPort, LocalCorpusPort, ProgressCallback, PromptFn, RecipeIngested,
    SourceFileProgress, WatchedUpdate, WatchedUpdateProgressFn,
};
use crate::index::CorpusIndex;
use crate::recipe::CatalogConfig;
use crate::source::{CorpusReadPort, IndexSource};
use crate::types::{BuiltinCorpus, EmbedFn, IncompleteIngest, IndexInfo};
use crate::{Error, Result};

fn unprogrammed(method: &str) -> String {
    format!("IngestPortDouble::{method}: not programmed by this test")
}

fn refuse(method: &str) -> Error {
    Error::Io(std::io::Error::other(unprogrammed(method)))
}

type OpenIndexFn = dyn Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CorpusIndex>> + Send>>
    + Send
    + Sync;

/// The ingest ports' double a svrn test programs.
#[derive(Default)]
pub struct IngestPortDouble {
    calls: Mutex<Vec<&'static str>>,
    acquirers: Mutex<Vec<String>>,
    extractors: Mutex<Vec<String>>,
    index_dir: Option<PathBuf>,
    open_index_for_corpus: Option<Box<OpenIndexFn>>,
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
}

#[async_trait]
impl IndexSource for IngestPortDouble {
    async fn usable_indexes(&self) -> Result<Vec<IndexInfo>> {
        self.record("usable_indexes");
        Err(refuse("usable_indexes"))
    }

    async fn open_index(&self, _path: &Path) -> Result<CorpusIndex> {
        self.record("open_index");
        Err(refuse("open_index"))
    }
}

#[async_trait]
impl CorpusReadPort for IngestPortDouble {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        self.record("embed");
        Err(refuse("embed"))
    }

    async fn installed_indexes(&self) -> Result<Vec<IndexInfo>> {
        self.record("installed_indexes");
        Err(refuse("installed_indexes"))
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
        panic!("{}", unprogrammed("foreground_lease"))
    }

    fn builtin_corpora(&self) -> Vec<BuiltinCorpus> {
        panic!("{}", unprogrammed("builtin_corpora"))
    }

    fn incomplete_ingests(&self) -> Vec<IncompleteIngest> {
        panic!("{}", unprogrammed("incomplete_ingests"))
    }

    fn declared_authority_tool(&self, _corpus_id: &str) -> Option<String> {
        panic!("{}", unprogrammed("declared_authority_tool"))
    }

    async fn catalog_config(&self, _corpus_id: &str) -> Result<Option<CatalogConfig>> {
        self.record("catalog_config");
        Err(refuse("catalog_config"))
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
        _work: &CatalogWork,
        _progress: Option<ProgressCallback>,
    ) -> std::result::Result<CatalogWorkIngested, CatalogWorkError> {
        self.record("ingest_catalog_work");
        Err(CatalogWorkError::Ingest(refuse("ingest_catalog_work")))
    }
}

#[async_trait]
impl LocalCorpusPort for IngestPortDouble {
    async fn ingest_recipe_path(
        &self,
        _recipe_path: &Path,
        _progress: Option<ProgressCallback>,
    ) -> Result<RecipeIngested> {
        self.record("ingest_recipe_path");
        Err(refuse("ingest_recipe_path"))
    }

    async fn ensure_empty_index(&self, _recipe_path: &Path) -> Result<()> {
        self.record("ensure_empty_index");
        Err(refuse("ensure_empty_index"))
    }

    fn cancel_corpus_ingest(&self, _corpus_id: &str) -> bool {
        panic!("{}", unprogrammed("cancel_corpus_ingest"))
    }

    fn ingest_in_flight(&self, _corpus_id: &str) -> bool {
        panic!("{}", unprogrammed("ingest_in_flight"))
    }

    fn remove_corpus_everything(&self, _corpus_id: &str) -> Result<()> {
        self.record("remove_corpus_everything");
        Err(refuse("remove_corpus_everything"))
    }

    fn atlas_teardown(&self, _index_dir: &Path, _corpus_id: &str) -> std::io::Result<()> {
        self.record("atlas_teardown");
        Err(std::io::Error::other(unprogrammed("atlas_teardown")))
    }

    fn source_file_progress(&self, _corpus_dir: &Path) -> Option<SourceFileProgress> {
        panic!("{}", unprogrammed("source_file_progress"))
    }

    async fn reindex_changed_sources_tiered(&self, _corpus_id: &str, _source_doc_ids: &[String]) {
        panic!("{}", unprogrammed("reindex_changed_sources_tiered"))
    }

    async fn apply_watched_update(
        self: Arc<Self>,
        _update: &WatchedUpdate,
        _fetch: DocFetchFn,
        _progress: WatchedUpdateProgressFn,
    ) -> Result<()> {
        self.record("apply_watched_update");
        Err(refuse("apply_watched_update"))
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
        panic!("{}", unprogrammed("embed_fn"))
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
    fn registrations_are_kept_by_kind() {
        let d = IngestPortDouble::new();
        let acquirer: CustomAcquirerFn = Arc::new(|_, dir| Box::pin(async move { Ok(dir) }));
        d.register_acquirer("sec_edgar", acquirer);
        assert_eq!(d.registered_acquirers(), vec!["sec_edgar".to_string()]);
    }
}
