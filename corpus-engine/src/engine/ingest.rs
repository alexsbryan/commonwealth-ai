//! Ingestion pipeline — acquire, extract, chunk, embed, index.

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
use crate::types::{CorpusSpec, IngestResult};

use super::{blake3_hex, normalize_content, CorpusEngine, EMBED_BATCH_SIZE};

impl CorpusEngine {
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
        let mut recipe = self.resolve_recipe(corpus).await?;

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
        // Auto-adapt: use the model's actual output dimensionality.
        // embedding_dimensions = 0 means auto-detect (the default when
        // the recipe omits the field). Only log if the recipe explicitly
        // specified a different value.
        if recipe.index.embedding_dimensions == 0 {
            recipe.index.embedding_dimensions = probe.len();
        } else if probe.len() != recipe.index.embedding_dimensions {
            tracing::info!(
                "Embedding model returns {} dimensions; recipe specified {}. \
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

        let use_batch_embed = self.batch_embed.is_some();
        if resume_iter_pos == 0 {
            if use_batch_embed {
                tracing::info!(
                    corpus = %recipe.corpus.id,
                    batch_size = EMBED_BATCH_SIZE,
                    "Starting embed+index pipeline (batch embedding enabled)"
                );
                eprintln!(
                    "[{}] Starting embed+index pipeline (batch embed, batch_size={})",
                    recipe.corpus.id, EMBED_BATCH_SIZE,
                );
            } else {
                tracing::info!(
                    corpus = %recipe.corpus.id,
                    "Starting embed+index pipeline (sequential embedding)"
                );
                eprintln!("[{}] Starting embed+index pipeline (sequential embed)", recipe.corpus.id);
            }
        }

        // Pending chunks awaiting embedding. When batch_embed is available,
        // we accumulate chunks and embed them all at once.
        let mut pending_chunks: Vec<InsertChunk> = Vec::new();
        let mut pending_texts: Vec<String> = Vec::new();

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

                let content_hash = blake3_hex(&content);
                pending_texts.push(content.clone());
                pending_chunks.push(InsertChunk {
                    content,
                    title: doc.title.clone(),
                    url: doc.url.clone(),
                    metadata: doc.metadata.as_ref().map(|m| m.to_string()),
                    content_hash: Some(content_hash),
                    source_doc_id: doc.url.clone()
                        .or_else(|| Some(doc.source_id.clone())),
                });

                if pending_chunks.len() >= EMBED_BATCH_SIZE {
                    // Embed the accumulated batch.
                    let embed_start = Instant::now();
                    let embed_count = pending_texts.len();
                    let embeddings = if let Some(ref batch_embed) = self.batch_embed {
                        (batch_embed)(&pending_texts).await?
                    } else {
                        let mut embs = Vec::with_capacity(pending_texts.len());
                        for text in &pending_texts {
                            embs.push((self.embed)(text).await?);
                        }
                        embs
                    };
                    let embed_ms = embed_start.elapsed().as_millis();

                    tracing::debug!(
                        chunks = embed_count,
                        embed_ms,
                        mode = if use_batch_embed { "batch" } else { "sequential" },
                        "Embed batch completed"
                    );

                    for (chunk, embedding) in pending_chunks.drain(..).zip(embeddings) {
                        batch.push((chunk, embedding));
                    }
                    pending_texts.clear();

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

        // Flush remaining pending chunks.
        if !pending_chunks.is_empty() {
            let embeddings = if let Some(ref batch_embed) = self.batch_embed {
                (batch_embed)(&pending_texts).await?
            } else {
                let mut embs = Vec::with_capacity(pending_texts.len());
                for text in &pending_texts {
                    embs.push((self.embed)(text).await?);
                }
                embs
            };
            for (chunk, embedding) in pending_chunks.drain(..).zip(embeddings) {
                batch.push((chunk, embedding));
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

        // Optional enrichment phase: field model enrichment.
        if let Some(enrichment_config) = recipe.enrichment.as_ref() {
            if enrichment_config.enabled {
                match self.inference.as_ref() {
                    Some(inference) => {
                        let field_engine =
                            crate::enrichment::field_engine::FieldModelEngine::from_recipe(
                                &recipe,
                                self.embed.clone(),
                                inference.clone(),
                            )?;
                        let id = recipe.corpus.id.clone();
                        let progress_fn = move |p: crate::enrichment::clustering::EnrichmentProgress| {
                            use crate::enrichment::clustering::EnrichmentProgress as EP;
                            match &p {
                                EP::Phase { phase, name, note } => {
                                    if note.is_empty() {
                                        eprintln!("[{id}] Phase {phase}: {name}");
                                    } else {
                                        eprintln!("[{id}] Phase {phase}: {name} ({note})");
                                    }
                                }
                                EP::PhaseSkipped { phase, name } =>
                                    eprintln!("[{id}] Phase {phase}: {name} — skipped (checkpoint)"),
                                EP::Resuming { from_phase } =>
                                    eprintln!("[{id}] Resuming enrichment from {from_phase}"),
                                EP::ClusteringStarted { total_chunks } =>
                                    eprintln!("[{id}] Clustering {total_chunks} chunks..."),
                                EP::ClusteringStep { step, detail } =>
                                    eprintln!("[{id}] ↳ {step}: {detail}"),
                                EP::ClusteringComplete { cluster_count, noise_chunks } =>
                                    eprintln!("[{id}] Clustering complete: {cluster_count} clusters, {noise_chunks} noise"),
                                EP::Phase1Progress { batches_done, batches_total } =>
                                    eprintln!("[{id}] Skeleton extraction: {batches_done}/{batches_total} batches"),
                                EP::Phase2bComplete { labeled_count } =>
                                    eprintln!("[{id}] Cluster labeling complete: {labeled_count} clusters labeled"),
                            }
                        };
                        field_engine.enrich(&index, &progress_fn).await?;
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

    // ── Private helpers ────────────────────────────────

    async fn resolve_recipe(&self, corpus: &CorpusSpec) -> Result<Recipe> {
        match corpus {
            CorpusSpec::Builtin(id) => self.registry.fetch_recipe(id).await,
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
}
