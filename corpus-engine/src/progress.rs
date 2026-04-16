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
            .map_err(|e| crate::error::Error::Io(e))?;
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
            .map_err(|e| crate::error::Error::Io(e))?;
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
#[derive(Debug, Clone)]
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
    },
    Indexing {
        chunks_indexed: u64,
        total: u64,
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
