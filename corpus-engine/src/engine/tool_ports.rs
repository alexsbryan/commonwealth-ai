// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's ports for svrn's tool families, implemented by the engine
//! (pb-ingest-dial-tools; the traits are `corpus_index::ingest_port`'s).
//!
//! The catalog family: the half of sovereign-tools' on-demand catalog ingest
//! that executes ingest — load and patch the content recipe, ingest it
//! inline, fold a shared-target staging corpus into its canonical — moved
//! here behind [`CatalogIngestPort`]; resolving the work, enrichment and link
//! expansion stay the tool's. The plugin family: acquirer and extractor
//! registration.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use corpus_index::ingest_port::{
    CatalogIngestPort, CatalogWork, CatalogWorkError, CatalogWorkIngested, DocFetchFn,
    EmbeddingCluster, EmbeddingClusters, EnrichmentPassRoute, EntityDelta, FieldModelError,
    FolderTieredPort, LocalCorpusPort, ProgressCallback, PromptFn, RecipeIngested,
    SourceFileProgress, WatchedUpdate, WatchedUpdateProgressFn, WatchedUpdateStage,
};

use super::CorpusEngine;
use crate::recipe::Recipe;
use crate::types::CorpusSpec;

/// The plugin family's port: every method delegates to the inherent one.
impl corpus_index::ingest_port::IngestPluginPort for CorpusEngine {
    fn register_acquirer(&self, kind: &str, acquirer: corpus_index::ingest_port::CustomAcquirerFn) {
        CorpusEngine::register_acquirer(self, kind, acquirer)
    }

    fn register_extractor(
        &self,
        kind: &str,
        extractor: corpus_index::ingest_port::CustomExtractorFn,
    ) {
        CorpusEngine::register_extractor(self, kind, extractor)
    }
}

