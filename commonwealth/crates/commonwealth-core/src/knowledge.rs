// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::{HandoffId, NodeId};
use crate::oicp::EmbedModelInfo;

/// The complete knowledge shard plan for the mesh.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeShardPlan {
    pub assignments: Vec<KnowledgeShardAssignment>,
    /// corpus_id -> replica count achieved.
    pub redundancy_achieved: HashMap<String, usize>,
}

/// A single node's assignment for a knowledge corpus shard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeShardAssignment {
    pub node_id: NodeId,
    pub corpus_id: String,
    /// None means the entire corpus is on this node.
    pub chunk_range: Option<ChunkRange>,
    pub is_replica: bool,
}

/// A contiguous range of chunk IDs within a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRange {
    /// First chunk ID (inclusive).
    pub start_id: u64,
    /// Last chunk ID (exclusive).
    pub end_id: u64,
}

impl ChunkRange {
    pub fn new(start_id: u64, end_id: u64) -> Self {
        debug_assert!(start_id < end_id, "empty chunk range: {start_id}..{end_id}");
        Self { start_id, end_id }
    }

    pub fn count(&self) -> u64 {
        self.end_id - self.start_id
    }
}

/// Information about a corpus shard hosted on a node.
/// Used in capability reports.
///
/// ## Phase: canonical-sync surface (Phase 6 of the resilience track)
///
/// Three new fields drive the mesh's self-healing canonical sync:
///
/// - `chunk_count`: how many chunks this peer's canonical contains.
///   Compared by the auto-recover path to pick the healthier peer
///   when several have a canonical for the same id.
/// - `canonical_fingerprint`: blake3 of the sorted content_hash list
///   for the canonical at this peer. Two peers with byte-identical
///   chunks arrive at the same string. The puller validates this
///   value against the file it actually downloaded so a poisoned
///   tarball fails closed.
/// - `total_shards` + `processed_shards`: lets a peer compute its
///   coverage ratio (`processed / total`) for sharded corpora.
///   Auto-recover compares ratios — not raw chunk counts — to pick
///   the most-complete peer, which is robust to legitimate corpus
///   updates that shrink the chunk set.
///
/// All three are `Option`/`Vec`-defaulted so older peers (whose
/// gossip blobs predate this struct) deserialize cleanly. A peer
/// missing the fields just opts out of the new sync paths until
/// it upgrades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusShardInfo {
    pub corpus_id: String,
    pub chunk_range: Option<ChunkRange>,
    pub is_replica: bool,
    pub last_updated: u64,
    /// Total chunks in this peer's canonical (or partition).
    /// Defaults to 0 for older peers; auto-recover treats `0` as
    /// "unknown" rather than "empty" — peers with a fingerprint
    /// but no chunk_count are eligible to pull from but not
    /// rankable by count.
    #[serde(default)]
    pub chunk_count: u64,
    /// Stable content fingerprint for the canonical. See
    /// `corpus_engine::IndexInfo::canonical_fingerprint` for the
    /// algorithm. `None` for partitions and for canonicals that
    /// haven't been stamped yet (the daemon's lazy-stamp pass on
    /// next start fills these in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_fingerprint: Option<String>,
    /// Total source shards this corpus expects (e.g. 38 for the
    /// canonical Wikipedia ingest). `None` for non-sharded corpora.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_shards: Option<usize>,
    /// Source shards this peer's canonical (or partition) has
    /// processed. The auto-recover path takes the union of these
    /// across peers to compute coverage ratios.
    #[serde(default)]
    pub processed_shards: Vec<usize>,
    // ── Atlas advertisement (Phase C1) ──────────────────────────
    //
    // These three fields let a peer joining the mesh decide
    // whether to pull this corpus's atlas instead of running
    // local Tier-2 enrichment. All three default to 0 / `None` so
    // older peers (and peers whose corpus has no atlas yet)
    // serialise + deserialise cleanly with no protocol break.
    /// Total atoms (entities + events + …) in this peer's
    /// `<corpus>/atlas/atoms.json`. `0` means "no atlas yet" or
    /// "older peer that doesn't advertise atlas state."
    #[serde(default)]
    pub atlas_atom_count: u64,
    /// Entities at `enrichment_depth = "extracted"` (Tier-2
    /// enriched). The mesh ranks atlases by this — a peer with a
    /// higher count has done more deep-extraction work and is the
    /// preferred atlas source for fresh nodes.
    #[serde(default)]
    pub atlas_tier2_count: u64,
    /// SHA-256 of `atoms.json` (hex). Receipt the puller validates
    /// against after fetching a peer's atlas — a corrupted /
    /// poisoned transfer fails closed. `None` = no atlas or
    /// fingerprint not yet stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atlas_fingerprint: Option<String>,
}

