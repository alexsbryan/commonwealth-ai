// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon family's port, [`IngestPort`], and its vocabulary: what svrn's
//! daemon asks ingest to do and reads back from its partition, collaborate,
//! recipe and status calls. The types above the port moved from corpus-engine,
//! which re-exports each at its historical path (pb-ingest-dial-daemon-ports).

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use corpus_engine_yield::{ForegroundSignal, YieldHook};
use serde::{Deserialize, Serialize};
use sovereign_contracts::daemon_wire::{RecipeDryRunReport, RecipeParameterSchema};

use super::cancel::CancellationRegistry;
use super::merge::PartitionMergePort;
use super::newsworthy::NewsworthyHost;
use super::{CatalogIngestPort, LocalCorpusPort, ProgressCallback};
use crate::index::CorpusIndex;
use crate::Result;

// ─── Ingest Result ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub corpus_id: String,
    pub chunks_created: u64,
    pub index_size_bytes: u64,
    pub duration_secs: u64,
    /// Documents skipped due to extraction errors (e.g. invalid UTF-8, corrupt lines).
    /// Non-zero warrants inspection of the source file on the ingesting node.
    #[serde(default)]
    pub docs_skipped: u64,
}

/// Consolidated on-disk state for a single corpus — what
/// the engine's `CorpusEngine::corpus_disk_status` reports.
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

/// Persistent snapshot of the sampler's output. Matches the on-disk
/// sidecar JSON shape exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleStats {
    /// Estimated total number of JSONL lines (articles) in the
    /// extracted file. For tiny files where the whole thing was
    /// scanned, this is exact.
    pub total_articles: u64,
    /// Mean sections per article in the sample. `1.0` means each
    /// article contributes one extracted doc (e.g. lead-only); `2.5`
    /// means 2.5 sections on average, typical of Wikipedia.
    pub mean_sections_per_article: f64,
    /// Product of the two above — the best denominator for
    /// `committed_iter_pos / total_sections_estimate` percents.
    pub total_sections_estimate: u64,
    /// Source-file mtime (unix seconds) captured at sample time.
    /// Used for cache invalidation.
    pub source_mtime_secs: u64,
    /// Source-file size in bytes captured at sample time.
    pub source_size_bytes: u64,
    /// When the sample ran, unix seconds. Purely diagnostic.
    pub sampled_at_secs: u64,
}

impl ArticleStats {
    /// Returns `true` when this cached snapshot was generated from a
    /// source file whose `(mtime, size)` still matches. Any drift
    /// invalidates the estimate — the file has been re-extracted or
    /// appended to since we sampled.
    pub fn matches_source(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        size == self.source_size_bytes && mtime == self.source_mtime_secs
    }
}

/// Per-file entry in the engine's `SourceFileManifest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceFileRecord {
    /// Zero-based position in the sorted HuggingFace parquet shard list.
    pub file_index: usize,
    /// Filename only, e.g. `"train-00021-of-00041.parquet"`.
    pub filename: String,
    /// Raw file size at download time; used to estimate storage requirements.
    pub size_bytes: u64,
    pub status: SourceFileStatus,
}

/// Lifecycle state of a single source file within the ingestion pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state")]
pub enum SourceFileStatus {
    Pending,
    InProgress {
        started_at: DateTime<Utc>,
    },
    Complete {
        /// Number of chunks written to the LanceDB index from this file.
        chunks_indexed: u64,
        completed_at: DateTime<Utc>,
    },
    Failed {
        reason: String,
    },
}

// ─── The daemon's port ──────────────────────────────────

/// A recipe's mesh-privacy posture, read from the recipe itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipeSharing {
    /// `[corpus] mesh_sharing`.
    pub mesh_sharing: bool,
    /// `[corpus] grantable`.
    pub grantable: bool,
}

/// What the corpus catalog shows of a registry entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryListing {
    /// The entry declares enrichment.
    pub enrichment_enabled: bool,
    /// Where the entry's recipe TOML is fetched from.
    pub toml_url: String,
}