#[async_trait]
impl CatalogIngestPort for CorpusEngine {
    async fn ingest_catalog_work(
        &self,
        work: &CatalogWork,
        progress: Option<ProgressCallback>,
    ) -> Result<CatalogWorkIngested, CatalogWorkError> {
        let mut content_recipe = self
            .registry()
            .fetch_recipe(&work.content_recipe)
            .await
            .map_err(CatalogWorkError::ContentRecipeLoad)?;
        patch_content_recipe(
            &mut content_recipe,
            &work.staging_corpus_id,
            &work.catalog_corpus_id,
            &work.download_url,
        );
        // Respect a content recipe's explicit retrieval-only opt-out
        // (`[enrichment] enabled = false`). Computed before `content_recipe`
        // is moved into the CorpusSpec.
        let opts_out_of_auto_enrichment = content_recipe.opts_out_of_auto_enrichment();
        let mut ingest_result = self
            .ingest(&CorpusSpec::Inline(Box::new(content_recipe)), progress)
            .await
            .map_err(CatalogWorkError::Ingest)?;

        // Shared-target append: when `[catalog].target_corpus_id` is set, fold
        // the staging corpus into the shared canonical (e.g. fetched articles
        // all land in `wikipedia-fetched`). This keeps installed_indexes()
        // bounded — one shared corpus instead of one per fetched article.
        if let Some(final_corpus_id) = &work.shared_target {
            let staging_corpus_id = &work.staging_corpus_id;
            let indexes_dir = self.index_dir().to_path_buf();
            let staging_path = indexes_dir.join(staging_corpus_id);
            let canonical_path = indexes_dir.join(final_corpus_id);
            // Resolve embedding model + dim from the staging corpus
            // we just wrote — those are the only authoritative source.
            let staging_index = self
                .open_index(&staging_path)
                .await
                .map_err(CatalogWorkError::Ingest)?;
            let staging_info = staging_index
                .info()
                .await
                .map_err(CatalogWorkError::Ingest)?;
            drop(staging_index); // release the lance handle before mutating the dir
            let report = crate::append_partition_to_canonical(
                &staging_path,
                &canonical_path,
                final_corpus_id,
                &staging_info.corpus_name,
                &staging_info.embedding_model,
                staging_info.embedding_dimensions,
                staging_info.mesh_sharing,
            )
            .await
            .map_err(CatalogWorkError::Ingest)?;
            tracing::info!(
                staging = %staging_corpus_id,
                canonical = %final_corpus_id,
                inserted = report.chunks_inserted,
                deduped = report.chunks_deduped,
                canonical_after = report.canonical_chunks_after,
                "catalog_ingest: appended staging into shared canonical"
            );
            // Finalise the canonical so retrieval treats it like any
            // other installed corpus:
            //   - stamp kind=Knowledge + parent_corpus_id (catalog hint),
            //   - rebuild vector + FTS so the freshly-appended chunks
            //     are searchable across both retrieval paths,
            //   - mark ingestion complete so installed_indexes() lists it.
            // Without these, `chat inspect` / OICP retrieval skip the
            // dir as "in-progress" and the corpus is invisible.
            if let Ok(canon) = corpus_index::index::CorpusIndex::open(&canonical_path).await {
                // Inherit the parent_corpus_id from the patched content
                // recipe (e.g. wikipedia-article sets parent="wikipedia"
                // so fetched articles surface alongside the curated L5).
                let parent = work.catalog_corpus_id.clone();
                if let Err(e) = canon.set_kind_and_parent(
                    Some(corpus_index::types::CorpusKind::Knowledge),
                    Some(&parent),
                ) {
                    tracing::warn!(
                        canonical = %final_corpus_id,
                        error = %e,
                        "catalog_ingest: set_kind_and_parent failed (non-fatal)"
                    );
                }
                if let Err(e) = canon.build_indexes(true, true, None).await {
                    tracing::warn!(
                        canonical = %final_corpus_id,
                        error = %e,
                        "catalog_ingest: build_indexes after append failed (non-fatal)"
                    );
                }
                if let Err(e) = canon.mark_ingestion_complete() {
                    tracing::warn!(
                        canonical = %final_corpus_id,
                        error = %e,
                        "catalog_ingest: mark_ingestion_complete failed (non-fatal)"
                    );
                }
            }
            // Delete the staging corpus dir — it's served its purpose.
            if let Err(e) = std::fs::remove_dir_all(&staging_path) {
                tracing::warn!(
                    staging = %staging_corpus_id,
                    error = %e,
                    "catalog_ingest: staging cleanup failed (non-fatal)"
                );
            }
            // Surface the post-append count to the caller as the
            // chunks_created result (more useful than the staging count).
            ingest_result.chunks_created = report.chunks_inserted;
        }

        Ok(CatalogWorkIngested {
            chunks_created: ingest_result.chunks_created,
            opts_out_of_auto_enrichment,
        })
    }
}

/// The local-corpus family's port (pb-ingest-dial-tools-local): the watched
/// update, the clustering and the field-model enrichment that sovereign-tools'
/// local_corpus and knowledge_view drove on the engine directly now run here.
#[async_trait]
impl LocalCorpusPort for CorpusEngine {
    async fn ingest_recipe_path(
        &self,
        recipe_path: &Path,
        progress: Option<ProgressCallback>,
    ) -> corpus_index::Result<RecipeIngested> {
        let result = self
            .ingest(&CorpusSpec::RecipePath(recipe_path.to_path_buf()), progress)
            .await?;
        Ok(RecipeIngested {
            corpus_id: result.corpus_id,
            chunks_created: result.chunks_created,
        })
    }

    async fn ensure_empty_index(&self, recipe_path: &Path) -> corpus_index::Result<()> {
        CorpusEngine::ensure_empty_index(self, &CorpusSpec::RecipePath(recipe_path.to_path_buf()))
            .await
            .map(drop)
    }

    fn cancel_corpus_ingest(&self, corpus_id: &str) -> bool {
        CorpusEngine::cancel_corpus_ingest(self, corpus_id)
    }

    fn ingest_in_flight(&self, corpus_id: &str) -> bool {
        self.cancel_registry().get(corpus_id).is_some()
    }

