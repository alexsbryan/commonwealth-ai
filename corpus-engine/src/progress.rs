// SPDX-License-Identifier: AGPL-3.0-or-later
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
    pub fn new(
        corpus_id: impl Into<String>,
        recipe_id: impl Into<String>,
        files: Vec<SourceFileRecord>,
    ) -> Self {
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
        let raw = std::fs::read_to_string(&manifest_path).map_err(crate::error::Error::Io)?;
        let manifest = serde_json::from_str::<Self>(&raw)
            .map_err(|e| crate::error::Error::Serialization(e.to_string()))?;
        Ok(Some(manifest))
    }

    /// Persist the manifest to `<index_path>/_source_manifest.json`.
    pub fn save(&self, index_path: &std::path::Path) -> crate::error::Result<()> {
        let manifest_path = index_path.join("_source_manifest.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| crate::error::Error::Serialization(e.to_string()))?;
        std::fs::write(&manifest_path, json).map_err(crate::error::Error::Io)?;
        Ok(())
    }
}

// The ingest port's vocabulary lives beside the port (pb-ingest-dial-daemon-ports).
pub use corpus_index::ingest_port::daemon::{SourceFileRecord, SourceFileStatus};

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

/// The ingest progress callback payload. Defined in
/// `sovereign_contracts::daemon_wire::ingest` since 2026-09-11 so a wire
/// client can name it without linking the engine; re-exported here so every
/// `corpus_engine::IngestProgress` path is unchanged.
pub use sovereign_contracts::daemon_wire::IngestProgress;

/// Thread-safe progress callback, defined beside ingest's ports in the
/// `corpus-index` leaf so a caller that holds only a port can build one.
pub use corpus_index::ingest_port::ProgressCallback; // shim: moved by pb-ingest-dial-tools