/// A recipe's offline validation: the pass `svrn recipe validate` runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeValidation {
    /// Blocking problems; empty means the recipe is valid.
    pub errors: Vec<String>,
    /// Non-blocking problems.
    pub warnings: Vec<String>,
    /// What the recipe will do that the author may want to override.
    pub notes: Vec<String>,
    /// The recipe's enrichment produces atoms.
    pub enrichment_ready: bool,
}

/// The prose terms a recipe's custom ontology declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeVocabulary {
    /// What the corpus calls a position.
    pub position_term: Option<String>,
    /// What it calls a tension.
    pub tension_term: Option<String>,
    /// What it calls a concern.
    pub concern_term: Option<String>,
    /// What it calls evidence.
    pub evidence_term: Option<String>,
}

/// Why a registry install could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallRefusal {
    /// The registry could not resolve the recipe.
    RecipeNotFound(String),
    /// The parameters did not coerce or did not validate against the recipe.
    InvalidParameters(String),
    /// A supplied recipe TOML did not load (`Recipe::from_toml`, the one load
    /// boundary), or names another corpus than the one it was asked to
    /// install as.
    InvalidRecipe(String),
}

/// One registry ingest, started by [`PreparedInstall::run`].
pub type InstallRun = Box<
    dyn FnOnce(
            Option<ProgressCallback>,
        ) -> Pin<Box<dyn Future<Output = Result<IngestResult>> + Send>>
        + Send,
>;

/// A registry recipe resolved and parameterised, ready to ingest.
pub struct PreparedInstall {
    /// The recipe declares `[enrichment] enabled = false`.
    pub opts_out_of_auto_enrichment: bool,
    /// Ingest the resolved recipe.
    pub run: InstallRun,
}

/// The harness port's card, named here so its implementor depends on the
/// port's crate rather than on `sovereign-contracts` directly.
pub use sovereign_contracts::daemon_wire::HarnessRunCardView;

/// The recipe authoring harness over a frozen sample, as the daemon's harness
/// route drives it. Implemented beside the harness, which judges in ingest's
/// config language, and composed where the engine is built.
#[async_trait]
pub trait RecipeHarnessPort: Send + Sync {
    /// Capture a frozen sample under `harness_root` if there is none, run
    /// rungs 1-5 of `recipe_toml` over it and, with `enrich`, verify the atoms
    /// already written under `index_dir`. `notice` hears the networked step.
    /// Errors name the step they failed in.
    async fn run_recipe_harness(
        &self,
        recipe_toml: &str,
        harness_root: &Path,
        sample_size: usize,
        enrich: bool,
        index_dir: &Path,
        notice: &(dyn for<'s> Fn(&'s str) + Sync),
    ) -> std::result::Result<HarnessRunCardView, String>;
}

/// Opens a corpus by id, the caller's choice of open.
pub type IndexOpener =
    Box<dyn FnOnce(String) -> Pin<Box<dyn Future<Output = Result<CorpusIndex>> + Send>> + Send>;

/// Builds the daemon's newsworthy host for the watcher's corpus id.
pub type NewsworthyHostFactory = Box<dyn FnOnce(String) -> Arc<dyn NewsworthyHost> + Send>;

/// Everything svrn's daemon asks of ingest beyond the tool families' ports.
/// The daemon holds one of these as its corpus handle and hands each tool
/// family and grants the narrower port it names.
#[async_trait]
pub trait IngestPort: LocalCorpusPort + CatalogIngestPort + PartitionMergePort {
    // ── Layout ──
    /// The directory the registry's local recipes live under.
    fn recipes_dir(&self) -> &Path;
    /// This node's partition directory for `corpus_id`.
    fn partition_path(&self, corpus_id: &str) -> PathBuf;
    /// The canonical index directory for `corpus_id`.
    fn canonical_path(&self, corpus_id: &str) -> PathBuf;
    /// `corpus_id`'s canonical index is installed.
    fn corpus_is_installed(&self, corpus_id: &str) -> bool;

