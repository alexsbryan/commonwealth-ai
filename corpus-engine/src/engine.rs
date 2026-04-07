//! CorpusEngine — orchestrates acquisition, extraction, chunking,
//! embedding, and indexing of corpus data.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::acquirers::bulk_download::BulkDownloader;
use crate::acquirers::huggingface::HuggingFaceDatasetAcquirer;
use crate::acquirers::local_file::LocalFileAcquirer;
use crate::chunkers::{self, Chunker};
use crate::error::{Error, Result};
use crate::extractors::{self, Extractor};
use crate::index::{CorpusIndex, InsertChunk};
use crate::progress::{IngestProgress, ProgressCallback};
use crate::recipe::{AcquirerConfig, ChunkerConfig, ExtractorConfig, Recipe};
use crate::types::{
    BuiltinCorpus, ChunkRange, CorpusSpec, EmbedFn, IndexInfo, IndexStats,
    IngestResult, ShardInfo,
};

const EMBED_BATCH_SIZE: usize = 64;

pub struct CorpusEngine {
    recipes_dir: PathBuf,
    index_dir: PathBuf,
    embed: EmbedFn,
    /// Optional inference function. Required only for the enrichment phase.
    inference: Option<crate::types::InferenceFn>,
    expected_embedding_model: String,
}

impl CorpusEngine {
    pub fn new(
        recipes_dir: PathBuf,
        index_dir: PathBuf,
        embed: EmbedFn,
    ) -> Self {
        Self {
            recipes_dir,
            index_dir,
            embed,
            inference: None,
            expected_embedding_model: "nomic-embed-text-v2".to_string(),
        }
    }

    pub fn with_embedding_model(mut self, model: &str) -> Self {
        self.expected_embedding_model = model.to_string();
        self
    }

