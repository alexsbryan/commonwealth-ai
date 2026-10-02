// SPDX-License-Identifier: AGPL-3.0-or-later
//! The merge port's vocabulary: what a partition merge reports as it runs and
//! when it finishes, and what the alignment projection wrote. sovereign-grants
//! decides WHO merges and pulls the peers' partitions; the merge itself is
//! ingest's, reached through this port (pb-grants-merge; phase-b-49: port
//! vocabulary goes with its port). corpus-engine re-exports each type at its
//! historical path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::index::CorpusIndex;
use crate::source::CorpusReadPort;
use crate::types::IndexInfo;
use crate::Result;

/// The merge family's port (sovereign-grants' `ShardManager` and
/// `auto_recover`): the merge, the finalize and the alignment projection a
/// collaborative ingest triggers. Ingest's engine implements it; every
/// method is one engine call, so grants decides and ingest merges.
#[async_trait]
pub trait PartitionMergePort: CorpusReadPort {
    /// Merge the partition index directories `partitions` into one index at
    /// `output`. Writes the chunks and stops: the result is not yet
    /// finalized.
    async fn merge_partitions(&self, partitions: &[PathBuf], output: &Path) -> Result<IndexInfo>;

    /// Finish a merged canonical so every surface can see it: build its
    /// indexes, mark them built, mark the ingest complete, stamp the
    /// fingerprint.
    async fn finalize_canonical(&self, canonical: &CorpusIndex, corpus_id: &str) -> Result<()>;

    /// Merge every `<corpus_id>-partition-*` under `index_dir` into the
    /// finalized canonical `<index_dir>/<corpus_id>`; phases go to
    /// `progress`.
    async fn merge_partitions_into_canonical(
        &self,
        index_dir: &Path,
        corpus_id: &str,
        progress: Option<Arc<dyn Fn(MergePhaseProgress) + Send + Sync>>,
    ) -> Result<PartitionMergeReport>;

    /// Project the alignment rows of the canonical at `canonical_path` onto
    /// `<home>/.claude/` (a no-op for a corpus that is not a mutable merge).
    async fn project_alignment(&self, canonical_path: &Path, home: &Path) -> Result<ProjectReport>;
}

/// Phase signals emitted by `merge_partitions_into_canonical` for
/// callers that want to render progress (CLI status lines, daemon
/// tracing, future progress streams to the UI).
#[derive(Debug, Clone)]
pub enum MergePhaseProgress {
    /// Discovery + preflight finished. `partition_count` is the
    /// number of `<corpus>-partition-*/` dirs that will be merged.
    DiscoveryComplete { partition_count: usize },
    /// Chunk merge phase finished (the `merge_shards` call). Reports
    /// the deduplication that happened during merge.
    MergeComplete {
        chunks_merged: u64,
        chunks_deduped: u64,
    },
    /// Meta-stamping phase finished (scope, processed_shards,
    /// total_shards, provenance).
    MetaStamped,
    /// Sub-phase of `build_indexes` finished — propagates the
    /// `(done, total)` pair from `build_indexes`'s callback.
    BuildSubPhase { done: u64, total: u64 },
    /// `build_indexes` finished and the canonical was marked
    /// `ingestion_complete`. Recovery is finished.
    Complete,
}

/// Result of a partition-merge recovery operation.
#[derive(Debug, Clone)]
pub struct PartitionMergeReport {
    pub partition_paths: Vec<PathBuf>,
    pub canonical_path: PathBuf,
    /// Total chunks across the input partitions before dedup.
    pub chunks_input: u64,
    /// Chunks present in the canonical after dedup + merge.
    pub chunks_merged: u64,
    /// Union of `processed_shards` across all input partitions.
    pub shard_union: std::collections::BTreeSet<usize>,
    /// Resolved `total_shards` (max across inputs, falling back to
    /// `max(union)+1` when none stamped).
    pub total_shards: Option<usize>,
    /// Resolved `embedding_model` stamped on the canonical (treats
    /// empty inputs as wildcard; see `merge_shards`).
    pub embedding_model: String,
    pub embedding_dimensions: usize,
}

/// Outcome of a single projection pass. Reported back to the merge
/// caller so daemon logs / progress streams can show what landed
/// without re-walking the FS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectReport {
    pub wrote: usize,
    pub skipped_local_newer: usize,
    pub skipped_locked: bool,
    pub skipped_unsafe_path: usize,
    pub swept_incoming: usize,
    /// Number of `notes://` rows upserted into `~/.svrnmesh/notes.db`.
    pub notes_upserted: usize,
    /// Number of `notes://` rows whose payload failed to deserialize
    /// or whose embedded id mismatched the chunk's source_doc_id.
    pub notes_deserialize_errors: usize,
}