impl CorpusShardInfo {
    /// Coverage ratio for sharded corpora — `processed_shards.len()
    /// / total_shards`. Returns `None` for non-sharded corpora
    /// (`total_shards.is_none()`) and for the degenerate case
    /// `total_shards = 0`. Used by `auto_recover` to pick the
    /// most-complete peer.
    pub fn coverage_ratio(&self) -> Option<f64> {
        let total = self.total_shards?;
        if total == 0 {
            return None;
        }
        Some(self.processed_shards.len() as f64 / total as f64)
    }
}

// -----------------------------------------------------------------
// Collaborative ingestion handoff
// -----------------------------------------------------------------

/// A negotiated agreement between the local node (Machine A) and one or more
/// mesh peers to divide the remaining source files for a mid-flight ingestion
/// and merge the resulting partial indexes.
///
/// Persisted in gossip state as
/// `AppState { app_id: "corpus-engine", key: "handoff:{handoff_id}" }`.
///
/// Two modes, selected by the `phase` field:
/// - **Legacy static partitioning** (`phase == HandoffPhase::legacy_open()` with
///   a non-empty `partitions`): each peer receives `ingest_partition` up-front
///   with its assigned file_indices / article_range. Status reported via the
///   per-partition `PartitionStatus` enum.
/// - **Pull-based work queue** (`phase` progresses Open → Draining → Merging →
///   Complete): the coordinator holds a queue of `WorkUnit`s in memory (see
///   `commonwealth-knowledge::work_queue`). Peers pull units via HTTP. The
///   `partitions` vec is empty; status lives in the queue, not in gossip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestionHandoff {
    pub handoff_id: HandoffId,
    pub corpus_id: String,
    pub recipe_id: String,
    /// Embedding model that all participating nodes must share.
    pub embed_model: EmbedModelInfo,
    /// Legacy static-partition assignments. Empty for pull-based handoffs.
    #[serde(default)]
    pub partitions: Vec<IngestionPartition>,
    /// The node responsible for collecting peer shards and calling
    /// `merge_shards()`.  Defaults to the node with the lowest `NodeId`
    /// among all partitions (legacy) or the coordinator (pull-based).
    /// `merge_assigned_to` is the pre-rename field name; still accepted on
    /// deserialize so old gossip blobs are readable during the upgrade window.
    #[serde(alias = "merge_assigned_to")]
    pub merge_leader: Option<NodeId>,
    /// Pull-based handoff lifecycle phase. Defaults to the legacy "open"
    /// marker for blobs that predate the pull-based path — the old static
    /// `partitions` stay the source of truth in that case.
    #[serde(default = "HandoffPhase::legacy_open")]
    pub phase: HandoffPhase,
    /// Monotonic counter bumped on every gossip write so peers can
    /// distinguish a freshly-opened queue from a stale replay.
    #[serde(default)]
    pub queue_version: u32,
    /// Per-job peer allowlist for an ephemeral grant-scoped ingest.
    /// `None` = open to any embed-compatible peer (default, unchanged
    /// behaviour). `Some(set)` = only these node_ids may enroll and lease
    /// units. Peers self-enforce this in `discover_and_spawn_pull_loops`;
    /// the coordinator's queue enforces it in `WorkQueueManager::next_unit`.
    #[serde(default)]
    pub allowed_peers: Option<Vec<NodeId>>,
    /// True when this handoff is backed by an ephemeral ingest grant — a
    /// user-selected, revocable, one-off compute assist over a normally
    /// local-only corpus. Peers wipe their `<corpus>-partition-<self>/`
    /// working dir on teardown when this is set, instead of retaining it.
    #[serde(default)]
    pub ephemeral: bool,
    pub created_at: u64, // Unix timestamp (ms)
    pub updated_at: u64, // Unix timestamp (ms)
}