    /// Provide an inference function for the optional enrichment phase.
    /// Without this, recipes that request `[enrichment] enabled = true`
    /// will log a warning and skip enrichment.
    pub fn with_inference_fn(mut self, inference: crate::types::InferenceFn) -> Self {
        self.inference = Some(inference);
        self
    }

    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// Embed a piece of text via the engine's embedding function.
    /// Exposed for downstream callers (tools, etc.) that need to construct
    /// query embeddings using the same model the corpus was indexed with.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        (self.embed)(text).await
    }

    // ── Ingestion ───────────────────────────────────────

    /// List built-in corpus definitions.
    pub fn builtin_corpora(&self) -> Vec<BuiltinCorpus> {
        crate::recipe::builtin_recipes()
            .into_iter()
            .map(|r| BuiltinCorpus {
                id: r.corpus.id,
                name: r.corpus.name,
                description: r.corpus.description,
                size_compressed_gb: r.corpus.size_compressed_gb,
                size_indexed_gb: r.corpus.size_indexed_gb,
                license: r.corpus.license,
                mesh_sharing: r.corpus.mesh_sharing,
            })
            .collect()
    }

    /// Discover community recipes in the recipes directory.
    pub fn discover_recipes(&self) -> Result<Vec<Recipe>> {
        let mut recipes = Vec::new();
        if self.recipes_dir.is_dir() {
            for entry in std::fs::read_dir(&self.recipes_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    match Recipe::from_file(&path) {
                        Ok(r) => recipes.push(r),
                        Err(e) => {
                            eprintln!("Skipping recipe {}: {e}", path.display());
                        }
                    }
                }
            }
        }
        Ok(recipes)
    }

    /// Ingest a corpus from source. Downloads, parses, chunks,
    /// embeds, and writes a complete index.
    ///
    /// Failure modes are surfaced cleanly:
    ///
    /// 1. **Pre-flight check.** Before touching disk, the engine asks
    ///    the configured `EmbedFn` to embed a tiny smoke string. If
    ///    that fails (most commonly because no embedding model is
    ///    configured), the error is returned immediately and no
    ///    index directory is created. This prevents the "ghost
    ///    install" state where a half-built directory makes the UI
    ///    think a corpus is installed.
    ///
    /// 2. **Cleanup on failure.** If any step *after* the pre-flight
    ///    fails (download error, parquet schema mismatch, embed
    ///    overflow mid-batch, …), the partial index directory is
    ///    deleted before the error propagates so the corpus appears
    ///    as "not installed" in the UI on the next refresh.
    pub async fn ingest(
        &self,
        corpus: &CorpusSpec,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        let mut recipe = self.resolve_recipe(corpus)?;

        // Ensure parent directory exists so `_downloads` and the
        // per-corpus index dir can be created underneath.
        std::fs::create_dir_all(&self.index_dir)?;

        // ── Pre-flight: validate the embed function works ─────────
        //
        // We do this before creating the index directory so a missing
        // or broken embedder fails fast with no on-disk side effects.
        // The smoke string is short and the result is discarded —
        // we only care that the call returns Ok and produces a vector
        // of the expected dimensionality.
        let probe = (self.embed)("probe").await.map_err(|e| {
            Error::Embed(format!(
                "Embedding function is not available: {e}. \
                 Configure an embedding model before installing corpora."
            ))
        })?;
        if probe.is_empty() {
            return Err(Error::Embed(
                "Embedding function returned an empty vector. \
                 The configured embed model may be misloaded."
                    .to_string(),
            ));
        }
        // Auto-adapt: use the model's actual output dimensionality rather
        // than rejecting it. Recipe defaults (768 for nomic) are just
        // hints — the real schema is determined by what the model returns.
        if probe.len() != recipe.index.embedding_dimensions {
            tracing::info!(
                "Embedding model returns {} dimensions; recipe default was {}. \
                 Using actual model dimensions.",
                probe.len(),
                recipe.index.embedding_dimensions,
            );
            recipe.index.embedding_dimensions = probe.len();
        }

        // ── Run the actual pipeline with cleanup-on-failure ───────
        let index_path = self.index_dir.join(&recipe.corpus.id);
        let result = self
            .ingest_inner(&recipe, &index_path, &progress)
            .await;

        match result {
            Ok(r) => Ok(r),
            Err(e) => {
                // Wipe the half-built index directory so the UI doesn't
                // misreport a failed install as "installed".
                if index_path.exists() {
                    if let Err(rm) = std::fs::remove_dir_all(&index_path) {
                        tracing::warn!(
                            "Failed to clean up partial index at {}: {rm}",
                            index_path.display()
                        );
                    }
                }
                Err(e)
            }
        }
    }

    /// The actual ingest pipeline. Pulled into its own function so the
    /// public `ingest()` can wrap it with cleanup-on-failure logic.
    async fn ingest_inner(
        &self,
        recipe: &Recipe,
        index_path: &Path,
        progress: &Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        let start = Instant::now();

        // Step 1: Acquire source data.
        let download_dir = self.index_dir.join("_downloads");
        let source_path = self
            .acquire_source(recipe, &download_dir, progress)
            .await?;

        // Step 2: Extract documents.
        let extractor = self.make_extractor(&recipe.extract);
        let doc_iter = extractor.extract(&source_path)?;

        // Step 3: Chunk, embed, and index.
        let chunker = self.make_chunker(&recipe.chunk);

        let index = CorpusIndex::create(
            index_path,
            &recipe.corpus.id,
            &recipe.corpus.name,
            &recipe.index.embedding_model,
            recipe.index.embedding_dimensions,
            recipe.corpus.mesh_sharing,
            &recipe.corpus.license,
        )
        .await?;

        let mut total_chunks = 0u64;
        let mut batch: Vec<(InsertChunk, Vec<f32>)> = Vec::new();

        for doc_result in doc_iter {
            let doc = match doc_result {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Skipping document: {e}");
                    continue;
                }
            };

            let text_chunks = chunker.chunk(&doc.content);

            for tc in text_chunks {
                let content = if let Some(ref title) = doc.title {
                    if !tc.content.starts_with(title.as_str()) {
                        format!("{title}\n\n{}", tc.content)
                    } else {
                        tc.content
                    }
                } else {
                    tc.content
                };

                let embedding = (self.embed)(&content).await?;

                batch.push((
                    InsertChunk {
                        content,
                        title: doc.title.clone(),
                        url: doc.url.clone(),
                        metadata: doc.metadata.as_ref().map(|m| m.to_string()),
                    },
                    embedding,
                ));

                if batch.len() >= EMBED_BATCH_SIZE {
                    total_chunks += batch.len() as u64;
                    index.insert_batch(&batch).await?;
                    batch.clear();

                    if let Some(ref cb) = progress {
                        cb(IngestProgress::Embedding {
                            chunks_embedded: total_chunks,
                            total: 0,
                        });
                    }
                }
            }
        }

        // Flush remaining.
        if !batch.is_empty() {
            total_chunks += batch.len() as u64;
            index.insert_batch(&batch).await?;
        }

        // A pipeline that produced zero chunks is almost always a bug
        // (wrong column name, empty parquet, all docs filtered out).
        // Surface this rather than leaving an empty index that pretends
        // to be installed.
        if total_chunks == 0 {
            return Err(Error::Extraction(format!(
                "Ingest produced zero chunks for corpus '{}'. \
                 The source may be empty, the extractor may be \
                 misconfigured, or every document may have been filtered.",
                recipe.corpus.id,
            )));
        }

        // Build search indexes (IVF-PQ + FTS).
        index.build_indexes().await?;

        // Optional enrichment phase: extract claims and relationships.
        if let Some(enrichment_config) = recipe.enrichment.as_ref() {
            if enrichment_config.enabled {
                match self.inference.as_ref() {
                    Some(inference) => {
                        let enricher = crate::enrichment::EnrichmentEngine::new(
                            self.embed.clone(),
                            inference.clone(),
                        );
                        let claims = enricher
                            .extract_claims(&index, enrichment_config, progress)
                            .await?;
                        index.store_claims(&claims).await?;

                        if enrichment_config.extract_relationships {
                            let rels = enricher
                                .extract_relationships(&claims, enrichment_config, progress)
                                .await?;
                            index.store_relationships(&rels).await?;
                        }

                        index.build_claims_index().await?;
                    }
                    None => {
                        tracing::warn!(
                            "Recipe '{}' requests enrichment but no InferenceFn was provided to CorpusEngine — skipping",
                            recipe.corpus.id,
                        );
                    }
                }
            }
        }

        let duration_secs = start.elapsed().as_secs();
        let info = index.info().await?;

        if let Some(ref cb) = progress {
            cb(IngestProgress::Complete {
                total_chunks,
                duration_secs,
            });
        }

        Ok(IngestResult {
            corpus_id: recipe.corpus.id.clone(),
            chunks_created: total_chunks,
            index_size_bytes: info.index_size_bytes,
            duration_secs,
        })
    }

    // ── Index Management ────────────────────────────────

    /// List all indexes present in the index directory.
    /// Each index is a subdirectory containing LanceDB data.
    pub async fn installed_indexes(&self) -> Result<Vec<IndexInfo>> {
        let mut indexes = Vec::new();
        if !self.index_dir.is_dir() {
            return Ok(indexes);
        }

        for entry in std::fs::read_dir(&self.index_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip internal directories.
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if name.starts_with('_') {
                continue;
            }
            // Check for _corpus_meta.json to identify valid indexes.
            if !path.join("_corpus_meta.json").exists() {
                continue;
            }
            match CorpusIndex::open(&path).await {
                Ok(idx) => match idx.info().await {
                    Ok(info) => indexes.push(info),
                    Err(e) => {
                        eprintln!("Skipping {}: {e}", path.display());
                    }
                },
                Err(e) => {
                    eprintln!("Skipping {}: {e}", path.display());
                }
            }
        }
        Ok(indexes)
    }

    /// Open an index for search. Validates embedding model.
    pub async fn open_index(&self, path: &Path) -> Result<CorpusIndex> {
        let index = CorpusIndex::open(path).await?;
        let info = index.info().await?;

        if info.embedding_model != self.expected_embedding_model {
            return Err(Error::IncompatibleEmbedding {
                index_model: info.embedding_model,
                expected_model: self.expected_embedding_model.clone(),
                path: path.to_owned(),
            });
        }

        Ok(index)
    }

    /// Remove an index directory.
    pub fn remove_index(&self, corpus_id: &str) -> Result<()> {
        let path = self.index_dir.join(corpus_id);
        if path.exists() && path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        }
        Ok(())
    }

    /// Open an index by corpus ID. Convenience wrapper for tools that
    /// don't want to construct a path manually.
    pub async fn open_index_for_corpus(&self, corpus_id: &str) -> Result<CorpusIndex> {
        let path = self.index_dir.join(corpus_id);
        self.open_index(&path).await
    }

    /// Return the IDs of all installed corpora that have an enriched
    /// `claims` table. Used by the `ClaimSearchTool` and
    /// `EpistemicLandscapeTool` to know which corpora to consult.
    pub async fn enriched_corpus_ids(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for info in self.installed_indexes().await? {
            // Try to open the index and check for a claims table.
            // We swallow open errors here so a single broken index
            // doesn't prevent the rest from being listed.
            if let Ok(index) = CorpusIndex::open(&info.path).await {
                if index.has_claims_table().await {
                    out.push(info.corpus_id);
                }
            }
        }
        Ok(out)
    }

    // ── Shard Operations ────────────────────────────────

    /// Report chunk ID range, count, and size for an index.
    pub async fn index_stats(&self, corpus_id: &str) -> Result<IndexStats> {
        let path = self.find_index_path(corpus_id)?;
        crate::sharding::index_stats(&path).await
    }

    /// Extract a subset of an existing index into a new directory.
    pub async fn extract_shard(
        &self,
        source_corpus_id: &str,
        chunk_range: ChunkRange,
        output_path: &Path,
    ) -> Result<ShardInfo> {
        let source_path = self.find_index_path(source_corpus_id)?;
        crate::sharding::extract_shard(&source_path, chunk_range, output_path).await
    }

    /// Merge multiple shard directories into a single index.
    pub async fn merge_shards(
        &self,
        shard_paths: &[PathBuf],
        output_path: &Path,
    ) -> Result<IndexInfo> {
        crate::sharding::merge_shards(shard_paths, output_path).await
    }

    // ── Private helpers ────────────────────────────────

    fn resolve_recipe(&self, corpus: &CorpusSpec) -> Result<Recipe> {
        match corpus {
            CorpusSpec::Builtin(id) => {
                crate::recipe::builtin_recipes()
                    .into_iter()
                    .find(|r| r.corpus.id == *id)
                    .ok_or_else(|| Error::Recipe(format!("Unknown builtin corpus: {id}")))
            }
            CorpusSpec::RecipePath(path) => Recipe::from_file(path),
        }
    }

    async fn acquire_source(
        &self,
        recipe: &Recipe,
        download_dir: &Path,
        progress: &Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        match &recipe.acquire {
            AcquirerConfig::BulkDownload { url, resume } => {
                let downloader = BulkDownloader::new(url, *resume);
                downloader
                    .download(download_dir, &recipe.corpus.id, progress)
                    .await
            }
            AcquirerConfig::LocalFile { path } => {
                let acq = LocalFileAcquirer::new(path);
                acq.acquire()
            }
            AcquirerConfig::HuggingFaceDataset { repo, subset } => {
                let acq = HuggingFaceDatasetAcquirer::new(repo, subset.as_deref());
                acq.download(download_dir, &recipe.corpus.id, progress).await
            }
            AcquirerConfig::WebCrawl { .. } => {
                Err(Error::Recipe("Web crawl acquirer not yet implemented".into()))
            }
            AcquirerConfig::ApiPaginated { .. } => {
                Err(Error::Recipe("API paginated acquirer not yet implemented".into()))
            }
        }
    }

    fn make_extractor(&self, config: &ExtractorConfig) -> Box<dyn Extractor> {
        match config {
            ExtractorConfig::MediawikiXml {
                namespace_filter,
                skip_redirects,
                decompress,
            } => Box::new(extractors::xml::MediawikiExtractor {
                namespace_filter: namespace_filter.clone(),
                skip_redirects: *skip_redirects,
                decompress: decompress.clone(),
            }),
            ExtractorConfig::StackExchangeXml { min_score } => {
                Box::new(extractors::xml::StackExchangeExtractor {
                    min_score: *min_score,
                })
            }
            ExtractorConfig::Jsonl {
                content_field,
                title_field,
                filter,
                decompress,
            } => Box::new(extractors::json::JsonlExtractor {
                content_field: content_field.clone(),
                title_field: title_field.clone(),
                filter: filter.clone(),
                decompress: decompress.clone(),
            }),
            ExtractorConfig::Html {
                content_selector,
                title_selector,
            } => Box::new(extractors::html::HtmlExtractor {
                content_selector: content_selector.clone(),
                title_selector: title_selector.clone(),
                label: String::new(),
            }),
            ExtractorConfig::Csv {
                content_column,
                title_column,
                delimiter,
            } => Box::new(extractors::csv::CsvExtractor {
                content_column: content_column.clone(),
                title_column: title_column.clone(),
                delimiter: delimiter.map(|c| c as u8),
            }),
            ExtractorConfig::Parquet {
                content_column,
                label_column,
                url_column,
            } => Box::new(extractors::parquet::ParquetExtractor {
                content_column: content_column.clone(),
                label_column: label_column.clone(),
                url_column: url_column.clone(),
            }),
            ExtractorConfig::Plaintext {
                title_pattern,
                strip_boilerplate,
            } => Box::new(extractors::plaintext::PlaintextExtractor {
                title_pattern: title_pattern.clone(),
                strip_boilerplate: strip_boilerplate.clone(),
            }),
        }
    }

    fn make_chunker(&self, config: &ChunkerConfig) -> Box<dyn Chunker> {
        match config {
            ChunkerConfig::Paragraph {
                max_chars,
                overlap_chars,
            } => Box::new(chunkers::paragraph::ParagraphChunker {
                max_chars: *max_chars,
                overlap_chars: *overlap_chars,
            }),
            ChunkerConfig::Sentence { max_chars } => {
                Box::new(chunkers::sentence::SentenceChunker {
                    max_chars: *max_chars,
                })
            }
            ChunkerConfig::Fixed {
                max_chars,
                overlap_chars,
            } => Box::new(chunkers::fixed::FixedChunker {
                max_chars: *max_chars,
                overlap_chars: *overlap_chars,
            }),
            ChunkerConfig::Semantic { max_chars } => {
                Box::new(chunkers::semantic::SemanticChunker {
                    max_chars: *max_chars,
                })
            }
        }
    }

    fn find_index_path(&self, corpus_id: &str) -> Result<PathBuf> {
        let path = self.index_dir.join(corpus_id);
        if path.exists() && path.is_dir() {
            return Ok(path);
        }
        Err(Error::IndexNotFound(format!(
            "No index found for corpus '{corpus_id}' in {}",
            self.index_dir.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mock_embed_fn() -> EmbedFn {
        Arc::new(|_text: &str| {
            Box::pin(async { Ok(vec![0.0_f32; 8]) })
        })
    }

    #[test]
    fn builtin_corpora_returns_entries() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            mock_embed_fn(),
        );
        let corpora = engine.builtin_corpora();
        assert!(!corpora.is_empty());
        assert!(corpora.iter().any(|c| c.id == "wikipedia"));
    }

    #[tokio::test]
    async fn installed_indexes_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            mock_embed_fn(),
        );
        let indexes = engine.installed_indexes().await.unwrap();
        assert!(indexes.is_empty());
    }

    #[tokio::test]
    async fn open_index_validates_embedding_model() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();

        let idx_path = idx_dir.join("test");
        CorpusIndex::create(
            &idx_path,
            "test",
            "Test",
            "different-model",
            8,
            true,
            "MIT",
        )
        .await
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        );

        let result = engine.open_index(&idx_path).await;
        assert!(matches!(result, Err(Error::IncompatibleEmbedding { .. })));
    }

    #[tokio::test]
    async fn remove_index_works() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();

        let idx_path = idx_dir.join("test");
        CorpusIndex::create(&idx_path, "test", "Test", "m", 8, true, "MIT")
            .await
            .unwrap();
        assert!(idx_path.exists());

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        );
        engine.remove_index("test").unwrap();
        assert!(!idx_path.exists());
    }
}
