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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusShardInfo {
    pub corpus_id: String,
    pub chunk_range: Option<ChunkRange>,
    pub is_replica: bool,
    pub last_updated: u64,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestionHandoff {
    pub handoff_id: HandoffId,
    pub corpus_id: String,
    pub recipe_id: String,
    /// Embedding model that all participating nodes must share.
    pub embed_model: EmbedModelInfo,
    pub partitions: Vec<IngestionPartition>,
    /// The node responsible for collecting peer shards and calling
    /// `merge_shards()`.  Defaults to the node with the lowest `NodeId`
    /// among all partitions.  If that node goes offline, the next-lowest
    /// `Complete` node takes over via LWW gossip update.
    pub merge_assigned_to: Option<NodeId>,
    pub created_at: u64,   // Unix timestamp (ms)
    pub updated_at: u64,   // Unix timestamp (ms)
}

impl IngestionHandoff {
    /// Create a new handoff with the given partitions.
    /// Sets `merge_assigned_to` to the lowest `NodeId` among all partitions.
    pub fn new(
        corpus_id: impl Into<String>,
        recipe_id: impl Into<String>,
        embed_model: EmbedModelInfo,
        partitions: Vec<IngestionPartition>,
    ) -> Self {
        let merge_assigned_to = partitions.iter().map(|p| p.node_id).min();
        let now = now_ms();
        Self {
            handoff_id: HandoffId::generate(),
            corpus_id: corpus_id.into(),
            recipe_id: recipe_id.into(),
            embed_model,
            partitions,
            merge_assigned_to,
            created_at: now,
            updated_at: now,
        }
    }
}

/// One node's share of the ingestion work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestionPartition {
    pub node_id: NodeId,
    /// Indices into the sorted HuggingFace parquet shard list.
    pub file_indices: Vec<usize>,
    pub status: PartitionStatus,
}

/// Lifecycle of a single partition.
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
    Failed {
        reason: String,
    },
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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