    fn remove_corpus_everything(&self, corpus_id: &str) -> corpus_index::Result<()> {
        CorpusEngine::remove_corpus_everything(self, corpus_id)
    }

    fn atlas_teardown(&self, index_dir: &Path, corpus_id: &str) -> std::io::Result<()> {
        crate::atlas_teardown(index_dir, corpus_id)
    }

    fn source_file_progress(&self, corpus_dir: &Path) -> Option<SourceFileProgress> {
        let manifest = crate::progress::SourceFileManifest::load(corpus_dir).ok()??;
        let done = manifest
            .files
            .iter()
            .filter(|f| matches!(f.status, crate::progress::SourceFileStatus::Complete { .. }))
            .count();
        Some(SourceFileProgress {
            done,
            total: manifest.files.len(),
        })
    }

    async fn reindex_changed_sources_tiered(&self, corpus_id: &str, source_doc_ids: &[String]) {
        CorpusEngine::reindex_changed_sources_tiered(self, corpus_id, source_doc_ids).await
    }

    async fn apply_watched_update(
        self: Arc<Self>,
        update: &WatchedUpdate,
        fetch: DocFetchFn,
        progress: WatchedUpdateProgressFn,
    ) -> corpus_index::Result<()> {
        use crate::update::delta::{
            CorpusUpdater, ManifestDiff, UpdatePhase, UpdateProgress, VersionManifest,
        };
        let new_manifest = VersionManifest {
            corpus_id: update.corpus_id.clone(),
            version: update.version.clone(),
            entries: update.entries.clone(),
        };
        let mdiff = ManifestDiff {
            new_documents: update.new_documents.clone(),
            updated_documents: update.updated_documents.clone(),
            deleted_documents: update.deleted_documents.clone(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<UpdateProgress>(32);
        let pump = tokio::spawn(async move {
            while let Some(p) = rx.recv().await {
                let stage = match p.phase {
                    UpdatePhase::Deletions => WatchedUpdateStage::Deletions,
                    UpdatePhase::Updates => WatchedUpdateStage::Updates,
                    UpdatePhase::Additions => WatchedUpdateStage::Additions,
                };
                progress(stage, p.current, p.total);
            }
        });
        let updater = CorpusUpdater::new(self).with_progress_tx(tx);
        let result = updater
            .apply_update(&update.corpus_id, &mdiff, &new_manifest, |doc_id: &str| {
                fetch(doc_id)
            })
            .await;
        drop(updater);
        let _ = pump.await;
        result
    }

    async fn recipe_enrichment_type(
        &self,
        corpus_id: &str,
    ) -> corpus_index::Result<Option<String>> {
        Ok(self
            .load_recipe(corpus_id)
            .await?
            .enrichment
            .map(|e| e.enrichment_type))
    }

    fn enrichment_pass_route(&self, enrichment_type: &str) -> Option<EnrichmentPassRoute> {
        self.enrichment_passes()
            .get(enrichment_type)
            .map(|p| EnrichmentPassRoute {
                pass_id: p.id().to_string(),
                is_atlas: p.id() == super::pass::ATLAS,
            })
    }

    fn prompt_fn(
        &self,
        inference: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    ) -> PromptFn {
        let infer = crate::enrichment::provider_inference::inference_to_inference_fn(inference);
        Arc::new(move |prompt: String| {
            let infer = Arc::clone(&infer);
            Box::pin(async move {
                infer(
                    &crate::enrichment::pipeline::ChatPrompt::new("", prompt.as_str()),
                    None,
                )
                .await
            })
        })
    }

    async fn cluster_embeddings(
        &self,
        index: &corpus_index::index::CorpusIndex,
        min_cluster_size: usize,
        on_step: &(dyn Fn(&str) + Send + Sync),
    ) -> corpus_index::Result<EmbeddingClusters> {
        use crate::enrichment::clustering::EnrichmentProgress;
        let cluster_cfg = crate::enrichment::domain::ClusteringConfig {
            min_cluster_size,
            epsilon: 0.2,
            label_sample_size: 5,
            max_cluster_points: 10_000,
            reduced_dims: 0,
        };
        let stage_cb = |p: EnrichmentProgress| {
            if let EnrichmentProgress::ClusteringStep { step, .. } = &p {
                on_step(step);
            }
        };
        let result =
            crate::enrichment::clustering::cluster_embeddings(index, &cluster_cfg, &stage_cb)
                .await?;
        Ok(EmbeddingClusters {
            assignments: result.assignments,
            clusters: result
                .clusters
                .into_iter()
                .map(|c| EmbeddingCluster {
                    id: c.id,
                    size: c.size,
                    centroid: c.centroid,
                    central_chunks: c.central_chunks,
                })
                .collect(),
        })
    }

    fn embed_fn(&self) -> corpus_index::types::EmbedFn {
        CorpusEngine::embed_fn(self)
    }

    async fn enrich_field_model(
        &self,
        index: &corpus_index::index::CorpusIndex,
        recipe: &serde_json::Value,
    ) -> Result<String, FieldModelError> {
        let recipe: Recipe = serde_json::from_value(recipe.clone()).map_err(|e| {
            FieldModelError::Construct(corpus_index::Error::Recipe(format!(
                "knowledge-view recipe document: {e}"
            )))
        })?;
        let inference = self.inference.clone().ok_or_else(|| {
            FieldModelError::Construct(corpus_index::Error::Recipe(
                "no enrichment inference is configured on the engine".into(),
            ))
        })?;
        let field_engine = crate::enrichment::field_engine::FieldModelEngine::from_recipe(
            &recipe,
            CorpusEngine::embed_fn(self),
            inference,
        )
        .map_err(FieldModelError::Construct)?;
        let corpus_id = recipe.corpus.id.as_str();
        let progress = |p: crate::enrichment::clustering::EnrichmentProgress| {
            tracing::debug!(view_id = corpus_id, ?p, "enrichment progress");
        };
        let stats = field_engine
            .enrich(index, &progress)
            .await
            .map_err(FieldModelError::Enrich)?;
        Ok(format!("{stats:?}"))
    }
}

/// The watched-folder driver's tiered build: the tiered provider and the
/// optional entity extractor, composed at daemon boot.
pub struct FolderTiered {
    provider: crate::enrichment::tiered::TieredProviderHandle,
    entity_extractor: Option<crate::enrichment::tiered::ChunkEntityExtractorHandle>,
}

impl FolderTiered {
    /// Compose the driver's tiered build.
    pub fn new(
        provider: crate::enrichment::tiered::TieredProviderHandle,
        entity_extractor: Option<crate::enrichment::tiered::ChunkEntityExtractorHandle>,
    ) -> Self {
        Self {
            provider,
            entity_extractor,
        }
    }
}

#[async_trait]
impl FolderTieredPort for FolderTiered {
    async fn reenrich_sources(
        &self,
        corpus_id: &str,
        source_doc_ids: &[String],
    ) -> corpus_index::Result<()> {
        self.provider
            .reenrich_sources(corpus_id, source_doc_ids)
            .await
    }

    fn has_entity_extractor(&self) -> bool {
        self.entity_extractor.is_some()
    }

    async fn extract_entity_delta(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> Option<corpus_index::Result<EntityDelta>> {
        let extractor = self.entity_extractor.as_ref()?;
        Some(
            extractor
                .extract_delta_for_corpus(corpus_id, index_path)
                .await
                .map(|o| {
                    // A refusal is not a failure, but it is not nothing
                    // either — record it where the operator already looks
                    // (ARCH 6).
                    crate::enrichment::tiered::report_refused_over_cap(
                        index_path,
                        corpus_id,
                        o.refused_over_cap as u64,
                    );
                    EntityDelta {
                        mentions: o.mentions,
                        refused_over_cap: o.refused_over_cap,
                    }
                }),
        )
    }

    async fn run_folder_tiered_enrichment(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> corpus_index::Result<()> {
        crate::enrichment::tiered::run_folder_tiered_enrichment(
            corpus_id,
            index_path,
            Some(&self.provider),
            None,
        )
        .await
        .map(drop)
    }
}

/// Patch a content recipe in place with the on-demand override
/// fields. Pure for testability — no IO, no engine calls.
///
/// `parent_corpus_id` is the catalog corpus by default. The recipe
/// itself may pre-declare a different parent (e.g. `wikipedia-article`
/// sets `parent_corpus_id = "wikipedia"` so fetched Wikipedia
/// articles surface under the user's existing Wikipedia corpus
/// rather than under `wikipedia-catalog`); when the recipe has a
/// non-empty `parent_corpus_id` we keep it.
pub(crate) fn patch_content_recipe(
    recipe: &mut Recipe,
    new_corpus_id: &str,
    parent_corpus_id: &str,
    download_url: &str,
) {
    recipe.corpus.id = new_corpus_id.to_string();
    if recipe
        .corpus
        .parent_corpus_id
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        recipe.corpus.parent_corpus_id = Some(parent_corpus_id.to_string());
    }
    // The on-demand guard in `ingest()` only relaxes when the recipe
    // is handed via CorpusSpec::Inline — leave `on_demand` set so a
    // future direct ingest of the *patched* recipe (saved to disk)
    // would still be refused.
    if let crate::recipe::AcquirerConfig::BulkDownload { url, urls, .. } = &mut recipe.acquire {
        *url = Some(download_url.to_string());
        *urls = None;
    }
}

/// The merge family's port (sovereign-grants): each method is the engine's
/// own merge, finalize or projection, unchanged.
#[async_trait]
impl corpus_index::ingest_port::merge::PartitionMergePort for CorpusEngine {
    async fn merge_partitions(
        &self,
        partitions: &[std::path::PathBuf],
        output: &Path,
    ) -> corpus_index::Result<corpus_index::types::IndexInfo> {
        CorpusEngine::merge_partitions(self, partitions, output).await
    }

    async fn finalize_canonical(
        &self,
        canonical: &corpus_index::index::CorpusIndex,
        corpus_id: &str,
    ) -> corpus_index::Result<()> {
        crate::sharding::finalize_canonical(canonical, corpus_id, None).await
    }

    async fn merge_partitions_into_canonical(
        &self,
        index_dir: &Path,
        corpus_id: &str,
        progress: Option<Arc<dyn Fn(crate::MergePhaseProgress) + Send + Sync>>,
    ) -> corpus_index::Result<crate::PartitionMergeReport> {
        crate::sharding::merge_partitions_into_canonical(index_dir, corpus_id, progress).await
    }

    async fn project_alignment(
        &self,
        canonical_path: &Path,
        home: &Path,
    ) -> corpus_index::Result<crate::alignment_projector::ProjectReport> {
        crate::alignment_projector::project(canonical_path, home).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{AcquirerConfig, ChunkerConfig, CorpusMeta, ExtractorConfig, IndexConfig};

    fn fake_content_recipe() -> Recipe {
        Recipe {
            corpus: CorpusMeta {
                id: "gutenberg-work".into(),
                name: "Gutenberg Work".into(),
                description: String::new(),
                license: "Public Domain".into(),
                mesh_sharing: true,
                scope: None,
                query_sharing: None,
                grantable: false,
                size_compressed_gb: 0.0,
                size_indexed_gb: 0.0,
                schema_version: 1,
                kind: corpus_index::types::CorpusKind::Knowledge,
                on_demand: true,
                parent_corpus_id: None,
                mutable_merge: None,
            },
            acquire: AcquirerConfig::BulkDownload {
                url: Some("https://example.com/PLACEHOLDER".into()),
                urls: None,
                resume: true,
            },
            extract: ExtractorConfig::Plaintext {
                title_pattern: None,
                strip_boilerplate: None,
            },
            chunk: ChunkerConfig::Sentence { max_chars: 2048 },
            index: IndexConfig::default(),
            authority: None,
            enrichment: None,
            update: None,
            prebuilt: None,
            catalog: None,
            filters: Vec::new(),
            filter_mode: Default::default(),
            parameters: Default::default(),
            resolved_parameters: Default::default(),
            display: None,
            retrieval: Default::default(),
        }
    }

    #[test]
    fn patch_content_recipe_overrides_id_url_and_parent() {
        let mut r = fake_content_recipe();
        patch_content_recipe(
            &mut r,
            "gutenberg-2701",
            "gutenberg",
            "https://www.gutenberg.org/cache/epub/2701/pg2701.txt",
        );
        assert_eq!(r.corpus.id, "gutenberg-2701");
        assert_eq!(r.corpus.parent_corpus_id.as_deref(), Some("gutenberg"));
        assert!(r.corpus.on_demand, "on_demand stays true so a future direct ingest of this patched recipe would still be refused");
        match r.acquire {
            AcquirerConfig::BulkDownload { url, urls, .. } => {
                assert_eq!(
                    url.as_deref(),
                    Some("https://www.gutenberg.org/cache/epub/2701/pg2701.txt")
                );
                assert!(urls.is_none());
            }
            other => panic!("expected BulkDownload, got {other:?}"),
        }
    }

    #[test]
    fn patch_respects_recipe_declared_parent() {
        // The wikipedia-article recipe pre-declares
        // `parent_corpus_id = "wikipedia"` so fetched articles
        // surface under the user's existing Wikipedia corpus
        // rather than the catalog id (`wikipedia-catalog`).
        let mut r = fake_content_recipe();
        r.corpus.parent_corpus_id = Some("wikipedia".into());
        patch_content_recipe(
            &mut r,
            "wikipedia-catalog-Roman_Empire",
            "wikipedia-catalog", // catalog id — would be the default
            "https://en.wikipedia.org/w/api.php?action=parse&page=Roman_Empire&redirects=1",
        );
        assert_eq!(
            r.corpus.parent_corpus_id.as_deref(),
            Some("wikipedia"),
            "recipe-declared parent should win over the catalog default"
        );
    }

    fn engine_at(dir: &std::path::Path) -> crate::CorpusEngine {
        let embed: corpus_index::types::EmbedFn =
            std::sync::Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0f32; 8]) }));
        crate::CorpusEngine::new(dir.join("recipes"), dir.join("idx"), embed)
    }

    /// sovereign-tools' sec_edgar `registering_makes_the_kind_resolvable_by_the_engine`,
    /// the engine half (phase-b-47): an acquirer registered through the plugin
    /// port is the one the `Custom { kind }` dispatch resolves.
    #[test]
    fn an_acquirer_registered_through_the_plugin_port_resolves_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine_at(dir.path());
        assert!(engine.custom_acquirer("sec_edgar").is_none());
        let acquirer: corpus_index::ingest_port::CustomAcquirerFn =
            std::sync::Arc::new(|_, d| Box::pin(async move { Ok(d) }));
        corpus_index::ingest_port::IngestPluginPort::register_acquirer(
            &engine,
            "sec_edgar",
            acquirer,
        );
        assert!(engine.custom_acquirer("sec_edgar").is_some());
    }

    /// sovereign-tools' knowledge_view manager tests, the engine half
    /// (phase-b-47): `open_index_for_corpus` opens `<index_dir>/<corpus_id>`,
    /// the index the double opens there with the leaf's `CorpusIndex::open`.
    #[tokio::test]
    async fn open_index_for_corpus_opens_the_corpus_dir_under_index_dir() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine_at(dir.path());
        let via_engine = corpus_index::source::CorpusReadPort::open_index_for_corpus(
            &engine,
            "personal-knowledge",
        )
        .await;
        let via_leaf = corpus_index::index::CorpusIndex::open(
            &dir.path().join("idx").join("personal-knowledge"),
        )
        .await;
        assert_eq!(via_engine.is_ok(), via_leaf.is_ok());
        if let (Ok(a), Ok(b)) = (via_engine, via_leaf) {
            assert_eq!(a.path(), b.path());
        }
    }
}
