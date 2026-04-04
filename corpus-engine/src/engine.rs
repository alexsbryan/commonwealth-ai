//! CorpusEngine — orchestrates acquisition, extraction, chunking,
//! embedding, and indexing of corpus data.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::acquirers::bulk_download::BulkDownloader;
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
            expected_embedding_model: "nomic-embed-text-v2".to_string(),
        }
    }

    pub fn with_embedding_model(mut self, model: &str) -> Self {
        self.expected_embedding_model = model.to_string();
        self
    }

    pub fn index_dir(&self) -> &Path {
        &self.index_dir
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
    pub async fn ingest(
        &self,
        corpus: &CorpusSpec,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        let recipe = self.resolve_recipe(corpus)?;
        let start = Instant::now();

        // Ensure index directory exists.
        std::fs::create_dir_all(&self.index_dir)?;

        // Step 1: Acquire source data.
        let download_dir = self.index_dir.join("_downloads");
        let source_path = self
            .acquire_source(&recipe, &download_dir, &progress)
            .await?;

        // Step 2: Extract documents.
        let extractor = self.make_extractor(&recipe.extract);
        let doc_iter = extractor.extract(&source_path)?;

        // Step 3: Chunk and embed.
        let chunker = self.make_chunker(&recipe.chunk);
        let index_path = self
            .index_dir
            .join(format!("{}.db", recipe.corpus.id));

        let index = CorpusIndex::create(
            &index_path,
            &recipe.corpus.id,
            &recipe.corpus.name,
            &recipe.index.embedding_model,
            recipe.index.embedding_dimensions,
            recipe.corpus.mesh_sharing,
            &recipe.corpus.license,
        )?;

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
                // Build the content with title prefix if available.
                let content = if let Some(ref title) = doc.title {
                    if !tc.content.starts_with(title.as_str()) {
                        format!("{title}\n\n{}", tc.content)
                    } else {
                        tc.content
                    }
                } else {
                    tc.content
                };

                // Embed.
                let embedding = (self.embed)(&content).await?;

                batch.push((
                    InsertChunk {
                        content,
                        title: doc.title.clone(),
                        url: doc.url.clone(),
                        metadata: doc
                            .metadata
                            .as_ref()
                            .map(|m| m.to_string()),
                    },
                    embedding,
                ));

                if batch.len() >= EMBED_BATCH_SIZE {
                    total_chunks += batch.len() as u64;
                    index.insert_batch(&batch)?;
                    batch.clear();

                    if let Some(ref cb) = progress {
                        cb(IngestProgress::Embedding {
                            chunks_embedded: total_chunks,
                            total: 0, // unknown total
                        });
                    }
                }
            }
        }

        // Flush remaining.
        if !batch.is_empty() {
            total_chunks += batch.len() as u64;
            index.insert_batch(&batch)?;
        }

        let duration_secs = start.elapsed().as_secs();
        let index_size_bytes = std::fs::metadata(&index_path)
            .map(|m| m.len())
            .unwrap_or(0);

        if let Some(ref cb) = progress {
            cb(IngestProgress::Complete {
                total_chunks,
                duration_secs,
            });
        }

        Ok(IngestResult {
            corpus_id: recipe.corpus.id,
            chunks_created: total_chunks,
            index_size_bytes,
            duration_secs,
        })
    }

    // ── Index Management ────────────────────────────────

    /// List all index files present in the index directory.
    pub fn installed_indexes(&self) -> Result<Vec<IndexInfo>> {
        let mut indexes = Vec::new();
        if !self.index_dir.is_dir() {
            return Ok(indexes);
        }

        for entry in std::fs::read_dir(&self.index_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("db") {
                // Skip internal directories.
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                if name.starts_with('_') {
                    continue;
                }
                match CorpusIndex::open(&path) {
                    Ok(idx) => match idx.info() {
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
        }
        Ok(indexes)
    }

    /// Open an index file (complete or shard) for search.
    /// Validates that the embedding model matches.
    pub fn open_index(&self, path: &Path) -> Result<CorpusIndex> {
        let index = CorpusIndex::open(path)?;
        let info = index.info()?;

        if info.embedding_model != self.expected_embedding_model {
            return Err(Error::IncompatibleEmbedding {
                index_model: info.embedding_model,
                expected_model: self.expected_embedding_model.clone(),
                path: path.to_owned(),
            });
        }

        Ok(index)
    }

    /// Remove an index file.
    pub fn remove_index(&self, corpus_id: &str) -> Result<()> {
        let path = self.index_dir.join(format!("{corpus_id}.db"));
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        // Also remove any WAL/SHM files.
        let wal = self.index_dir.join(format!("{corpus_id}.db-wal"));
        let shm = self.index_dir.join(format!("{corpus_id}.db-shm"));
        let _ = std::fs::remove_file(wal);
        let _ = std::fs::remove_file(shm);
        Ok(())
    }

    // ── Shard Operations ────────────────────────────────

    /// Report chunk ID range, count, and size for an index.
    pub fn index_stats(&self, corpus_id: &str) -> Result<IndexStats> {
        let path = self.find_index_path(corpus_id)?;
        crate::sharding::index_stats(&path)
    }

    /// Extract a subset of an existing index into a new file.
    pub fn extract_shard(
        &self,
        source_corpus_id: &str,
        chunk_range: ChunkRange,
        output_path: &Path,
    ) -> Result<ShardInfo> {
        let source_path = self.find_index_path(source_corpus_id)?;
        crate::sharding::extract_shard(&source_path, chunk_range, output_path)
    }

    /// Merge multiple shard files into a single index.
    pub fn merge_shards(
        &self,
        shard_paths: &[PathBuf],
        output_path: &Path,
    ) -> Result<IndexInfo> {
        crate::sharding::merge_shards(shard_paths, output_path)
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
            } => Box::new(extractors::parquet::ParquetExtractor {
                content_column: content_column.clone(),
                label_column: label_column.clone(),
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
        let path = self.index_dir.join(format!("{corpus_id}.db"));
        if path.exists() {
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

    #[test]
    fn installed_indexes_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            mock_embed_fn(),
        );
        let indexes = engine.installed_indexes().unwrap();
        assert!(indexes.is_empty());
    }

    #[test]
    fn open_index_validates_embedding_model() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();

        // Create an index with a different embedding model.
        let idx_path = idx_dir.join("test.db");
        CorpusIndex::create(
            &idx_path,
            "test",
            "Test",
            "different-model",
            8,
            true,
            "MIT",
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        );

        let result = engine.open_index(&idx_path);
        assert!(matches!(result, Err(Error::IncompatibleEmbedding { .. })));
    }

    #[test]
    fn remove_index_works() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();

        let idx_path = idx_dir.join("test.db");
        CorpusIndex::create(&idx_path, "test", "Test", "m", 8, true, "MIT").unwrap();
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