    // ── Ingest state on disk ──
    /// Corpora whose meta says an ingest is in progress.
    fn in_progress_ingestions(&self) -> Vec<String>;
    /// Corpora with a finished partition that was never promoted.
    fn corpora_with_stranded_partitions(&self) -> Vec<String>;
    /// The registry every running ingest's cancel flag is kept in.
    fn cancel_registry(&self) -> CancellationRegistry;
    /// A source-file manifest loads for `corpus_id` (an unreadable one reads
    /// as absent, as every caller treated it).
    fn has_source_manifest(&self, corpus_id: &str) -> bool;
    /// The manifest's files not yet complete.
    fn remaining_source_files(&self, corpus_id: &str) -> Result<Vec<SourceFileRecord>>;
    /// Articles in `corpus_id`'s extracted JSONL download.
    fn count_jsonl_articles(&self, corpus_id: &str) -> Result<u64>;
    /// JSONL shards inside `corpus_id`'s source ZIP.
    fn jsonl_source_shard_count(&self, corpus_id: &str) -> Result<usize>;
    /// The article a committed iteration position has reached, sampled.
    fn estimate_article_pos(
        &self,
        corpus_id: &str,
        committed_iter_pos: u64,
        sample_size: usize,
    ) -> Result<Option<u64>>;
    /// ZIP shards committed across the canonical and every partition.
    fn corpus_processed_shards(&self, corpus_id: &str) -> Vec<usize>;
    /// The canonical meta's `committed_iter_pos`, `0` when unreadable.
    fn corpus_committed_iter_pos(&self, corpus_id: &str) -> u64;
    /// `corpus_id`'s consolidated on-disk state.
    fn corpus_disk_status(&self, corpus_id: &str) -> CorpusDiskStatus;
    /// The article-stats sidecar, when it still matches its source.
    fn cached_article_stats(&self, corpus_id: &str) -> Option<ArticleStats>;
    /// Sample the extracted JSONL and write the article-stats sidecar.
    fn compute_article_stats(&self, corpus_id: &str) -> Option<ArticleStats>;
    /// The on-disk status rows `/internal/corpus/status` serves, as their
    /// wire JSON.
    fn corpus_status_rows(&self) -> std::io::Result<serde_json::Value>;
    /// Open `path` without the handle cache.
    async fn open_index_transient(&self, path: &Path) -> Result<CorpusIndex>;
    /// A human-readable report on every index directory.
    async fn diagnose_indexes(&self) -> String;
    /// Retry `index`'s recorded field-skeleton failures: `(retried, fixed)`.
    fn reprocess_skeleton_failures(&self, index: &CorpusIndex) -> Result<(usize, usize)>;
    /// Stream `canonical_path` as a zstd tar at `compression_level`; the
    /// bytes read.
    fn pack_canonical(
        &self,
        canonical_path: &Path,
        writer: Box<dyn std::io::Write + Send>,
        compression_level: i32,
    ) -> Result<u64>;
    /// Unpack a stream `pack_canonical` wrote into `dest`, which must not
    /// exist; the bytes written.
    fn unpack_canonical(&self, reader: Box<dyn std::io::Read + Send>, dest: &Path) -> Result<u64>;

    // ── Foreground ──
    /// Install the hook ingest yields to between batches.
    fn set_yield_hook(&self, hook: Arc<dyn YieldHook>);
    /// Install the "a person is waiting" signal.
    fn set_foreground_signal(&self, signal: Arc<dyn ForegroundSignal>);

