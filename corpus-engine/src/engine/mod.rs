//! CorpusEngine — orchestrates acquisition, extraction, chunking,
//! embedding, and indexing of corpus data.

pub mod article_stats;
mod cancel;
mod ingest;

#[cfg(feature = "treesitter")]
pub mod reindex;

pub use article_stats::ArticleStats;
pub use cancel::{CancellationFlag, CancellationRegistry};

/// Consolidated on-disk state for a single corpus — what
/// [`CorpusEngine::corpus_disk_status`] reports.
///
/// Intentionally flat and serde-friendly so the commonwealth-api
/// `/internal/corpus/status` handler can drop it straight into its
/// response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorpusDiskStatus {
    pub corpus_id: String,
    /// Canonical `<corpus>/` directory exists with a meta file.
    pub canonical_present: bool,
    /// Partition-of-self `<corpus>-partition-<self>/` directory
    /// exists with a meta file.
    pub partition_present: bool,
    /// `ingestion_in_progress=true` on the canonical meta.
    pub canonical_in_progress: bool,
    /// `ingestion_in_progress=true` on the partition-of-self meta.
    pub partition_in_progress: bool,
    /// Latest `committed_iter_pos` across partition-of-self and
    /// canonical (partition preferred when both are present).
    pub committed_iter_pos: u64,
    /// ZIP shard indices known to have been fully committed —
    /// merged across canonical and every partition subdirectory
    /// for this corpus.
    pub shards_completed: Vec<usize>,
    /// Total JSONL shard count inside the source ZIP. `0` when the
    /// corpus does not have a multi-shard source (HF parquet, plain
    /// JSONL, code corpora) — the UI treats that as "no shard-based
    /// percent estimate available".
    pub shards_total: usize,
}

impl CorpusDiskStatus {
    /// Best-effort completion estimate in `[0.0, 1.0]`, or `None`
    /// when the on-disk signals don't support a sensible estimate.
    ///
    /// Current heuristic: for multi-shard JSONL corpora the shard
    /// completion ratio is both honest and responsive (processed
    /// shards tick up coarsely but reliably). For everything else we
    /// return `None` and let the UI fall back to a phase label — the
    /// raw `IngestProgress` percent isn't reliable enough to bless as
    /// a standalone completion estimate without more context.
    pub fn estimated_fraction(&self) -> Option<f32> {
        if self.shards_total > 0 {
            Some(self.shards_completed.len() as f32 / self.shards_total as f32)
        } else {
            None
        }
    }
}

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};

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

/// Runtime-registered acquirer closure. Receives the custom acquirer
/// `params` blob from the recipe and the per-ingest `download_dir`;
/// returns the local path that the extractor should read (typically a
/// JSONL file).
///
/// Uses `Pin<Box<dyn Future>>` rather than the static-dispatch [`Acquirer`]
/// trait because it must be object-safe: the registry stores heterogeneous
/// implementations keyed by `kind` string.
///
/// Progress reporting is intentionally omitted here. The
/// `ProgressCallback` type is not `Clone`, and KnowledgeView-style
/// acquirers (SQLite → JSONL) finish in sub-second time, so a progress
/// bar buys nothing. If a future custom acquirer needs progress, the
/// closure can emit it via its own side channel.
pub type CustomAcquirerFn = Arc<
    dyn Fn(
            serde_json::Value,
            PathBuf,
        ) -> Pin<Box<dyn Future<Output = Result<PathBuf>> + Send>>
        + Send
        + Sync,
>;

/// Default partition-suffix for engines constructed without a mesh
/// node id (standalone CLI / tests). Mesh daemons override via
/// [`CorpusEngine::with_self_node_id`] at startup.
pub(crate) const DEFAULT_LOCAL_NODE_SUFFIX: &str = "local";

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
    /// Display-formatted identifier for this node, used as the partition
    /// suffix when ingesting into `<corpus>-partition-<self_node_id>`.
    /// Callers (daemon startup, CLI) set this from the persistent node
    /// id. Defaults to `"local"` for standalone CLI flows where no mesh
    /// membership exists.
    self_node_id: String,
    /// Shared registry of cancellation flags for in-flight ingest tasks.
    /// The install command, the peer ingest_partition handler, and any
    /// future background finalizer all register their flag here so a
    /// user-initiated cancel from Desktop can signal whichever task is
    /// actually running.
    cancel_registry: CancellationRegistry,
    /// Runtime-registered acquirers, keyed by the `kind` string on
    /// [`AcquirerConfig::Custom`]. Populated by callers (typically
    /// `sovereign-tools` via [`CorpusEngine::register_acquirer`]) so
    /// DB-reading acquirers can live outside this crate.
    custom_acquirers: Arc<RwLock<HashMap<String, CustomAcquirerFn>>>,
}

