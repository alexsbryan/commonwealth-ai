// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon family's port vocabulary: what svrn's daemon reads back from
//! ingest's partition, collaborate and status calls. Moved from corpus-engine,
//! which re-exports each at its historical path (pb-ingest-dial-daemon-ports).

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
