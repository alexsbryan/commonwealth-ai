use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// The complete knowledge shard plan for the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
