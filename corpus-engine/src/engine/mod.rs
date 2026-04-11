//! CorpusEngine — orchestrates acquisition, extraction, chunking,
//! embedding, and indexing of corpus data.

mod ingest;

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::recipe::Recipe;
use crate::registry::RecipeRegistry;
use crate::types::{
    BatchEmbedFn, BuiltinCorpus, ChunkRange, EmbedFn, IndexInfo, IndexStats, ShardInfo,
};

/// Number of chunks to accumulate before calling the embed function.
/// Kept moderate so that progress reporting stays responsive.
const EMBED_BATCH_SIZE: usize = 256;

/// Number of embedded chunks to accumulate before flushing to
/// the LanceDB index. Larger values drastically reduce the number
/// of index fragments and thus compaction stalls. At 10K chunks and
/// ~32 chunks/s this means one insert every ~5 minutes, and LanceDB
/// compaction triggers much less often.
/// Memory: 10K chunks × 768 dims × 4 bytes ≈ 30MB — trivial.
const INDEX_FLUSH_SIZE: usize = 10_000;

pub struct CorpusEngine {
    registry: RecipeRegistry,
    recipes_dir: PathBuf,
    index_dir: PathBuf,
    embed: EmbedFn,
    /// Optional batch embedding function. When available, the ingest
    /// pipeline embeds chunks in batches for significantly higher throughput.
    /// Falls back to sequential `embed` calls when `None`.
    batch_embed: Option<BatchEmbedFn>,
    /// Optional primary inference function. Required for the enrichment phase.
    inference: Option<crate::types::InferenceFn>,
    /// Optional fast inference function (e.g. Qwen3-1.7B).
    /// Used for claim extraction; falls back to `inference` when `None`.
    fast_inference: Option<crate::types::InferenceFn>,
    expected_embedding_model: String,
}

impl CorpusEngine {
    pub fn new(
        recipes_dir: PathBuf,
        index_dir: PathBuf,
        embed: EmbedFn,
    ) -> Self {
        // Use recipes_dir as local overrides: checked before fetching URLs.
        // During development, local recipe.toml files in recipes/<id>/ are found here.
        // After install, cached recipe TOMLs are found here for delta updates.
        let registry = RecipeRegistry::from_bundled(Some(recipes_dir.clone()));
        Self {
            registry,
            recipes_dir,
            index_dir,
            embed,
            batch_embed: None,
            inference: None,
            fast_inference: None,
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

    /// Provide a fast inference function used exclusively for claim extraction.
    /// Falls back to the primary `inference` function if not set.
    pub fn with_fast_inference_fn(mut self, f: crate::types::InferenceFn) -> Self {
        self.fast_inference = Some(f);
        self
    }

    /// Provide a batch embedding function for high-throughput corpus ingest.
    /// When set, the ingest pipeline embeds chunks in batches rather than
    /// one-at-a-time, yielding 5-10x throughput improvement.
    pub fn with_batch_embed_fn(mut self, f: BatchEmbedFn) -> Self {
        self.batch_embed = Some(f);
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

    /// List built-in corpus definitions from the registry snapshot.
    ///
    /// Uses the bundled registry snapshot — no network required.
    /// Call `registry_mut().refresh().await` on startup to pick up
    /// any live updates from the public registry.
    pub fn builtin_corpora(&self) -> Vec<BuiltinCorpus> {
        self.registry.catalog()
    }

    /// Access the registry for catalog queries or background refresh.
    pub fn registry(&self) -> &RecipeRegistry {
        &self.registry
    }

    /// Mutable access to the registry — used to call `refresh()` on startup.
    pub fn registry_mut(&mut self) -> &mut RecipeRegistry {
        &mut self.registry
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
                tracing::debug!(
                    corpus = name,
                    "Skipping partial index — ingestion was not completed"
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

    /// Return the IDs of all installed corpora that have field model
    /// enrichment data. Used by epistemic tools to know which corpora
    /// to consult.
    pub async fn enriched_corpus_ids(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for info in self.installed_indexes().await? {
            if let Ok(index) = CorpusIndex::open(&info.path).await {
                if index.has_field_model_tables().await {
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
    ///
    /// Delegates to `RecipeRegistry::fetch_recipe()`:
    /// checks local overrides first, then fetches from the registry URL.
    pub async fn load_recipe(&self, corpus_id: &str) -> Result<Recipe> {
        self.registry.fetch_recipe(corpus_id).await
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