    // ── Ingest ──
    /// Ingest registry recipe `recipe_id` into `output_path`, restricted to
    /// the given files or article range.
    #[allow(clippy::too_many_arguments)]
    async fn ingest_with_overrides(
        &self,
        recipe_id: &str,
        file_indices: Option<Vec<usize>>,
        article_range: Option<(u64, u64)>,
        output_path: &Path,
        progress: Option<ProgressCallback>,
        unit_id: Option<u32>,
    ) -> Result<IngestResult>;
    /// Continue a sampled corpus to its full source.
    async fn expand_corpus_to_full(
        &self,
        corpus_id: &str,
        progress: Option<ProgressCallback>,
    ) -> Result<IngestResult>;
    /// Resume conversation enrichment an earlier daemon left unfinished; the
    /// count resumed.
    async fn resume_interrupted_conversation_enrichment(&self) -> usize;
    /// Fetch registry recipe `corpus_id`, coerce and resolve `parameters`
    /// against it, and hand back the ingest to run.
    async fn prepare_registry_install(
        self: Arc<Self>,
        corpus_id: &str,
        parameters: &BTreeMap<String, serde_json::Value>,
    ) -> std::result::Result<PreparedInstall, InstallRefusal>;
    /// Load `recipe_toml` (it must name `corpus_id`), resolve `parameters`
    /// against it, and hand back the ingest to run. The run writes the recipe
    /// into the local registry, ingests it, and stamps `recipe_sha256` of
    /// its text on the corpus (`Corpus::stamp_recipe_sha256`).
    async fn prepare_recipe_install(
        self: Arc<Self>,
        corpus_id: &str,
        recipe_toml: &str,
        parameters: &BTreeMap<String, serde_json::Value>,
    ) -> std::result::Result<PreparedInstall, InstallRefusal>;

    // ── Recipes ──
    /// `corpus_id`'s recipe's privacy posture.
    async fn recipe_sharing(&self, corpus_id: &str) -> Result<RecipeSharing>;
    /// The registry entry `id`, as the catalog lists it.
    fn registry_listing(&self, id: &str) -> Option<RegistryListing>;
    /// The `[corpus] id` of a recipe TOML; `Err` when it does not parse.
    fn recipe_corpus_id(&self, toml_text: &str) -> Result<String>;
    /// Validate a recipe TOML offline; `Err` when it does not parse.
    fn validate_recipe_toml(&self, toml_text: &str) -> Result<RecipeValidation>;
    /// Install a recipe TOML into the local registry; its path.
    fn install_local_recipe(&self, toml_text: &str) -> Result<PathBuf>;
    /// Registry recipe `corpus_id`'s declared `[parameters]`.
    async fn recipe_parameter_schema(&self, corpus_id: &str) -> Result<RecipeParameterSchema>;
    /// Run the recipe harness on the recipe file at `recipe_path` (no
    /// embedding), as the dry-run report.
    async fn dry_run_recipe(
        &self,
        recipe_path: &Path,
        sample_size: usize,
        offline: bool,
    ) -> Result<RecipeDryRunReport>;
    /// The same run, as the protocol's per-stage recipe test report, in its
    /// wire JSON (this leaf cannot name `oicp-types`).
    async fn test_recipe_report(
        &self,
        recipe_path: &Path,
        sample_size: usize,
        offline: bool,
    ) -> Result<serde_json::Value>;
    /// The custom-ontology terms of the recipe file at `recipe_path`:
    /// `Ok(None)` when it declares no custom ontology, `Err` when it does
    /// not parse.
    fn recipe_vocabulary(&self, recipe_path: &Path) -> Result<Option<RecipeVocabulary>>;
    /// `[enrichment] domain` of the recipe file at `recipe_path`; `None`
    /// when it has none or does not parse.
    fn recipe_enrichment_domain(&self, recipe_path: &Path) -> Option<String>;

    // ── Newsworthy ──
    /// Start the newsworthy watcher over the host `host` builds for the
    /// watcher's corpus id.
    fn spawn_newsworthy_watcher(
        self: Arc<Self>,
        host: NewsworthyHostFactory,
        shutdown: tokio::sync::watch::Receiver<bool>,
        force_tick: tokio::sync::mpsc::Receiver<()>,
    ) -> tokio::task::JoinHandle<()>;
    /// Apply one newsworthy tick's atom delta to `corpus_id`'s atlas;
    /// `Err(reason)` asks the caller to rebuild in full. The index is opened
    /// through `open`, the caller's choice: the daemon's is its caching read
    /// of a served corpus, a choice corpus-engine's residency census
    /// (tests/main/index_cache_residency.rs) forbids the engine to make.
    async fn apply_newsworthy_incremental(
        &self,
        open: IndexOpener,
        indexes_dir: PathBuf,
        corpus_id: String,
        role: &'static str,
        doc_ids: Vec<String>,
    ) -> std::result::Result<(), String>;
}
