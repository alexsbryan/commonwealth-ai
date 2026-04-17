//! CorpusEngine — orchestrates acquisition, extraction, chunking,
//! embedding, and indexing of corpus data.

mod ingest;

#[cfg(feature = "treesitter")]
pub mod reindex;

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::progress::{
    ManifestReconstructionReport, ReconstructionMethod, SourceFileManifest, SourceFileRecord,
    SourceFileStatus,
};
use crate::recipe::Recipe;
use crate::registry::RecipeRegistry;
use crate::types::{
    BatchEmbedFn, BuiltinCorpus, ChunkRange, EmbedFn, IndexInfo, IndexStats, ShardInfo,
};

/// Number of chunks to accumulate before calling the embed function.
/// Kept moderate so that progress reporting stays responsive.
const EMBED_BATCH_SIZE: usize = 256;

/// Number of embedded chunks to accumulate before flushing to
/// the LanceDB index and writing a crash-recovery checkpoint.
///
/// Tradeoff: larger values → fewer LanceDB fragments (less compaction
/// pressure) but more work lost if the embed process crashes mid-batch.
/// The llama.cpp Metal backend is known to assert-abort under prolonged
/// GPU memory pressure (ggml_metal_buffer_set_tensor: buf_src = NULL).
/// On a multi-day ingestion run, crashes will happen. At 2K chunks and
/// ~32 chunks/s, a checkpoint is written every ~60s so at most ~60s of
/// embedding work is lost per crash. LanceDB compaction handles the
/// increased fragment count gracefully.
const INDEX_FLUSH_SIZE: usize = 2_000;

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

    /// Read the `committed_iter_pos` from a corpus's `_corpus_meta.json`.
    /// Returns 0 when the meta file is absent (corpus not yet started).
    pub fn corpus_committed_iter_pos(&self, corpus_id: &str) -> u64 {
        let path = self.index_dir.join(corpus_id).join("_corpus_meta.json");
        let Ok(content) = std::fs::read_to_string(&path) else { return 0 };
        serde_json::from_str::<serde_json::Value>(&content)
            .ok()
            .and_then(|v| v["committed_iter_pos"].as_u64())
            .unwrap_or(0)
    }

    /// Count the total number of articles (non-empty lines) in a Wikipedia
    /// JSONL corpus cache at `_downloads/{corpus_id}.extracted.jsonl`.
    ///
    /// Uses a fast byte-scan rather than line-by-line iteration so it's
    /// efficient even on 20GB+ JSONL files (~1-2 min on cold I/O).
    pub fn count_jsonl_articles(&self, corpus_id: &str) -> Result<u64> {
        let jsonl_path = self
            .index_dir
            .join("_downloads")
            .join(format!("{corpus_id}.extracted.jsonl"));
        if !jsonl_path.exists() {
            return Err(Error::Recipe(format!(
                "JSONL cache not found for corpus '{corpus_id}': \
                 expected {path}. \
                 Start or resume ingestion once so the ZIP is extracted.",
                path = jsonl_path.display()
            )));
        }
        let file = std::fs::File::open(&jsonl_path)?;
        let mut reader = std::io::BufReader::with_capacity(256 * 1024, file);
        let mut count = 0u64;
        let mut buf = Vec::with_capacity(256 * 1024);
        loop {
            buf.clear();
            let n = std::io::BufRead::read_until(&mut reader, b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            let trimmed = buf.trim_ascii();
            if !trimmed.is_empty() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Estimate how many articles Machine A has already processed for a
    /// Wikipedia JSONL corpus, given the stored `committed_iter_pos` (which
    /// counts sections, not articles).
    ///
    /// Samples the first `sample_size` articles from the JSONL to derive the
    /// mean sections-per-article, then extrapolates. Returns `None` when the
    /// JSONL cache is absent (ingestion not yet started on this node).
    pub fn estimate_article_pos(
        &self,
        corpus_id: &str,
        committed_iter_pos: u64,
        sample_size: usize,
    ) -> Result<Option<u64>> {
        let jsonl_path = self
            .index_dir
            .join("_downloads")
            .join(format!("{corpus_id}.extracted.jsonl"));
        if !jsonl_path.exists() {
            return Ok(None);
        }
        if committed_iter_pos == 0 {
            return Ok(Some(0));
        }

        // Sample the first `sample_size` articles: parse each line minimally
        // to count top-level sections, derive mean sections-per-article.
        let file = std::fs::File::open(&jsonl_path)?;
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
        let mut article_count = 0usize;
        let mut total_sections = 0usize;
        let mut line = String::new();
        while article_count < sample_size {
            line.clear();
            let n = std::io::BufRead::read_line(&mut reader, &mut line)?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            article_count += 1;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                let section_count = v["sections"]
                    .as_array()
                    .map(|s| s.len())
                    .unwrap_or(1)
                    .max(1);
                total_sections += section_count;
            } else {
                total_sections += 5; // fallback estimate
            }
        }

        if article_count == 0 || total_sections == 0 {
            return Ok(Some(committed_iter_pos / 5)); // coarse fallback
        }
        let mean_sections = total_sections as f64 / article_count as f64;
        let estimated = (committed_iter_pos as f64 / mean_sections) as u64;
        Ok(Some(estimated))
    }

    /// Return corpus IDs where ingestion has started but not finished
    /// (`ingestion_in_progress: true` AND `committed_iter_pos > 0`).
    /// Used by the auto-collaborate loop. Callers should cross-check
    /// against `AppStateInner::active_ingests` to skip corpora with a
    /// live ingest task already running.
    pub fn in_progress_ingestions(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.index_dir) else { return vec![] };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let meta_path = e.path().join("_corpus_meta.json");
                let content = std::fs::read_to_string(&meta_path).ok()?;
                let v: serde_json::Value = serde_json::from_str(&content).ok()?;
                let in_progress = v["ingestion_in_progress"].as_bool() == Some(true);
                let has_started = v["committed_iter_pos"].as_u64().unwrap_or(0) > 0;
                if in_progress && has_started {
                    e.file_name().into_string().ok()
                } else {
                    None
                }
            })
            .collect()
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

    /// Dump diagnostic information about all installed indexes.
    /// Checks both the metadata file and the actual LanceDB state.
    pub async fn diagnose_indexes(&self) -> String {
        let mut report = String::new();
        report.push_str(&format!("Index directory: {}\n", self.index_dir.display()));

        if !self.index_dir.is_dir() {
            report.push_str("  Directory does not exist.\n");
            return report;
        }

        let entries = match std::fs::read_dir(&self.index_dir) {
            Ok(e) => e,
            Err(e) => {
                report.push_str(&format!("  Cannot read directory: {e}\n"));
                return report;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            if name.starts_with('_') {
                continue;
            }

            report.push_str(&format!("\n--- {} ---\n", name));

            let meta_path = path.join("_corpus_meta.json");
            if !meta_path.exists() {
                report.push_str("  No _corpus_meta.json — not an index.\n");
                continue;
            }

            // Read meta file raw.
            if let Ok(raw) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
                    report.push_str(&format!(
                        "  ingestion_in_progress: {}\n\
                         indexes_built: {}\n\
                         vector_index_built: {}\n\
                         content_fts_built: {}\n\
                         title_fts_built: {}\n\
                         committed_iter_pos: {}\n\
                         embedding_model: {}\n\
                         embedding_dimensions: {}\n",
                        meta.get("ingestion_in_progress").unwrap_or(&serde_json::Value::Null),
                        meta.get("indexes_built").unwrap_or(&serde_json::Value::Null),
                        meta.get("vector_index_built").unwrap_or(&serde_json::Value::Null),
                        meta.get("content_fts_built").unwrap_or(&serde_json::Value::Null),
                        meta.get("title_fts_built").unwrap_or(&serde_json::Value::Null),
                        meta.get("committed_iter_pos").unwrap_or(&serde_json::Value::Null),
                        meta.get("embedding_model").unwrap_or(&serde_json::Value::Null),
                        meta.get("embedding_dimensions").unwrap_or(&serde_json::Value::Null),
                    ));
                } else {
                    report.push_str("  Meta file exists but failed to parse.\n");
                }
            }

            // Check ingestion complete.
            let complete = CorpusIndex::is_ingestion_complete(&path);
            report.push_str(&format!("  is_ingestion_complete: {complete}\n"));

            if !complete {
                report.push_str("  *** WOULD BE SKIPPED by installed_indexes() ***\n");
            }

            // Open and diagnose the actual LanceDB state.
            match CorpusIndex::open(&path).await {
                Ok(idx) => {
                    report.push_str(&idx.diagnose().await);
                    report.push('\n');
                }
                Err(e) => {
                    report.push_str(&format!("  Failed to open index: {e}\n"));
                }
            }
        }

        report
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

    // ── Source-file manifest ────────────────────────────

    /// Load the source-file manifest for a corpus, if one exists.
    ///
    /// Returns `None` when the manifest has not yet been written (e.g. for
    /// corpora that predate T1, or for single-file sources like JSONL).
    pub fn source_manifest(&self, corpus_id: &str) -> Result<Option<SourceFileManifest>> {
        let index_path = self.index_dir.join(corpus_id);
        SourceFileManifest::load(&index_path)
    }

    /// Return source files that are not yet fully committed for a corpus.
    ///
    /// Returns `Pending` and `InProgress` records sorted by `file_index`.
    /// An `InProgress` record on crash recovery conservatively means the file
    /// must be re-processed (it may have partially committed chunks).
    ///
    /// Returns `Err` if no manifest exists — callers should run
    /// `reconstruct_source_manifest()` first for pre-T1 indexes.
    pub fn remaining_source_files(&self, corpus_id: &str) -> Result<Vec<SourceFileRecord>> {
        let manifest = self.source_manifest(corpus_id)?.ok_or_else(|| {
            Error::Recipe(format!(
                "No source manifest for corpus '{corpus_id}'. \
                 Run `sovereign corpus reconstruct-manifest {corpus_id}` first."
            ))
        })?;
        let mut remaining: Vec<SourceFileRecord> = manifest
            .files
            .into_iter()
            .filter(|r| !matches!(r.status, crate::progress::SourceFileStatus::Complete { .. }))
            .collect();
        remaining.sort_by_key(|r| r.file_index);
        Ok(remaining)
    }

    /// Reconstruct a [`SourceFileManifest`] for a pre-T1 index that was
    /// ingested before per-file progress tracking was added.
    ///
    /// Strategy: read `committed_iter_pos` from `_corpus_meta.json`, then
    /// read per-file row counts from parquet metadata in `source_dir`
    /// (typically `~/.sovereign/indexes/_downloads/{corpus_id}/`).
    /// A binary search over the cumulative row sums determines which files
    /// are Complete vs InProgress vs Pending.
    ///
    /// If `source_dir` is `None`, defaults to `index_dir/_downloads/{corpus_id}/`.
    /// If no parquet files are found there, falls back to a single-file
    /// (`SingleFile`) report with one InProgress entry.
    ///
    /// The resulting manifest is written to `_source_manifest.json` alongside
    /// `_corpus_meta.json`.
    pub fn reconstruct_source_manifest(
        &self,
        corpus_id: &str,
        source_dir: Option<&Path>,
    ) -> Result<ManifestReconstructionReport> {
        let index_path = self.index_dir.join(corpus_id);
        let meta_path = index_path.join("_corpus_meta.json");
        if !meta_path.exists() {
            return Err(Error::Recipe(format!(
                "No index found for corpus '{corpus_id}' at {}",
                index_path.display()
            )));
        }

        // Read committed_iter_pos from _corpus_meta.json.
        let meta_raw = std::fs::read_to_string(&meta_path)?;
        let meta: serde_json::Value = serde_json::from_str(&meta_raw)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let committed_iter_pos = meta
            .get("committed_iter_pos")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // Find parquet files in the source dir.
        let default_download = self.index_dir.join("_downloads").join(corpus_id);
        let search_dir = source_dir.unwrap_or(&default_download);

        let mut parquet_paths: Vec<PathBuf> = if search_dir.is_dir() {
            std::fs::read_dir(search_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
                .collect()
        } else {
            Vec::new()
        };
        parquet_paths.sort();

        let mut warnings = Vec::new();

        if parquet_paths.is_empty() {
            // No parquet files — single-file source (e.g. JSONL).
            let record = SourceFileRecord {
                file_index: 0,
                filename: index_path
                    .join("_downloads")
                    .join(corpus_id)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("source")
                    .to_string(),
                size_bytes: 0,
                status: if committed_iter_pos > 0 {
                    SourceFileStatus::InProgress { started_at: Utc::now() }
                } else {
                    SourceFileStatus::Pending
                },
            };
            let manifest = SourceFileManifest::new(corpus_id, corpus_id, vec![record]);
            manifest.save(&index_path)?;
            return Ok(ManifestReconstructionReport {
                manifest,
                method: ReconstructionMethod::SingleFile,
                warnings,
                conservative_reprocessing_count: if committed_iter_pos > 0 { 1 } else { 0 },
            });
        }

        // Read row counts from parquet metadata (fast — no data scan).
        let mut row_counts: Vec<u64> = Vec::with_capacity(parquet_paths.len());
        for path in &parquet_paths {
            match read_parquet_row_count(path) {
                Ok(n) => row_counts.push(n),
                Err(e) => {
                    warnings.push(format!(
                        "Could not read row count for {}: {e}. Assuming 0.",
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                    ));
                    row_counts.push(0);
                }
            }
        }

        // Build cumulative row sums.
        let mut cumulative: Vec<u64> = Vec::with_capacity(parquet_paths.len());
        let mut acc: u64 = 0;
        for &n in &row_counts {
            acc += n;
            cumulative.push(acc);
        }

        // Classify each file: Complete if cumulative_end <= committed_iter_pos,
        // InProgress for the boundary file, Pending for the rest.
        let mut conservative_count = 0;
        let mut files: Vec<SourceFileRecord> = parquet_paths
            .iter()
            .zip(row_counts.iter())
            .enumerate()
            .map(|(i, (path, &size_hint))| {
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                let cumulative_end = cumulative[i];
                let cumulative_start = if i == 0 { 0 } else { cumulative[i - 1] };

                let status = if cumulative_end <= committed_iter_pos {
                    // All rows from this file have been ingested.
                    SourceFileStatus::Complete {
                        chunks_indexed: 0, // unknown without a LanceDB query
                        completed_at: Utc::now(),
                    }
                } else if cumulative_start < committed_iter_pos {
                    // committed_iter_pos falls within this file: partial.
                    // Reset to Pending so the entire file is re-processed on
                    // resume — conservative but correct.
                    conservative_count += 1;
                    SourceFileStatus::Pending
                } else {
                    SourceFileStatus::Pending
                };

                SourceFileRecord {
                    file_index: i,
                    filename,
                    size_bytes: {
                        // Approximate from actual file size if available.
                        std::fs::metadata(path).map(|m| m.len()).unwrap_or(size_hint)
                    },
                    status,
                }
            })
            .collect();

        // Sort by file_index (they're already in order, but be explicit).
        files.sort_by_key(|r| r.file_index);

        let manifest = SourceFileManifest::new(corpus_id, corpus_id, files);
        manifest.save(&index_path)?;

        Ok(ManifestReconstructionReport {
            manifest,
            method: ReconstructionMethod::IterPosVerification,
            warnings,
            conservative_reprocessing_count: conservative_count,
        })
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

    /// Merge partition shard directories produced by collaborative ingestion.
    ///
    /// Pre-checks that all shards share the same `embedding_model` and
    /// `embedding_dimensions` before merging to prevent silently mixing
    /// incompatible vector spaces.
    pub async fn merge_partitions(
        &self,
        shard_dirs: &[PathBuf],
        output_dir: &PathBuf,
    ) -> Result<IndexInfo> {
        if shard_dirs.is_empty() {
            return Err(Error::NoShardsFound("no shard directories provided".into()));
        }

        // Verify all shards share the same embedding model + dimensions.
        let mut ref_model: Option<String> = None;
        let mut ref_dims: Option<usize> = None;
        for shard_path in shard_dirs {
            let idx = CorpusIndex::open(shard_path).await?;
            let info = idx.info().await?;
            match (&ref_model, &ref_dims) {
                (None, None) => {
                    ref_model = Some(info.embedding_model.clone());
                    ref_dims = Some(info.embedding_dimensions);
                }
                (Some(m), Some(d)) if m != &info.embedding_model || *d != info.embedding_dimensions => {
                    return Err(Error::Recipe(format!(
                        "embed model mismatch: expected {m}/{d} but shard '{}' has {}/{}",
                        shard_path.display(),
                        info.embedding_model,
                        info.embedding_dimensions,
                    )));
                }
                _ => {}
            }
        }

        crate::sharding::merge_shards(shard_dirs, output_dir).await
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
                    source_file: None,
                    code: crate::index::InsertCodeMeta::default(),
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

/// Read the total row count from a parquet file's metadata.
///
/// Uses the file metadata only — no data pages are read, so this is fast
/// even for large parquet shards.
fn read_parquet_row_count(path: &Path) -> crate::error::Result<u64> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| Error::Extraction(format!("parquet metadata read: {e}")))?;
    let metadata = builder.metadata();
    let row_count: u64 = metadata
        .row_groups()
        .iter()
        .map(|rg| rg.num_rows() as u64)
        .sum();
    Ok(row_count)
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

    #[test]
    fn in_progress_ingestions_returns_started_corpora() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":100000}"#,
        ).unwrap();

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir, mock_embed_fn());
        let result = engine.in_progress_ingestions();
        assert_eq!(result, vec!["wikipedia"]);
    }

    #[test]
    fn in_progress_ingestions_skips_not_yet_started() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        // committed_iter_pos == 0 means still downloading — not eligible
        std::fs::write(
            idx_dir.join("wikipedia/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":0}"#,
        ).unwrap();

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir, mock_embed_fn());
        assert!(engine.in_progress_ingestions().is_empty());
    }

    #[test]
    fn in_progress_ingestions_skips_complete_corpora() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia/_corpus_meta.json"),
            r#"{"ingestion_in_progress":false,"committed_iter_pos":5000000}"#,
        ).unwrap();

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir, mock_embed_fn());
        assert!(engine.in_progress_ingestions().is_empty());
    }
}
