//! CorpusEngine — orchestrates acquisition, extraction, chunking,
//! embedding, and indexing of corpus data.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::acquirers::bulk_download::BulkDownloader;
use crate::acquirers::huggingface::HuggingFaceDatasetAcquirer;
use crate::acquirers::local_file::LocalFileAcquirer;
use crate::chunkers::{self, Chunker};
use crate::enrichment::article_profile::compute_article_profiles;
use crate::enrichment::link_graph::LinkGraphBuilder;
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

    /// Return a clone of the embedding function.
    /// Used by `CorpusIndexChecker` to re-embed corrupt chunks.
    pub fn embed_fn(&self) -> crate::types::EmbedFn {
        self.embed.clone()
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
                if index_path.exists() {
                    if CorpusIndex::has_committed_data(&index_path) {
                        // Committed chunks exist — preserve the partial index so
                        // the user can resume without re-embedding everything.
                        tracing::info!(
                            "Corpus '{}' install failed ({e}), but committed chunks exist — preserving for resume",
                            recipe.corpus.id,
                        );
                        eprintln!(
                            "[{}] Install failed ({e}). Committed chunks are preserved — re-install to resume.",
                            recipe.corpus.id,
                        );
                    } else {
                        // No chunks committed — fresh install failed early. Safe to wipe.
                        if let Err(rm) = std::fs::remove_dir_all(&index_path) {
                            tracing::warn!(
                                "Failed to clean up partial index at {}: {rm}",
                                index_path.display()
                            );
                        }
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

        // Open or resume a partial index (supports resuming after process kill).
        let (index, resume_iter_pos) = CorpusIndex::create_or_resume(
            index_path,
            &recipe.corpus.id,
            &recipe.corpus.name,
            // Use the engine's actual embedding model name (derived from the
            // configured file path), not the recipe's hardcoded default string.
            &self.expected_embedding_model,
            recipe.index.embedding_dimensions,
            recipe.corpus.mesh_sharing,
            &recipe.corpus.license,
        )
        .await?;

        // Initialise counters. On resume these start from where we left off.
        let mut total_chunks = index.chunk_count().await.unwrap_or(0);
        let mut docs_processed = 0u64; // successful docs in THIS run
        let mut iter_pos = 0u64;       // absolute position in the source iterator
        let mut batch: Vec<(InsertChunk, Vec<f32>)> = Vec::new();
        let mut batch_start = Instant::now();

        if resume_iter_pos == 0 {
            eprintln!("[{}] Starting embed+index pipeline", recipe.corpus.id);
        }

        for doc_result in doc_iter {
            iter_pos += 1;

            // Skip documents that were already committed in a previous run.
            if iter_pos <= resume_iter_pos {
                continue;
            }

            let doc = match doc_result {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Skipping document: {e}");
                    continue;
                }
            };

            docs_processed += 1;

            let cleaned_content = normalize_content(&doc.content);
            let text_chunks = chunker.chunk(&cleaned_content);

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

                let content_hash = blake3_hex(&content);
                batch.push((
                    InsertChunk {
                        content,
                        title: doc.title.clone(),
                        url: doc.url.clone(),
                        metadata: doc.metadata.as_ref().map(|m| m.to_string()),
                        content_hash: Some(content_hash),
                        source_doc_id: doc.url.clone()
                            .or_else(|| Some(doc.source_id.clone())),
                    },
                    embedding,
                ));

                if batch.len() >= EMBED_BATCH_SIZE {
                    let batch_secs = batch_start.elapsed().as_secs_f32().max(0.001);
                    let chunks_per_sec = batch.len() as f32 / batch_secs;
                    total_chunks += batch.len() as u64;
                    index.insert_batch(&batch).await?;
                    // Checkpoint: persist how far we've iterated so a restart can resume.
                    let _ = index.update_committed_iter_pos(iter_pos);
                    batch.clear();

                    let elapsed = start.elapsed();
                    eprintln!(
                        "[{}] {total_chunks} chunks | {} docs | {chunks_per_sec:.1} chunks/s | {}m{}s elapsed",
                        recipe.corpus.id,
                        resume_iter_pos + docs_processed,
                        elapsed.as_secs() / 60,
                        elapsed.as_secs() % 60,
                    );

                    if let Some(ref cb) = progress {
                        cb(IngestProgress::Embedding {
                            chunks_embedded: total_chunks,
                            total: 0,
                            docs_processed: resume_iter_pos + docs_processed,
                            chunks_per_sec,
                        });
                    }

                    batch_start = Instant::now();
                }
            }
        }

        // Flush remaining.
        if !batch.is_empty() {
            total_chunks += batch.len() as u64;
            index.insert_batch(&batch).await?;
            let _ = index.update_committed_iter_pos(iter_pos);
            eprintln!(
                "[{}] Flushed final batch — {total_chunks} chunks total from {} docs",
                recipe.corpus.id,
                resume_iter_pos + docs_processed,
            );
        }

        // A pipeline that produced zero chunks is almost always a bug
        // (wrong column name, empty parquet, all docs filtered out).
        // On resume: if we skipped everything (all docs were committed), total_chunks
        // is from the existing table and we proceed to build indexes normally.
        if total_chunks == 0 {
            return Err(Error::Extraction(format!(
                "Ingest produced zero chunks for corpus '{}'. \
                 The source may be empty, the extractor may be \
                 misconfigured, or every document may have been filtered.",
                recipe.corpus.id,
            )));
        }

        // Build search indexes (IVF-PQ + FTS).
        // Skip if already completed in a previous run — this is the common case
        // when a process was killed after build_indexes() but before
        // mark_ingestion_complete(). We detect it via the `indexes_built` flag
        // so we don't waste minutes rebuilding what's already there.
        if CorpusIndex::indexes_are_built(index_path) {
            eprintln!(
                "[{}] Search indexes already built — skipping to completion",
                recipe.corpus.id,
            );
        } else {
            let build_vector = recipe.index.vector;
            let build_fts = recipe.index.fts;
            let dims = recipe.index.embedding_dimensions;
            // Estimate IVF-PQ partition count: LanceDB Auto ≈ sqrt(N), capped 2–512.
            let est_partitions = (total_chunks as f64).sqrt().round() as u64;
            let est_partitions = est_partitions.max(2).min(512);
            eprintln!(
                "[{id}] Index build starting — model: {model} ({dims}d), \
                 chunks: {total_chunks}, \
                 vector: {build_vector} (IVF-PQ auto ≈ {est_partitions} partitions), \
                 fts: {build_fts}",
                id = recipe.corpus.id,
                model = self.expected_embedding_model,
            );
            if let Some(ref cb) = progress {
                cb(IngestProgress::Indexing {
                    chunks_indexed: 0,
                    total: total_chunks,
                });
            }
            let sub_phase_cb: Option<Box<dyn Fn(u64, u64) + Send + Sync>> =
                progress.as_ref().map(|cb| -> Box<dyn Fn(u64, u64) + Send + Sync> {
                    Box::new(move |done, total_phases| {
                        cb(IngestProgress::Indexing {
                            chunks_indexed: total_chunks * done / total_phases,
                            total: total_chunks,
                        });
                    })
                });
            index.build_indexes(build_vector, build_fts, sub_phase_cb.as_deref()).await?;
            // Checkpoint: if killed after this point, resume can skip rebuild.
            let _ = index.mark_indexes_built();
        }

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

        // Structural enrichment phase: link graph + article profiles.
        // Runs when the recipe uses a WikipediaStructured extractor with
        // structural_signals = true. No LLM required.
        if structural_signals_enabled(&recipe.extract) {
            let controversy_patterns =
                controversy_patterns_from_config(&recipe.extract);

            if let Some(ref cb) = progress {
                cb(IngestProgress::BuildingLinkGraph {
                    current: 0,
                    total: 0,
                });
            }

            let builder = LinkGraphBuilder {
                controversy_section_types: vec!["controversy".to_string()],
            };
            let link_rels = builder.build(&index, &progress).await?;

            if !link_rels.is_empty() {
                index.store_relationships(&link_rels).await?;
            }

            let profiles = compute_article_profiles(&index, &link_rels).await?;

            if let Some(ref cb) = progress {
                cb(IngestProgress::ComputingArticleProfiles {
                    article_count: profiles.len(),
                });
            }

            if !profiles.is_empty() {
                index.store_article_profiles(&profiles).await?;
            }
        }

        let duration_secs = start.elapsed().as_secs();
        let info = index.info().await?;

        // Mark the index as fully committed so it survives a restart as "Indexed"
        // rather than being treated as a partial/incomplete ingest.
        if let Err(e) = index.mark_ingestion_complete() {
            tracing::warn!("Failed to mark ingestion complete for '{}': {e}", recipe.corpus.id);
        }

        eprintln!(
            "[{}] Ingestion complete — {total_chunks} chunks in {}m{}s",
            recipe.corpus.id,
            duration_secs / 60,
            duration_secs % 60,
        );

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
            // Skip indexes where ingestion was interrupted (process killed mid-embed).
            if !CorpusIndex::is_ingestion_complete(&path) {
                eprintln!(
                    "[corpus-engine] Skipping partial index at '{}' — ingestion was not completed. \
                     Re-install the corpus to build a complete index.",
                    name
                );
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

        // Warn on mismatch rather than hard-erroring so that indexes written
        // before the model name was recorded correctly (they all stored the
        // placeholder "nomic-embed-text-v2") remain searchable after the fix.
        // A true incompatibility (different dimensionality) will surface as a
        // search error anyway; the string check is informational only.
        if info.embedding_model != self.expected_embedding_model {
            tracing::warn!(
                "Corpus '{}' was indexed with model '{}' but current engine expects '{}'. \
                 Search results may be degraded if the models differ. Re-install the corpus to fix.",
                info.corpus_id,
                info.embedding_model,
                self.expected_embedding_model,
            );
        }

        Ok(index)
    }

    /// Validate that all installed indexes were built with the same embedding
    /// dimensions as the currently loaded model produces.
    ///
    /// Call this at startup after performing a probe embed to detect
    /// switched embed models early — before a search call surfaces an
    /// opaque LanceDB schema error at query time.
    ///
    /// Returns an error naming the first mismatched corpus. The caller
    /// should surface this as a plain-language warning (not a hard abort)
    /// so that existing users who switch embed models are not blocked.
    pub async fn validate_embed_dimensions(&self, loaded_dims: usize) -> Result<()> {
        for info in self.installed_indexes().await? {
            if info.embedding_dimensions != 0 && info.embedding_dimensions != loaded_dims {
                return Err(crate::Error::Database(format!(
                    "Corpus '{}' was built with {} embedding dimensions but the \
                     loaded model produces {}. To fix: rebuild the corpus in \
                     Settings → Knowledge → Rebuild.",
                    info.corpus_id, info.embedding_dimensions, loaded_dims,
                )));
            }
        }
        Ok(())
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

    /// Retry claim extraction on previously-failed chunks using the
    /// truncation-repair parser, without re-running inference.
    ///
    /// Loads `_enrichment_failures.ndjson`, recovers what it can, embeds the
    /// new claims, stores them, and rewrites the failures file with only the
    /// still-unresolved records.
    ///
    /// Returns the number of newly recovered claims (0 if nothing to retry).
    pub async fn retry_enrichment_failures(&self, corpus_id: &str) -> Result<usize> {
        let index = self.open_index_for_corpus(corpus_id).await?;

        // retry_parse_failures only calls self.embed, never self.inference.
        // Supply a dummy InferenceFn so EnrichmentEngine can be constructed.
        let dummy: crate::types::InferenceFn = std::sync::Arc::new(|_| {
            Box::pin(async {
                Err(crate::error::Error::Embed(
                    "inference not available in retry mode".into(),
                ))
            })
        });
        let enricher =
            crate::enrichment::EnrichmentEngine::new(self.embed_fn(), dummy);

        let new_claims = enricher.retry_parse_failures(&index).await?;
        let recovered = new_claims.len();

        if recovered > 0 {
            index.store_claims(&new_claims).await?;
            index.build_claims_index().await?;
        }

        Ok(recovered)
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

    // ── CorpusUpdater / health helpers ──────────────────

    /// Find and parse the recipe for `corpus_id`.
    /// Checks builtin recipes first, then scans the recipes directory.
    pub fn load_recipe(&self, corpus_id: &str) -> Result<Recipe> {
        // Check builtins first.
        if let Some(r) = crate::recipe::builtin_recipes()
            .into_iter()
            .find(|r| r.corpus.id == corpus_id)
        {
            return Ok(r);
        }
        // Scan recipes directory for a .toml whose corpus.id matches.
        let dir = &self.recipes_dir;
        if dir.exists() {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    if let Ok(recipe) = Recipe::from_file(&path) {
                        if recipe.corpus.id == corpus_id {
                            return Ok(recipe);
                        }
                    }
                }
            }
        }
        Err(Error::Recipe(format!("No recipe found for corpus_id: {corpus_id}")))
    }

    /// Chunk a document's text content using the recipe's chunker config.
    pub fn chunk_document(&self, recipe: &Recipe, content: &str) -> Result<Vec<crate::index::InsertChunk>> {
        let chunker = self.make_chunker(&recipe.chunk);
        let chunks: Vec<_> = chunker
            .chunk(content)
            .into_iter()
            .map(|tc| {
                let hash = blake3_hex(&tc.content);
                crate::index::InsertChunk {
                    content: tc.content,
                    title: None,
                    url: None,
                    metadata: None,
                    content_hash: Some(hash),
                    source_doc_id: None,
                }
            })
            .collect();
        Ok(chunks)
    }

    /// Embed a batch of `InsertChunk` objects, returning `EmbeddedChunk` pairs.
    pub async fn embed_chunks(
        &self,
        chunks: &[crate::index::InsertChunk],
    ) -> Result<Vec<crate::index::EmbeddedChunk>> {
        let mut out = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let embedding = (self.embed)(&chunk.content).await?;
            out.push(crate::index::EmbeddedChunk {
                insert: chunk.clone(),
                embedding,
            });
        }
        Ok(out)
    }

    /// Persist the `_update_progress.json` sidecar for a corpus.
    pub fn save_update_progress(
        &self,
        corpus_id: &str,
        log: &crate::update::delta::UpdateProgressLog,
    ) -> Result<()> {
        let path = self.index_dir.join(corpus_id).join("_update_progress.json");
        let json = serde_json::to_string_pretty(log)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load the `_update_progress.json` sidecar for a corpus.
    pub fn load_update_progress(
        &self,
        corpus_id: &str,
    ) -> Result<crate::update::delta::UpdateProgressLog> {
        let path = self.index_dir.join(corpus_id).join("_update_progress.json");
        if !path.exists() {
            return Ok(Default::default());
        }
        let json = std::fs::read_to_string(&path)?;
        serde_json::from_str(&json).map_err(|e| Error::Serialization(e.to_string()))
    }

    /// Delete the `_update_progress.json` sidecar for a corpus.
    pub fn clear_update_progress(&self, corpus_id: &str) -> Result<()> {
        let path = self.index_dir.join(corpus_id).join("_update_progress.json");
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Load the stored `VersionManifest` for a corpus.
    pub fn load_stored_manifest(
        &self,
        corpus_id: &str,
    ) -> Result<crate::update::delta::VersionManifest> {
        let path = self.index_dir.join(corpus_id).join("_version_manifest.json");
        if !path.exists() {
            return Err(Error::IndexNotFound(format!("No manifest for {corpus_id}")));
        }
        let json = std::fs::read_to_string(&path)?;
        serde_json::from_str(&json).map_err(|e| Error::Serialization(e.to_string()))
    }

    /// Save the `VersionManifest` for a corpus.
    pub fn save_stored_manifest(
        &self,
        corpus_id: &str,
        manifest: &crate::update::delta::VersionManifest,
    ) -> Result<()> {
        let path = self.index_dir.join(corpus_id).join("_version_manifest.json");
        let json = serde_json::to_string_pretty(manifest)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        std::fs::write(&path, json)?;
        Ok(())
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

    pub(crate) async fn acquire_source(
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

    pub(crate) fn make_extractor(&self, config: &ExtractorConfig) -> Box<dyn Extractor> {
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
            ExtractorConfig::WikipediaStructured {
                title_column,
                url_column,
                controversy_patterns,
                factual_patterns,
                ..
            } => Box::new(
                extractors::wikipedia_structured::WikipediaStructuredExtractor {
                    title_column: title_column.clone(),
                    url_column: url_column.clone(),
                    controversy_patterns: controversy_patterns.clone(),
                    factual_patterns: factual_patterns.clone(),
                },
            ),
            ExtractorConfig::WikipediaJsonl {
                controversy_patterns,
                factual_patterns,
            } => Box::new(
                extractors::wikipedia_jsonl::WikipediaJsonlExtractor {
                    controversy_patterns: controversy_patterns.clone(),
                    factual_patterns: factual_patterns.clone(),
                },
            ),
        }
    }

    pub(crate) fn make_chunker(&self, config: &ChunkerConfig) -> Box<dyn Chunker> {
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

    /// Run the recipe test harness against a recipe file.
    ///
    /// Downloads a small sample, runs extract → chunk → (optionally embed+search),
    /// and returns a structured report. The report can be serialized to Markdown
    /// via `TestReport::to_markdown()` for inclusion in a PR.
    pub async fn test_recipe(
        &self,
        recipe_path: &std::path::Path,
        options: &crate::testing::TestOptions,
    ) -> Result<crate::testing::TestReport> {
        crate::testing::run_test(self, recipe_path, options).await
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

/// Returns true if the extractor config requests structural signal extraction
/// (i.e., is a WikipediaStructured extractor with structural_signals = true).
fn structural_signals_enabled(config: &ExtractorConfig) -> bool {
    matches!(
        config,
        ExtractorConfig::WikipediaStructured {
            structural_signals: true,
            ..
        }
    )
}

/// Extract the controversy patterns from a WikipediaStructured extractor config.
/// Returns an empty vec for all other extractor types.
fn controversy_patterns_from_config(config: &ExtractorConfig) -> Vec<String> {
    match config {
        ExtractorConfig::WikipediaStructured {
            controversy_patterns,
            ..
        } => controversy_patterns.clone(),
        _ => vec![],
    }
}

/// Compute the BLAKE3 hex digest of a string.
pub(crate) fn blake3_hex(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
}

/// Strip model-generated artifacts from raw corpus text before chunking.
/// Some HuggingFace datasets contain LLM-generated content with `<think>`
/// blocks; storing those verbatim pollutes every chunk and breaks enrichment.
pub(crate) fn normalize_content(s: &str) -> String {
    if !s.contains("<think>") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(rel_end) => {
                rest = &rest[start + rel_end + "</think>".len()..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
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
    async fn open_index_allows_model_mismatch_with_warning() {
        // Model-name mismatches are downgraded to warnings so that indexes
        // written before the placeholder "nomic-embed-text-v2" was fixed
        // remain searchable.  A true dimensionality mismatch surfaces as a
        // search error, so blocking open() adds no safety.
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

        // Should succeed (warn, not error) when model names differ.
        assert!(engine.open_index(&idx_path).await.is_ok());
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
