// SPDX-License-Identifier: AGPL-3.0-or-later
//! The catalog family's ingest port: ingest one catalog work (pb-ingest-dial-tools).
//!
//! The half of sovereign-tools' on-demand catalog ingest that executes ingest —
//! load and patch the content recipe, ingest it inline, fold a shared-target
//! staging corpus into its canonical — moved here behind
//! [`CatalogIngestPort`]; resolving the work, enrichment and link expansion
//! stay the tool's.

use async_trait::async_trait;
use corpus_index::ingest_port::{
    CatalogIngestPort, CatalogWork, CatalogWorkError, CatalogWorkIngested, ProgressCallback,
};

use super::CorpusEngine;
use crate::recipe::Recipe;
use crate::types::CorpusSpec;

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
}