impl IngestionHandoff {
    /// Create a legacy static-partition handoff.
    /// Sets `merge_leader` to the lowest `NodeId` among all partitions.
    pub fn new(
        corpus_id: impl Into<String>,
        recipe_id: impl Into<String>,
        embed_model: EmbedModelInfo,
        partitions: Vec<IngestionPartition>,
    ) -> Self {
        let merge_leader = partitions.iter().map(|p| p.node_id).min();
        let now = now_ms();
        Self {
            handoff_id: HandoffId::generate(),
            corpus_id: corpus_id.into(),
            recipe_id: recipe_id.into(),
            embed_model,
            partitions,
            merge_leader,
            phase: HandoffPhase::legacy_open(),
            queue_version: 0,
            allowed_peers: None,
            ephemeral: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// Create a pull-based handoff announcement. The actual queue of units
    /// lives on the coordinator's `WorkQueueManager`; this struct is only the
    /// gossip-visible pointer that tells peers "there's work to pull here."
    pub fn new_queue(
        corpus_id: impl Into<String>,
        recipe_id: impl Into<String>,
        embed_model: EmbedModelInfo,
        merge_leader: NodeId,
    ) -> Self {
        let now = now_ms();
        Self {
            handoff_id: HandoffId::generate(),
            corpus_id: corpus_id.into(),
            recipe_id: recipe_id.into(),
            embed_model,
            partitions: Vec::new(),
            merge_leader: Some(merge_leader),
            phase: HandoffPhase::Open,
            queue_version: 1,
            allowed_peers: None,
            ephemeral: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// True when this is a pull-based handoff (populated from `new_queue`
    /// or deserialized from a peer that opened one).
    pub fn is_queue_mode(&self) -> bool {
        // Legacy handoffs set phase = legacy_open AND have non-empty partitions.
        // Queue handoffs set phase through the real lifecycle AND have empty
        // partitions. The disambiguator is `partitions.is_empty()` — phase
        // alone is insufficient because legacy_open == Open.
        self.partitions.is_empty()
            && matches!(
                self.phase,
                HandoffPhase::Open
                    | HandoffPhase::Draining
                    | HandoffPhase::Merging
                    | HandoffPhase::Complete
                    | HandoffPhase::Failed { .. }
            )
    }
}

/// One node's share of the ingestion work (legacy static partitioning).
/// Pull-based handoffs use `WorkUnit` + the coordinator's queue instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestionPartition {
    pub node_id: NodeId,
    /// Indices into the sorted HuggingFace parquet shard list.
    /// Empty for JSONL corpora (use `article_range` instead).
    pub file_indices: Vec<usize>,
    /// Article range `[start, end)` for JSONL corpora (e.g. Wikipedia).
    /// `None` for HuggingFace parquet corpora (use `file_indices` instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub article_range: Option<(u64, u64)>,
    pub status: PartitionStatus,
}

/// Lifecycle of a single partition (legacy static-partitioning path).
/// Unused by the pull-based queue, which tracks status per-unit internally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state")]
pub enum PartitionStatus {
    /// Files have been assigned but ingestion hasn't started yet.
    Assigned,
    /// Ingestion is actively running on this node.
    InProgress {
        started_at: u64, // Unix timestamp (ms)
    },
    /// All assigned files have been ingested and the shard is ready for merge.
    Complete {
        completed_at: u64, // Unix timestamp (ms)
    },
    /// Ingestion failed; the merge leader should skip or retry this partition.
    Failed { reason: String },
}

// -----------------------------------------------------------------
// Pull-based work queue (new path)
// -----------------------------------------------------------------

/// Stable identifier for a work unit inside a handoff.
/// Derived from its position in the initial queue (0..N).
pub type UnitId = u32;

/// One indivisible piece of ingestion work. Unifies the three corpus shapes
/// so the queue can round-robin units across peers without branching on
/// corpus type during dispatch.
///
/// The position in the coordinator's initial unit list becomes the unit's
/// `UnitId` — stable for the lifetime of the handoff, used for merge-time
/// dedup when lease expiry causes the same unit to be processed twice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "value")]
pub enum WorkUnit {
    /// Index into the sorted HuggingFace parquet shard list for this recipe.
    HfFile(usize),
    /// Index into the ZIP archive's ordered shard table (e.g. one of the 76
    /// `enwiki_namespace_*.jsonl` entries inside `wikipedia.zip`).
    JsonlShard(usize),
    /// Article range `[start, end)` for single-file JSONL corpora. The
    /// coordinator slices the total article count into roughly-equal units
    /// (typically ~32 per corpus) so there is enough granularity to load-
    /// balance across peers.
    JsonlRange { start: u64, end: u64 },
}

impl WorkUnit {
    /// Convert to the `(file_indices, article_range)` pair that the
    /// existing `ingest_with_overrides` / `ingest_partition` paths consume.
    pub fn to_ingest_args(&self) -> (Option<Vec<usize>>, Option<(u64, u64)>) {
        match self {
            WorkUnit::HfFile(i) => (Some(vec![*i]), None),
            WorkUnit::JsonlShard(i) => (Some(vec![*i]), None),
            WorkUnit::JsonlRange { start, end } => (None, Some((*start, *end))),
        }
    }
}

/// Per-unit status inside the coordinator's queue. Lives in memory on the
/// coordinator; not gossiped (too chatty for LWW; linearizable reads needed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state")]
pub enum UnitStatus {
    /// Waiting to be pulled by a peer. `prior_attempts` is 0 on the initial
    /// enqueue and N after N failed leases — preserved across requeue so
    /// `MAX_UNIT_ATTEMPTS` counts total attempts, not per-peer attempts.
    Queued {
        #[serde(default)]
        prior_attempts: u32,
    },
    /// A peer holds the lease. Heartbeats extend `expires_at_ms`; the reaper
    /// transitions this back to `Queued` (or terminal `Failed` after enough
    /// attempts) when the lease lapses.
    Leased {
        peer: NodeId,
        leased_at_ms: u64,
        last_heartbeat_ms: u64,
        expires_at_ms: u64,
        /// 1 on first lease; incremented on every re-lease after expiry.
        attempts: u32,
    },
    /// Peer successfully finished ingesting this unit.
    Complete { peer: NodeId, completed_at_ms: u64 },
    /// Terminal after `MAX_UNIT_ATTEMPTS` failed leases. The merge leader
    /// proceeds without this unit; the corpus will be missing its chunks.
    Failed {
        last_peer: NodeId,
        reason: String,
        attempts: u32,
    },
}

/// Overall lifecycle of a pull-based handoff. Gossiped as part of
/// `IngestionHandoff` so peers can recognize when a queue is open.
///
/// Transitions (coordinator-driven; peers observe):
/// - `Open` → `Draining` when queue empties (some leases still outstanding)
/// - `Draining` → `Merging` when all leases terminate (Complete or Failed)
/// - `Merging` → `Complete` when the leader finishes `coordinate_merge`
/// - Any → `Failed { reason }` on unrecoverable error
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "phase")]
pub enum HandoffPhase {
    Open,
    Draining,
    Merging,
    Complete,
    Failed { reason: String },
}

impl HandoffPhase {
    /// Default for blobs predating the pull-based path. A legacy handoff's
    /// partitions are the source of truth; this phase exists only so serde
    /// deserializes older gossip without error.
    pub fn legacy_open() -> Self {
        HandoffPhase::Open
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, HandoffPhase::Complete | HandoffPhase::Failed { .. })
    }
}