/// Returns the raw ZIP entry indices (in TOC order) that represent real
/// JSONL shard payloads.
///
/// Rejects:
///   - Entries inside `__MACOSX/...` (macOS zip tool's metadata folder).
///   - Entries whose **basename** starts with `._` (macOS resource forks
///     like `._enwiki_namespace_0_37.jsonl` — the `.jsonl` extension is
///     incidental; these are AppleDouble files and must not be treated
///     as shard payloads).
///   - Entries not ending in `.jsonl` or `.ndjson`.
///
/// This is the single source of truth for "what counts as a shard" —
/// both [`CorpusEngine::jsonl_source_shard_count`] and the
/// `WikipediaJsonlShardedZipIterator` consume this list so the logical
/// shard index N → physical ZIP index mapping is identical on every
/// peer. Adding a third TOC-walker elsewhere without going through this
/// helper will re-introduce the drift that made peers pick up
/// `__MACOSX/._*.jsonl` resource forks as data.
pub(crate) fn canonical_jsonl_shard_entries(zip_path: &Path) -> Result<Vec<usize>> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        Error::Extraction(format!(
            "Failed to read ZIP TOC at {}: {e}",
            zip_path.display()
        ))
    })?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name();
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let basename = match name.rsplit('/').next() {
            Some(b) => b,
            None => name,
        };
        if basename.starts_with("._") {
            continue;
        }
        let lower = name.to_lowercase();
        if !(lower.ends_with(".jsonl") || lower.ends_with(".ndjson")) {
            continue;
        }
        out.push(i);
    }
    Ok(out)
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
            expected_embedding_model: "qwen3-embedding-0.6b".to_string(),
            self_node_id: DEFAULT_LOCAL_NODE_SUFFIX.to_string(),
            cancel_registry: CancellationRegistry::new(),
            custom_acquirers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a custom acquirer keyed by `kind`. Recipes referencing
    /// `[acquire] type = "custom" kind = "<kind>" params = { ... }`
    /// will dispatch through this closure at ingest time.
    ///
    /// Overwrites any previously registered acquirer with the same
    /// `kind`. Call before [`CorpusEngine::ingest`].
    pub fn register_acquirer(&self, kind: impl Into<String>, acquirer: CustomAcquirerFn) {
        let mut guard = self
            .custom_acquirers
            .write()
            .expect("custom_acquirers RwLock poisoned");
        guard.insert(kind.into(), acquirer);
    }

    /// Look up a registered custom acquirer by `kind`. Used by the
    /// ingest dispatch in [`acquire_source`].
    pub(crate) fn custom_acquirer(&self, kind: &str) -> Option<CustomAcquirerFn> {
        let guard = self
            .custom_acquirers
            .read()
            .expect("custom_acquirers RwLock poisoned");
        guard.get(kind).cloned()
    }

    /// Set the partition suffix used when writing to
    /// `<corpus>-partition-<self_node_id>`. Expected input is the
    /// Display form of a `NodeId` (the same value gossip carries and
    /// the collaborate coordinator uses in its path formatting).
    pub fn with_self_node_id(mut self, node_id: impl Into<String>) -> Self {
        self.self_node_id = node_id.into();
        self
    }

    /// Read-only accessor for the node-id suffix.
    pub fn self_node_id(&self) -> &str {
        &self.self_node_id
    }

    /// Handle into the shared cancellation registry. The daemon hands
    /// this out to HTTP routes (`POST /internal/corpus/cancel`) and to
    /// the install command so that both can signal the ingest loop.
    pub fn cancel_registry(&self) -> CancellationRegistry {
        self.cancel_registry.clone()
    }

    /// Signal a running ingest of `corpus_id` to stop cooperatively.
    /// Returns true when a flag was found and flipped; false when no
    /// ingest is registered for this corpus.
    ///
    /// The ingest task exits with [`Error::Cancelled`]; callers are
    /// expected to await task exit and then run
    /// [`remove_corpus_everything`](Self::remove_corpus_everything).
    pub fn cancel_corpus_ingest(&self, corpus_id: &str) -> bool {
        self.cancel_registry.cancel(corpus_id)
    }

    /// Canonical per-corpus partition directory for this node:
    /// `<index_dir>/<corpus_id>-partition-<self_node_id>`.
    ///
    /// Every in-progress ingest writes here (solo install, coordinator's
    /// local share, peer share). The canonical `<corpus>/` directory is
    /// materialised only by the finalise/merge step, never by a direct
    /// ingest write.
    pub fn partition_path(&self, corpus_id: &str) -> PathBuf {
        self.index_dir
            .join(format!("{corpus_id}-partition-{}", self.self_node_id))
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

    /// Count JSONL shards inside the source ZIP for a corpus, without
    /// extracting. Used by the collaborative-ingestion planner to
    /// decide whether article-range partitioning is safe (works for
    /// single-shard sources) or has to be deferred to file-index
    /// partitioning (multi-shard sources like Wikipedia, where the
    /// article-range split is unsafe across peers with non-identical
    /// extractions).
    ///
    /// Returns `Err` if no `_downloads/{corpus_id}.zip` exists — the
    /// caller treats that as "not applicable, proceed with the
    /// single-shard fast path."
    pub fn jsonl_source_shard_count(&self, corpus_id: &str) -> Result<usize> {
        let zip_path = self
            .index_dir
            .join("_downloads")
            .join(format!("{corpus_id}.zip"));
        if !zip_path.exists() {
            return Err(Error::Recipe(format!(
                "No source ZIP at {} — cannot determine shard count",
                zip_path.display()
            )));
        }
        Ok(canonical_jsonl_shard_entries(&zip_path)?.len())
    }

    /// Return a cached `ArticleStats` for a corpus's extracted JSONL
    /// without doing any I/O beyond reading the sidecar file. Returns
    /// `None` when no sidecar exists yet or when the sidecar's
    /// `(mtime, size)` don't match the current source file.
    ///
    /// Callers that want to guarantee a stats value should fall
    /// through to [`compute_article_stats`](Self::compute_article_stats)
    /// on `None`.
    pub fn cached_article_stats(&self, corpus_id: &str) -> Option<ArticleStats> {
        let jsonl_path = self
            .index_dir
            .join("_downloads")
            .join(format!("{corpus_id}.extracted.jsonl"));
        article_stats::read_sidecar(&jsonl_path)
    }

    /// Sample the first ~100 MB of `<corpus>.extracted.jsonl` and
    /// write a sidecar recording the article / section counts. This
    /// is the `None`-case fallback for
    /// [`cached_article_stats`](Self::cached_article_stats) — it
    /// does 1–2 s of blocking I/O on first call per corpus, then
    /// subsequent calls are sidecar-only.
    ///
    /// Returns `None` when no extracted JSONL exists yet (e.g. fresh
    /// install still downloading). Errors from the sampler are
    /// logged and coalesced to `None` so the UI falls back to an
    /// indeterminate progress indicator rather than crashing out.
    ///
    /// **Blocking**: synchronous I/O. Wrap in `spawn_blocking` when
    /// called from an async context.
    pub fn compute_article_stats(&self, corpus_id: &str) -> Option<ArticleStats> {
        let jsonl_path = self
            .index_dir
            .join("_downloads")
            .join(format!("{corpus_id}.extracted.jsonl"));
        if !jsonl_path.exists() {
            return None;
        }
        if let Some(cached) = article_stats::read_sidecar(&jsonl_path) {
            return Some(cached);
        }
        match article_stats::sample_article_stats(&jsonl_path) {
            Ok(stats) => Some(stats),
            Err(e) => {
                tracing::warn!(
                    corpus_id,
                    error = %e,
                    "compute_article_stats: sampler failed — UI percent will stay indeterminate"
                );
                None
            }
        }
    }

    /// Consolidated on-disk status for a single corpus, suitable for
    /// rendering a status row in the Desktop settings UI.
    ///
    /// Reads every source in one pass — the canonical `_corpus_meta.json`,
    /// the partition-of-self `_corpus_meta.json`, the merged
    /// `processed_shards` view, and the ZIP's shard count — so
    /// clients don't have to make four round-trips through the engine.
    /// Everything is best-effort: a missing or unreadable file yields
    /// the default value for the corresponding field rather than an
    /// error, because the UI can still render a partial row.
    pub fn corpus_disk_status(&self, corpus_id: &str) -> CorpusDiskStatus {
        let canonical = self.index_dir.join(corpus_id);
        let partition = self.partition_path(corpus_id);

        let read_meta = |path: &Path| -> Option<serde_json::Value> {
            let raw = std::fs::read_to_string(path.join("_corpus_meta.json")).ok()?;
            serde_json::from_str(&raw).ok()
        };

        let canonical_meta = read_meta(&canonical);
        let partition_meta = read_meta(&partition);

        let canonical_present = canonical_meta.is_some();
        let partition_present = partition_meta.is_some();

        let canonical_in_progress = canonical_meta
            .as_ref()
            .and_then(|m| m["ingestion_in_progress"].as_bool())
            .unwrap_or(false);
        let partition_in_progress = partition_meta
            .as_ref()
            .and_then(|m| m["ingestion_in_progress"].as_bool())
            .unwrap_or(false);

        // committed_iter_pos: prefer the partition's (newer work
        // goes there under the unified primitive), fall back to the
        // canonical's for legacy resumes.
        let committed_iter_pos = partition_meta
            .as_ref()
            .and_then(|m| m["committed_iter_pos"].as_u64())
            .or_else(|| {
                canonical_meta
                    .as_ref()
                    .and_then(|m| m["committed_iter_pos"].as_u64())
            })
            .unwrap_or(0);

        let shards_completed = self.corpus_processed_shards(corpus_id);
        // ZIP shard count is only relevant for JSONL multi-shard
        // corpora (Wikipedia); other sources surface 0 here, which
        // the UI interprets as "no shard-based percent estimate".
        let shards_total = self.jsonl_source_shard_count(corpus_id).unwrap_or(0);

        CorpusDiskStatus {
            corpus_id: corpus_id.to_string(),
            canonical_present,
            partition_present,
            canonical_in_progress,
            partition_in_progress,
            committed_iter_pos,
            shards_completed,
            shards_total,
        }
    }

    /// Return the set of ZIP shard indices that have been fully
    /// committed for a JSONL corpus, merged across the canonical
    /// corpus index and any per-partition subdirectories produced
    /// by a prior collaborative run.
    ///
    /// Used by the collaborative-ingestion coordinator to decide
    /// which shards still need to be assigned. Returns an empty set
    /// when no index (canonical or partition) exists yet — correct
    /// for a fresh corpus.
    pub fn corpus_processed_shards(&self, corpus_id: &str) -> Vec<usize> {
        let mut shards: std::collections::BTreeSet<usize> = Default::default();

        // Walk index_dir: include the canonical `<corpus>` dir and any
        // `<corpus>-partition-*` directories whose meta records
        // processed_shards.
        let Ok(entries) = std::fs::read_dir(&self.index_dir) else {
            return Vec::new();
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let is_canonical = name == corpus_id;
            let is_partition = name
                .strip_prefix(&format!("{corpus_id}-partition-"))
                .is_some();
            if !is_canonical && !is_partition {
                continue;
            }
            let meta_path = path.join("_corpus_meta.json");
            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            if let Some(arr) = v.get("processed_shards").and_then(|x| x.as_array()) {
                for v in arr {
                    if let Some(i) = v.as_u64() {
                        shards.insert(i as usize);
                    }
                }
            }
        }
        shards.into_iter().collect()
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

    /// Return corpus IDs where ingestion has started but not finished.
    ///
    /// Considers two on-disk shapes, both produced by the unified ingest
    /// primitive:
    ///
    /// 1. **Canonical `<corpus>/`** with `ingestion_in_progress: true` AND
    ///    `committed_iter_pos > 0`. Legacy shape from pre-unification
    ///    ingests; still detected so existing partial indexes get resumed.
    ///
    /// 2. **Partition-of-self `<corpus>-partition-<self>/`** with
    ///    `ingestion_in_progress: true`. The new-style partial output
    ///    written by every install (solo, coordinator, or peer). We do
    ///    not require `committed_iter_pos > 0` here because a partition
    ///    that was registered but never flushed a batch is still in
    ///    progress from the daemon's perspective — the auto-collaborate
    ///    loop should still pick it up when peers become available.
    ///
    /// Used by the auto-collaborate loop. Callers should cross-check
    /// against `active_ingests` to skip corpora with a live ingest task
    /// already running.
    pub fn in_progress_ingestions(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.index_dir) else {
            return vec![];
        };

        let self_partition_prefix =
            format!("-partition-{}", self.self_node_id);
        let mut out: std::collections::BTreeSet<String> = Default::default();

        for entry in entries.flatten() {
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else { continue };
            if name.starts_with('_') {
                continue;
            }

            let meta_path = entry.path().join("_corpus_meta.json");
            let Ok(content) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content)
            else {
                continue;
            };
            if v["ingestion_in_progress"].as_bool() != Some(true) {
                continue;
            }

            // Determine the corpus id this directory belongs to.
            let corpus_id = if let Some(rest) = name.strip_suffix(&self_partition_prefix)
            {
                // <corpus>-partition-<self> for our own node.
                rest.to_string()
            } else if name.contains("-partition-") {
                // <corpus>-partition-<peer>: skip — the coordinator's
                // coordinate_merge is responsible for these, not the
                // auto-collaborate loop.
                continue;
            } else if v["committed_iter_pos"].as_u64().unwrap_or(0) > 0 {
                // Legacy canonical path with committed data.
                name.to_string()
            } else {
                continue;
            };
            out.insert(corpus_id);
        }

        out.into_iter().collect()
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
        // before the model name was recorded correctly (they originally
        // stored a placeholder default) remain searchable after the fix.
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

    /// Wipe all local on-disk state for `corpus_id`: the canonical
    /// index directory **and** every `<corpus_id>-partition-*` sibling
    /// left behind by a collaborative run.
    ///
    /// This is the implementation of the Desktop "Cancel / Remove"
    /// action. It does **not** signal a running ingest task; callers
    /// are expected to fire the corpus's cancellation flag via the
    /// registry and await task exit before wiping, otherwise an
    /// in-flight LanceDB writer may recreate files after the delete.
    ///
    /// Silently ignores missing directories — the end state is
    /// idempotent ("corpus is absent on this node"). A partial failure
    /// (some dirs removed, one errored) returns the first error but
    /// does not attempt to roll back: the caller's next invocation
    /// will complete the cleanup.
    pub fn remove_corpus_everything(&self, corpus_id: &str) -> Result<()> {
        let mut first_err: Option<std::io::Error> = None;

        // Canonical first so a concurrent observer never sees the
        // canonical disappear while partition dirs still claim
        // "ingestion_in_progress=false" (which would briefly look
        // "installed" to UI polls that skip partition dirs).
        let canonical = self.index_dir.join(corpus_id);
        if canonical.exists() {
            if let Err(e) = std::fs::remove_dir_all(&canonical) {
                first_err = Some(e);
            }
        }

        // Every <corpus>-partition-* sibling.
        let prefix = format!("{corpus_id}-partition-");
        if let Ok(entries) = std::fs::read_dir(&self.index_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else { continue };
                if !name_str.starts_with(&prefix) {
                    continue;
                }
                if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }

        if let Some(e) = first_err {
            return Err(Error::Database(format!(
                "remove_corpus_everything({corpus_id}): {e}"
            )));
        }
        Ok(())
    }

    /// One-shot migration: rename the canonical `<corpus_id>/`
    /// directory to `<corpus_id>-partition-<self>/` and mark its
    /// meta as partition-shaped. Used by the `sovereign corpus
    /// migrate-to-partition` CLI helper so users with pre-unified
    /// canonical ingests (committed chunks from a prior daemon
    /// version) can participate in collaborative ingest without
    /// losing their progress.
    ///
    /// Preconditions (all required; error otherwise):
    ///   - Canonical dir exists with a `_corpus_meta.json`.
    ///   - The meta's `ingestion_in_progress` is `true` — migrating
    ///     a completed corpus would turn a working index back into a
    ///     work-in-progress partition, which is nonsense.
    ///   - Partition-of-self dir does NOT exist (we refuse to
    ///     overwrite it).
    ///
    /// Post-rename, the meta is rewritten with:
    ///   - `is_shard = true`
    ///   - `processed_shards = []` (legacy canonical predates
    ///     per-shard tracking; the sharded extractor will re-emit
    ///     the remaining shards and content-hash dedup catches any
    ///     overlap with committed chunks)
    ///   - `committed_iter_pos` and `ingestion_in_progress=true`
    ///     preserved verbatim so resume semantics carry over.
    pub fn migrate_canonical_to_partition(&self, corpus_id: &str) -> Result<PathBuf> {
        let canonical = self.index_dir.join(corpus_id);
        let partition = self.partition_path(corpus_id);

        if !canonical.exists() {
            return Err(Error::IndexNotFound(format!(
                "No canonical index at {} — nothing to migrate",
                canonical.display()
            )));
        }
        let meta_path = canonical.join("_corpus_meta.json");
        if !meta_path.exists() {
            return Err(Error::IndexNotFound(format!(
                "Canonical at {} has no _corpus_meta.json",
                canonical.display()
            )));
        }
        if partition.exists() {
            return Err(Error::Database(format!(
                "Partition-of-self already exists at {} — refusing to overwrite",
                partition.display()
            )));
        }

        let raw = std::fs::read_to_string(&meta_path)?;
        let mut meta: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| Error::Serialization(format!("read meta: {e}")))?;
        let in_progress = meta["ingestion_in_progress"].as_bool().unwrap_or(false);
        if !in_progress {
            return Err(Error::Database(format!(
                "Canonical at {} is fully ingested (ingestion_in_progress=false); \
                 migration would misrepresent a completed corpus as work-in-progress. \
                 If you really want to re-participate in collaborative ingest from \
                 this snapshot, clear ingestion_in_progress in _corpus_meta.json first.",
                canonical.display()
            )));
        }

        // Rename first; if the meta rewrite fails we still have a
        // partition-of-self dir that matches the old meta (which is
        // safe and recoverable by hand). Doing it in the other order
        // would write partition-shape meta into the canonical dir,
        // which is weird if the rename subsequently fails.
        if let Some(parent) = partition.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&canonical, &partition)?;

        if let Some(obj) = meta.as_object_mut() {
            obj.insert("is_shard".into(), serde_json::Value::Bool(true));
            obj.entry("processed_shards".to_string())
                .or_insert(serde_json::Value::Array(Vec::new()));
            // Ensure we leave ingestion_in_progress=true even if the
            // source omitted it (older layouts defaulted to false via
            // serde).
            obj.insert(
                "ingestion_in_progress".into(),
                serde_json::Value::Bool(true),
            );
        }
        let rewritten = serde_json::to_string_pretty(&meta)
            .map_err(|e| Error::Serialization(format!("write meta: {e}")))?;
        std::fs::write(partition.join("_corpus_meta.json"), rewritten)?;

        tracing::info!(
            corpus_id,
            from = %canonical.display(),
            to = %partition.display(),
            "migrate_canonical_to_partition: legacy canonical promoted to partition-of-self"
        );
        Ok(partition)
    }

    /// Promote this node's `<corpus_id>-partition-<self>` directory to
    /// the canonical `<corpus_id>/` by atomic rename, clearing the
    /// partition-specific metadata. Idempotent: returns `Ok(false)`
    /// when there is nothing to promote.
    ///
    /// Refuses to run and returns `Ok(false)` when *any* other
    /// `<corpus_id>-partition-*` directory is present — that means at
    /// least one peer participated, so the correct finaliser is the
    /// multi-partition `ShardManager::coordinate_merge`, not this
    /// single-shard rename.
    pub fn finalise_solo_ingest(&self, corpus_id: &str) -> Result<bool> {
        let canonical = self.index_dir.join(corpus_id);
        if canonical.exists() {
            // Canonical already present (previous solo finalise, or
            // merge leader already finished). Nothing to do.
            return Ok(false);
        }

        let self_partition = self.partition_path(corpus_id);
        if !self_partition.exists() {
            return Ok(false);
        }

        // Check for any peer partitions — if present, defer to merge.
        let prefix = format!("{corpus_id}-partition-");
        let self_suffix = format!("{corpus_id}-partition-{}", self.self_node_id);
        if let Ok(entries) = std::fs::read_dir(&self.index_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else { continue };
                if name_str.starts_with(&prefix) && name_str != self_suffix {
                    tracing::info!(
                        corpus_id,
                        peer_partition = %entry.path().display(),
                        "finalise_solo_ingest: peer partition present — deferring to coordinate_merge"
                    );
                    return Ok(false);
                }
            }
        }

        crate::sharding::promote_single_shard(&self_partition, &canonical)?;
        tracing::info!(
            corpus_id,
            from = %self_partition.display(),
            to = %canonical.display(),
            "finalise_solo_ingest: promoted partition-of-self to canonical"
        );
        Ok(true)
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
                            unit_id: None,
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
        // written before the embedding-model default was formalised
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
    fn partition_path_uses_default_local_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            mock_embed_fn(),
        );
        assert_eq!(engine.self_node_id(), "local");
        assert_eq!(
            engine.partition_path("wikipedia"),
            dir.path().join("indexes").join("wikipedia-partition-local")
        );
    }

    #[test]
    fn partition_path_reflects_with_self_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            mock_embed_fn(),
        )
        .with_self_node_id("b88252e4325bc377");
        assert_eq!(engine.self_node_id(), "b88252e4325bc377");
        assert_eq!(
            engine.partition_path("wikipedia"),
            dir.path()
                .join("indexes")
                .join("wikipedia-partition-b88252e4325bc377")
        );
    }

    #[test]
    fn cancel_registry_handles_survive_cloning() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            mock_embed_fn(),
        );
        let a = engine.cancel_registry();
        let b = engine.cancel_registry();
        let flag = a.register("wikipedia");
        assert!(b.get("wikipedia").is_some());
        b.cancel("wikipedia");
        assert!(flag.is_cancelled());
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

    #[test]
    fn in_progress_ingestions_includes_partition_of_self() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-nodeA")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia-partition-nodeA/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":0}"#,
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        )
        .with_self_node_id("nodeA");

        assert_eq!(engine.in_progress_ingestions(), vec!["wikipedia".to_string()]);
    }

    #[test]
    fn in_progress_ingestions_skips_other_nodes_partition_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        // A peer's partition dir sitting on disk (maybe cached before a prior
        // merge). The auto-collaborate loop should NOT treat this as an
        // in-progress ingest for *this* node — coordinate_merge owns those.
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-peerX")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia-partition-peerX/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":500}"#,
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        )
        .with_self_node_id("nodeA");

        assert!(engine.in_progress_ingestions().is_empty());
    }

    #[test]
    fn in_progress_ingestions_dedups_canonical_and_self_partition() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":100}"#,
        )
        .unwrap();
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-nodeA")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia-partition-nodeA/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":0}"#,
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        )
        .with_self_node_id("nodeA");

        assert_eq!(engine.in_progress_ingestions(), vec!["wikipedia".to_string()]);
    }

    // ── remove_corpus_everything ──────────────────────────────────────

    #[test]
    fn remove_corpus_everything_wipes_canonical_and_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-local")).unwrap();
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-abc123")).unwrap();
        std::fs::create_dir_all(idx_dir.join("openalex")).unwrap(); // unrelated

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir.clone(), mock_embed_fn());
        engine.remove_corpus_everything("wikipedia").unwrap();

        assert!(!idx_dir.join("wikipedia").exists());
        assert!(!idx_dir.join("wikipedia-partition-local").exists());
        assert!(!idx_dir.join("wikipedia-partition-abc123").exists());
        // Other corpora are untouched.
        assert!(idx_dir.join("openalex").exists());
    }

    #[test]
    fn remove_corpus_everything_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir, mock_embed_fn());
        // No canonical, no partitions — must still return Ok.
        engine.remove_corpus_everything("wikipedia").unwrap();
    }

    // ── finalise_solo_ingest ──────────────────────────────────────────

    /// Write a minimal but valid `_corpus_meta.json` so the partition dir
    /// looks like an ingest output to `promote_single_shard`.
    fn seed_partition_meta(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("_corpus_meta.json"),
            r#"{
                "corpus_id": "wikipedia",
                "corpus_name": "Wikipedia",
                "embedding_model": "m",
                "embedding_dimensions": 8,
                "mesh_sharing": true,
                "license": "MIT",
                "created_at": 0,
                "last_updated": 0,
                "is_shard": true,
                "chunk_range_start": 0,
                "chunk_range_end": 100,
                "ingestion_in_progress": true,
                "processed_shards": [0, 1, 2]
            }"#,
        )
        .unwrap();
    }

    #[test]
    fn corpus_disk_status_reports_partition_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-nodeA")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia-partition-nodeA/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":1234,"processed_shards":[0,1,2]}"#,
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        )
        .with_self_node_id("nodeA");

        let status = engine.corpus_disk_status("wikipedia");
        assert!(status.partition_present);
        assert!(status.partition_in_progress);
        assert!(!status.canonical_present);
        assert_eq!(status.committed_iter_pos, 1234);
        assert_eq!(status.shards_completed, vec![0, 1, 2]);
        // No ZIP on disk → shards_total is 0, estimated_fraction is None.
        assert_eq!(status.shards_total, 0);
        assert_eq!(status.estimated_fraction(), None);
    }

    #[test]
    fn corpus_disk_status_reports_legacy_canonical_resume() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        std::fs::write(
            idx_dir.join("wikipedia/_corpus_meta.json"),
            r#"{"ingestion_in_progress":true,"committed_iter_pos":3651456,"processed_shards":[]}"#,
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        )
        .with_self_node_id("nodeA");

        let status = engine.corpus_disk_status("wikipedia");
        assert!(status.canonical_present);
        assert!(status.canonical_in_progress);
        assert!(!status.partition_present);
        assert_eq!(status.committed_iter_pos, 3651456);
        assert!(status.shards_completed.is_empty());
    }

    // ── migrate_canonical_to_partition ────────────────────────────────

    fn seed_canonical_meta(dir: &std::path::Path, committed: u64, in_progress: bool) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("_corpus_meta.json"),
            serde_json::json!({
                "corpus_id": "wikipedia",
                "ingestion_in_progress": in_progress,
                "committed_iter_pos": committed,
                "is_shard": false,
            })
            .to_string(),
        )
        .unwrap();
        // Also drop a dummy chunks.lance subdir so assertions on
        // directory move are faithful to the real LanceDB layout.
        std::fs::create_dir_all(dir.join("chunks.lance")).unwrap();
        std::fs::write(dir.join("chunks.lance/.keep"), "").unwrap();
    }

    #[test]
    fn migrate_moves_canonical_to_partition_and_flags_meta() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        seed_canonical_meta(&idx_dir.join("wikipedia"), 3_651_456, true);

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir.clone(),
            mock_embed_fn(),
        )
        .with_self_node_id("node-abc");

        let new_path = engine.migrate_canonical_to_partition("wikipedia").unwrap();
        assert_eq!(new_path, idx_dir.join("wikipedia-partition-node-abc"));
        assert!(!idx_dir.join("wikipedia").exists());
        assert!(idx_dir.join("wikipedia-partition-node-abc").exists());
        // LanceDB contents carried across the rename.
        assert!(
            idx_dir
                .join("wikipedia-partition-node-abc/chunks.lance/.keep")
                .exists(),
            "chunks.lance must travel with the rename — data preserved"
        );

        let raw = std::fs::read_to_string(
            idx_dir.join("wikipedia-partition-node-abc/_corpus_meta.json"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["is_shard"], serde_json::Value::Bool(true));
        assert_eq!(
            v["ingestion_in_progress"],
            serde_json::Value::Bool(true),
            "ingestion_in_progress must stay true so resume picks up"
        );
        assert_eq!(
            v["committed_iter_pos"].as_u64(),
            Some(3_651_456),
            "committed_iter_pos must be preserved exactly"
        );
        assert!(v["processed_shards"].as_array().unwrap().is_empty());
    }

    #[test]
    fn migrate_refuses_when_no_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        )
        .with_self_node_id("node-abc");
        assert!(engine.migrate_canonical_to_partition("wikipedia").is_err());
    }

    #[test]
    fn migrate_refuses_when_canonical_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        seed_canonical_meta(&idx_dir.join("wikipedia"), 10, false);

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir.clone(),
            mock_embed_fn(),
        )
        .with_self_node_id("node-abc");
        let err = engine
            .migrate_canonical_to_partition("wikipedia")
            .unwrap_err();
        // Canonical should stay untouched on the refusal path.
        assert!(idx_dir.join("wikipedia").exists());
        assert!(!idx_dir.join("wikipedia-partition-node-abc").exists());
        let msg = err.to_string();
        assert!(msg.contains("fully ingested"), "unexpected error: {msg}");
    }

    #[test]
    fn migrate_refuses_when_partition_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        seed_canonical_meta(&idx_dir.join("wikipedia"), 5, true);
        std::fs::create_dir_all(idx_dir.join("wikipedia-partition-node-abc")).unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir.clone(),
            mock_embed_fn(),
        )
        .with_self_node_id("node-abc");

        assert!(engine.migrate_canonical_to_partition("wikipedia").is_err());
        // Canonical stays put.
        assert!(idx_dir.join("wikipedia").exists());
    }

    #[test]
    fn finalise_solo_ingest_promotes_partition_to_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        seed_partition_meta(&idx_dir.join("wikipedia-partition-local"));

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir.clone(), mock_embed_fn());
        let promoted = engine.finalise_solo_ingest("wikipedia").unwrap();
        assert!(promoted);
        assert!(!idx_dir.join("wikipedia-partition-local").exists());
        assert!(idx_dir.join("wikipedia").exists());

        let raw = std::fs::read_to_string(idx_dir.join("wikipedia/_corpus_meta.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["is_shard"], serde_json::Value::Bool(false));
        assert_eq!(v["ingestion_in_progress"], serde_json::Value::Bool(false));
        assert!(v["processed_shards"].as_array().unwrap().is_empty());
    }

    #[test]
    fn finalise_solo_ingest_no_partition_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir, mock_embed_fn());
        assert!(!engine.finalise_solo_ingest("wikipedia").unwrap());
    }

    #[test]
    fn finalise_solo_ingest_defers_when_peer_partition_present() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        seed_partition_meta(&idx_dir.join("wikipedia-partition-local"));
        seed_partition_meta(&idx_dir.join("wikipedia-partition-peerX"));

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir.clone(), mock_embed_fn());
        let promoted = engine.finalise_solo_ingest("wikipedia").unwrap();
        assert!(!promoted, "must defer to coordinate_merge when a peer partition exists");
        assert!(idx_dir.join("wikipedia-partition-local").exists());
        assert!(idx_dir.join("wikipedia-partition-peerX").exists());
        assert!(!idx_dir.join("wikipedia").exists());
    }

    #[test]
    fn finalise_solo_ingest_noop_when_canonical_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("wikipedia")).unwrap();
        seed_partition_meta(&idx_dir.join("wikipedia-partition-local"));

        let engine = CorpusEngine::new(dir.path().join("recipes"), idx_dir.clone(), mock_embed_fn());
        assert!(!engine.finalise_solo_ingest("wikipedia").unwrap());
        // Partition dir stays — caller (the merge path, or a later cleanup)
        // is responsible for resolving the conflict.
        assert!(idx_dir.join("wikipedia-partition-local").exists());
    }

    fn build_zip_with_entries(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        use std::io::Write;
        let zip_path = dir.join("test.zip");
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
        zip_path
    }

    #[test]
    fn canonical_jsonl_shard_entries_filters_macos_junk() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = build_zip_with_entries(
            dir.path(),
            &[
                ("real_one.jsonl", b"{}\n"),                      // raw 0 — keep
                ("__MACOSX/._real_one.jsonl", b"resource fork"),  // raw 1 — drop (__MACOSX/)
                ("._also_resource.jsonl", b"rf"),                 // raw 2 — drop (._ prefix)
                ("real_two.ndjson", b"{}\n"),                     // raw 3 — keep
                ("readme.txt", b"hi"),                            // raw 4 — drop (ext)
                ("nested/real_three.jsonl", b"{}\n"),             // raw 5 — keep
                ("nested/._rf.jsonl", b"rf"),                     // raw 6 — drop (basename ._)
            ],
        );

        let canonical = canonical_jsonl_shard_entries(&zip_path).unwrap();
        assert_eq!(canonical, vec![0, 3, 5]);
    }

    #[test]
    fn jsonl_source_shard_count_reflects_canonical_after_filter() {
        let dir = tempfile::tempdir().unwrap();
        let idx_dir = dir.path().join("indexes");
        std::fs::create_dir_all(idx_dir.join("_downloads")).unwrap();

        // Build a ZIP that would have appeared as 4 shards to the
        // pre-fix counter (extension-only filter) because the
        // __MACOSX resource fork happens to end in .jsonl. The
        // canonical count is 2.
        let zip_path = build_zip_with_entries(
            &idx_dir.join("_downloads"),
            &[
                ("shard_0.jsonl", b"{}\n"),
                ("__MACOSX/._shard_0.jsonl", b"rf"),
                ("shard_1.jsonl", b"{}\n"),
                ("._shard_2.jsonl", b"rf"),
            ],
        );
        // rename to the expected filename for jsonl_source_shard_count
        std::fs::rename(&zip_path, idx_dir.join("_downloads/wikipedia.zip")).unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            idx_dir,
            mock_embed_fn(),
        );
        assert_eq!(engine.jsonl_source_shard_count("wikipedia").unwrap(), 2);
    }
}
