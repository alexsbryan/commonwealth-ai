use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Source-file manifest ──────────────────────────────────────────────────

/// Tracks which source files (e.g. HuggingFace parquet shards) have been
/// fully committed to a LanceDB index. Written to `_source_manifest.json`
/// alongside `_corpus_meta.json` during ingestion.
///
/// Enables collaborative ingestion: Machine A can reconstruct this manifest
/// for a mid-flight index (T0) and then distribute the remaining files
/// across mesh peers (T2–T4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceFileManifest {
    pub corpus_id: String,
    pub recipe_id: String,
    /// Schema version — always 1 for now; increment when adding required fields.
    pub schema_version: u8,
    pub files: Vec<SourceFileRecord>,
    pub updated_at: DateTime<Utc>,
}

impl SourceFileManifest {
    /// Construct an initial manifest with all files in `Pending` state.
    pub fn new(corpus_id: impl Into<String>, recipe_id: impl Into<String>, files: Vec<SourceFileRecord>) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            recipe_id: recipe_id.into(),
            schema_version: 1,
            files,
            updated_at: Utc::now(),
        }
    }

    /// Read a manifest from disk. Returns `None` if the file does not exist.
    pub fn load(path: &std::path::Path) -> crate::error::Result<Option<Self>> {
        let manifest_path = path.join("_source_manifest.json");
        if !manifest_path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(crate::error::Error::Io)?;
        let manifest = serde_json::from_str::<Self>(&raw)
            .map_err(|e| crate::error::Error::Serialization(e.to_string()))?;
        Ok(Some(manifest))
    }

    /// Persist the manifest to `<index_path>/_source_manifest.json`.
    pub fn save(&self, index_path: &std::path::Path) -> crate::error::Result<()> {
        let manifest_path = index_path.join("_source_manifest.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| crate::error::Error::Serialization(e.to_string()))?;
        std::fs::write(&manifest_path, json)
            .map_err(crate::error::Error::Io)?;
        Ok(())
    }
}

/// Per-file entry in a [`SourceFileManifest`].
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

// ─── Reconstruction report ─────────────────────────────────────────────────

/// Result of reconstructing a [`SourceFileManifest`] for a pre-T1 index.
///
/// Returned by `CorpusEngine::reconstruct_source_manifest()`.
#[derive(Debug, Clone)]
pub struct ManifestReconstructionReport {
    pub manifest: SourceFileManifest,
    pub method: ReconstructionMethod,
    /// Non-fatal notes about the reconstruction (e.g. "could not read row
    /// count for file X, assumed 0").
    pub warnings: Vec<String>,
    /// Number of files that were reset from `InProgress` → `Pending` as a
    /// conservative measure (may have partial committed chunks).
    pub conservative_reprocessing_count: usize,
}

/// How the manifest was reconstructed from an existing index.
#[derive(Debug, Clone, PartialEq)]
pub enum ReconstructionMethod {
    /// `committed_iter_pos` was divided among files using per-file row counts
    /// read from parquet metadata (fast, no full scan required).
    IterPosVerification,
    /// Source parquet files were not found; fallback to a heuristic estimate
    /// based on average docs-per-file.
    ChunkCountHeuristic { median_rows_per_file: u64 },
    /// Source is a single file (e.g. JSONL); no file-level splitting.
    SingleFile,
}

// ─── Progress callbacks ────────────────────────────────────────────────────

/// Progress updates during corpus ingestion.
///
/// `Serialize`/`Deserialize` are derived so the commonwealth-api
/// `/internal/corpus/progress` endpoint can expose a snapshot of the
/// per-corpus progress map to HTTP clients (the Sovereign Desktop
/// command, the CLI, and any future headless consumer). The variant
/// tag is represented externally as the variant name.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum IngestProgress {
    Downloading {
        percent: f32,
        bytes_downloaded: u64,
        bytes_total: Option<u64>,
    },
    Extracting {
        documents_processed: u64,
    },
    Chunking {
        chunks_created: u64,
    },
    Embedding {
        chunks_embedded: u64,
        /// Total chunks expected. Zero means unknown (e.g. streaming extraction).
        total: u64,
        /// Number of source documents processed so far.
        docs_processed: u64,
        /// Embedding throughput in chunks per second over the last batch.
        chunks_per_sec: f32,
        /// Best-effort upper bound on the number of source documents the
        /// active filter expects to accept (e.g. ~51K for Wikipedia +
        /// `vital_articles_l5`). `None` for unfiltered ingests, where
        /// the natural denominator is shard-scan progress instead.
        ///
        /// When `Some`, the desktop UI prefers `docs_processed /
        /// expected_docs` over the shard-based estimate — that ratio is
        /// the only honest signal for filtered ingests, where the
        /// extractor must scan the entire source ZIP regardless of how
        /// few documents the filter accepts.
        expected_docs: Option<u64>,
    },
    Indexing {
        chunks_indexed: u64,
        total: u64,
    },
    /// Background IVF-PQ rebuild after a delta expansion. Surfaces as
    /// "Optimizing search index…" in the UI; search remains live
    /// throughout. Emitted by `CorpusEngine::expand_corpus` after the
    /// new vectors land — the centroids trained at the original (smaller)
    /// scope are suboptimal at the new scale, so the index is rebuilt
    /// in place.
    ///
    /// `current_chunks` is the chunk count at rebuild start (also the
    /// total since rebuilds run on a frozen snapshot — partitions are
    /// re-trained against the existing data, no chunks are added).
    OptimizingIndex {
        current_chunks: u64,
    },
    /// Post-embed enrichment phases (skeleton extraction, entity
    /// extraction, embedding clustering, cluster labeling, atlas
    /// build). Emitted by the field engine via the enrichment progress
    /// callback after the embed/index pipeline finishes; the desktop
    /// UI maps `phase` to a human-readable label and shows
    /// `detail` verbatim.
    ///
    /// Without this variant, the desktop polled `/internal/corpus/progress`
    /// and saw the last `Embedding` event from before enrichment
    /// started — so any ingest with enrichment enabled (conversations-
    /// anthropic, atlas-bearing recipes) appeared to hang at
    /// "Embedding chunks…" while clustering or entity extraction
    /// burned CPU silently. Observed 2026-05-20 mid-conversations
    /// ingest.
    ///
    /// `phase` is a stable machine token (`skeleton-extraction`,
    /// `entity-extraction`, `clustering`, `cluster-labeling`,
    /// `phase-skipped`, `resuming`). `detail` carries the same
    /// human-readable text the daemon already eprintln's. `fraction`
    /// is set when the underlying phase reports a numeric ratio
    /// (Phase 1b batch progress); `None` otherwise.
    Enriching {
        phase: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fraction: Option<f32>,
    },
    Complete {
        total_chunks: u64,
        duration_secs: u64,
    },
}

/// Thread-safe progress callback. Must be `Sync` because the engine's
/// async pipeline holds an `&Option<ProgressCallback>` across `.await`
/// points, which requires the callback itself to be safe to share by
/// reference between tasks.
pub type ProgressCallback = Box<dyn Fn(IngestProgress) + Send + Sync>;