/// Outcome reported by a peer via `complete_unit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CompleteOutcome {
    Complete,
    Failed,
}

/// Maximum re-lease attempts before a unit becomes terminal `Failed`.
/// Unit fails three peers in a row → the merge leader proceeds without it.
pub const MAX_UNIT_ATTEMPTS: u32 = 3;

/// Default lease duration in milliseconds (5 minutes). Heartbeats refresh
/// the lease every `LEASE_MS / 3` on the peer side.
pub const LEASE_MS: u64 = 300_000;

use crate::clock::unix_now_millis as now_ms;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_range_count() {
        let r = ChunkRange::new(0, 1000);
        assert_eq!(r.count(), 1000);
    }

    #[test]
    fn chunk_range_serde_roundtrip() {
        let r = ChunkRange::new(500, 1500);
        let json = serde_json::to_string(&r).unwrap();
        let back: ChunkRange = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn corpus_shard_info_serde_roundtrips_atlas_fields() {
        let info = CorpusShardInfo {
            corpus_id: "wikipedia".into(),
            chunk_range: None,
            is_replica: false,
            last_updated: 0,
            chunk_count: 1_000_000,
            canonical_fingerprint: Some("abc123".into()),
            total_shards: Some(38),
            processed_shards: vec![0, 1, 2],
            atlas_atom_count: 51_280,
            atlas_tier2_count: 612,
            atlas_fingerprint: Some(
                "7c3f8e9b1f0a2d3c4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d".into(),
            ),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: CorpusShardInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.atlas_atom_count, 51_280);
        assert_eq!(back.atlas_tier2_count, 612);
        assert!(back.atlas_fingerprint.unwrap().starts_with("7c3f"));
    }

    /// A blob from an older peer (pre-C1) won't carry the atlas
    /// fields. Deserialize must succeed with zero counts and `None`
    /// fingerprint so the upgrade window is graceful.
    #[test]
    fn corpus_shard_info_back_compat_without_atlas_fields() {
        let json = r#"{
            "corpus_id": "wikipedia",
            "chunk_range": null,
            "is_replica": false,
            "last_updated": 0,
            "chunk_count": 0,
            "processed_shards": []
        }"#;
        let back: CorpusShardInfo = serde_json::from_str(json).unwrap();
        assert_eq!(back.atlas_atom_count, 0);
        assert_eq!(back.atlas_tier2_count, 0);
        assert!(back.atlas_fingerprint.is_none());
    }

    #[test]
    fn knowledge_shard_plan_serde_roundtrip() {
        let plan = KnowledgeShardPlan {
            assignments: vec![KnowledgeShardAssignment {
                node_id: NodeId::from_u128(1),
                corpus_id: "wikipedia".into(),
                chunk_range: None,
                is_replica: false,
            }],
            redundancy_achieved: [("wikipedia".into(), 2)].into_iter().collect(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: KnowledgeShardPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.assignments.len(), 1);
        assert_eq!(back.redundancy_achieved["wikipedia"], 2);
    }
}
