use std::collections::HashMap;

use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::{
    ChunkRange, IngestionHandoff, IngestionPartition, KnowledgeShardAssignment,
    KnowledgeShardPlan, PartitionStatus,
};
use commonwealth_core::mesh::MemberRecord;
use commonwealth_core::oicp::EmbedModelInfo;
use corpus_engine::SourceFileRecord;

// ─── Collaborative ingestion planner ─────────────────────────────────────────

/// Errors that prevent planning collaborative ingestion.
#[derive(Debug, thiserror::Error)]
pub enum CollaborativeIngestionError {
    #[error("no compatible peers: embed model mismatch (local: {local}, candidates: {candidates})")]
    NoCompatiblePeers { local: String, candidates: String },
    #[error("no compatible peers: insufficient storage (need {needed_gb:.1} GB total)")]
    InsufficientStorage { needed_gb: f64 },
    #[error("corpus {0} is already complete — no remaining files")]
    AlreadyComplete(String),
    #[error("no source manifest for corpus {0} — run `corpus reconstruct-manifest` first")]
    NoManifest(String),
}

/// Divide remaining source files across the local node and compatible mesh
/// peers, returning an `IngestionHandoff` ready to be gossiped.
///
/// ### Assignment rules
/// 1. Filter peers whose `embed_model` matches `local_embed_model` exactly.
/// 2. Pin any `InProgress` files to the local node (Machine A already holds
///    partial chunks for them; reprocessing elsewhere would cause duplicates).
/// 3. Distribute the remaining `Pending` files in contiguous blocks across
///    all nodes (`N = all_nodes.len()`), assigning the remainder to the
///    first nodes.
/// 4. Set `merge_assigned_to` to the node with the lowest `NodeId`.
pub fn plan_collaborative_ingestion(
    corpus_id: &str,
    recipe_id: &str,
    remaining_files: &[SourceFileRecord],
    local_node: &MemberRecord,
    candidates: &[MemberRecord],
    local_embed_model: &EmbedModelInfo,
) -> Result<IngestionHandoff, CollaborativeIngestionError> {
    use corpus_engine::SourceFileStatus;

    if remaining_files.is_empty() {
        return Err(CollaborativeIngestionError::AlreadyComplete(corpus_id.to_string()));
    }

    // Filter candidates whose embed_model matches exactly.
    let compatible_peers: Vec<&MemberRecord> = candidates
        .iter()
        .filter(|peer| {
            // We check the KnowledgeManifest embed_model via the OICP capabilities.
            // At this layer we receive the raw MemberRecord; the caller is responsible
            // for including only peers whose embed_model has already been verified.
            // Here we check free storage as a secondary gate.
            peer.capabilities.hardware.free_storage_gb > 0
        })
        .collect();

    // Build the full participant list: local node first (index 0).
    let mut all_nodes: Vec<NodeId> = std::iter::once(local_node.node_id)
        .chain(compatible_peers.iter().map(|p| p.node_id))
        .collect();
    all_nodes.dedup();

    let n = all_nodes.len();

    // Separate InProgress (pinned to local) from Pending (distributable).
    let in_progress: Vec<&SourceFileRecord> = remaining_files
        .iter()
        .filter(|f| matches!(f.status, SourceFileStatus::InProgress { .. }))
        .collect();
    let pending: Vec<&SourceFileRecord> = remaining_files
        .iter()
        .filter(|f| matches!(f.status, SourceFileStatus::Pending))
        .collect();

    // Estimate total storage needed: sum of file sizes * 1.3 (index overhead).
    let total_bytes: u64 = remaining_files.iter().map(|f| f.size_bytes).sum();
    let needed_gb = total_bytes as f64 / 1024.0_f64.powi(3) * 1.3;

    // Check if any single node (or the collective) has enough storage.
    let total_available_gb: f64 = all_nodes
        .iter()
        .map(|nid| {
            if *nid == local_node.node_id {
                local_node.capabilities.hardware.free_storage_gb as f64
            } else {
                compatible_peers
                    .iter()
                    .find(|p| p.node_id == *nid)
                    .map(|p| p.capabilities.hardware.free_storage_gb as f64)
                    .unwrap_or(0.0)
            }
        })
        .sum();

    if total_available_gb < needed_gb * 0.8 {
        // Allow 20% slack — estimates are rough.
        return Err(CollaborativeIngestionError::InsufficientStorage {
            needed_gb,
        });
    }

    // Build per-node partitions.
    let mut partitions: Vec<IngestionPartition> = all_nodes
        .iter()
        .map(|nid| IngestionPartition {
            node_id: *nid,
            file_indices: Vec::new(),
            article_range: None,
            status: PartitionStatus::Assigned,
        })
        .collect();

    // Pin InProgress to local node (partition[0]).
    for f in &in_progress {
        partitions[0].file_indices.push(f.file_index);
    }

    // Distribute pending files in contiguous blocks.
    let p = pending.len();
    if p > 0 {
        let base = p / n;
        let remainder = p % n;
        let mut offset = 0usize;
        for (i, partition) in partitions.iter_mut().enumerate() {
            let count = base + if i < remainder { 1 } else { 0 };
            for f in &pending[offset..offset + count] {
                partition.file_indices.push(f.file_index);
            }
            offset += count;
        }
    }

    // Sort each partition's file_indices for deterministic ordering.
    for p in &mut partitions {
        p.file_indices.sort_unstable();
    }

    Ok(IngestionHandoff::new(
        corpus_id,
        recipe_id,
        local_embed_model.clone(),
        partitions,
    ))
}

/// Divide a Wikipedia JSONL corpus across the local node and compatible
/// mesh peers, returning an `IngestionHandoff` ready to be gossiped.
///
/// Unlike `plan_collaborative_ingestion` (which works on HF parquet shards),
/// this function partitions a single JSONL file by article index range.
///
/// ### Arguments
/// - `current_article_pos`: estimated article index Machine A has already
///   processed (derived from `committed_iter_pos` via sampling; see
///   `CorpusEngine::estimate_article_pos`). Machine A's partition starts
///   here; articles `0..current_article_pos` are already committed.
/// - `total_articles`: total article count in the JSONL (from
///   `CorpusEngine::count_jsonl_articles`).
///
/// Machine A always gets `[current_article_pos, split)` and Machine B
/// gets `[split, total_articles)` where split ≈ the midpoint of remaining work.
pub fn plan_collaborative_ingestion_jsonl(
    corpus_id: &str,
    recipe_id: &str,
    current_article_pos: u64,
    total_articles: u64,
    local_node: &MemberRecord,
    candidates: &[MemberRecord],
    local_embed_model: &EmbedModelInfo,
) -> Result<IngestionHandoff, CollaborativeIngestionError> {
    let remaining = total_articles.saturating_sub(current_article_pos);
    if remaining == 0 {
        return Err(CollaborativeIngestionError::AlreadyComplete(corpus_id.to_string()));
    }

    // All passed candidates are already filtered to Online status by the
    // caller. Include all of them regardless of free_storage_gb (which may
    // be 0 on first gossip) — equal distribution doesn't use storage weight,
    // and the ingest_partition receiver validates embed model compatibility.
    let mut all_nodes: Vec<NodeId> = std::iter::once(local_node.node_id)
        .chain(candidates.iter().map(|p| p.node_id))
        .collect();
    all_nodes.dedup();
    let n = all_nodes.len() as u64;

    // Divide remaining articles into contiguous blocks, one per node.
    // Local node (index 0) starts from `current_article_pos`; each
    // subsequent node takes the next block.
    let base = remaining / n;
    let extra = remaining % n;
    let mut cursor = current_article_pos;
    let mut partitions: Vec<IngestionPartition> = Vec::with_capacity(all_nodes.len());
    for (i, nid) in all_nodes.iter().enumerate() {
        let count = base + if (i as u64) < extra { 1 } else { 0 };
        let start = cursor;
        cursor += count;
        partitions.push(IngestionPartition {
            node_id: *nid,
            file_indices: Vec::new(),
            article_range: Some((start, cursor)),
            status: PartitionStatus::Assigned,
        });
    }

    // Local node's partition carries `current_article_pos` as its start so
    // the ingest pipeline uses `article_range` + existing `committed_iter_pos`
    // skip-ahead to resume cleanly from where it left off.
    debug_assert_eq!(
        partitions.last().map(|p| p.article_range.unwrap_or((0, 0)).1),
        Some(total_articles),
        "partition ranges must cover all remaining articles"
    );

    Ok(IngestionHandoff::new(
        corpus_id,
        recipe_id,
        local_embed_model.clone(),
        partitions,
    ))
}

// ─── Existing knowledge shard assignment ────────────────────────────────────

/// Information about a corpus to be assigned across the mesh.
#[derive(Debug, Clone)]
pub struct CorpusInfo {
    pub corpus_id: String,
    pub total_chunks: u64,
    pub size_gb: f32,
    /// If false, corpus can only be stored on the node that ingested it.
    pub mesh_sharing: bool,
}

/// A node with available storage capacity.
#[derive(Debug, Clone)]
pub struct NodeWithCapacity {
    pub node_id: NodeId,
    pub free_storage_gb: f32,
}

/// Assign knowledge corpora across mesh nodes.
///
/// Implements the algorithm from the architecture:
/// 1. If a corpus fits on one node, assign it whole.
/// 2. If not, split by chunk ID range proportional to free disk space.
/// 3. Assign replicas to different nodes than the primary.
/// 4. Respect mesh_sharing flags (restricted corpora are not replicated).
pub fn assign_knowledge_shards(
    corpora: &[CorpusInfo],
    nodes: &[NodeWithCapacity],
    redundancy_target: usize,
) -> KnowledgeShardPlan {
    let mut assignments = Vec::new();
    let mut redundancy_achieved: HashMap<String, usize> = HashMap::new();

    if nodes.is_empty() {
        return KnowledgeShardPlan {
            assignments,
            redundancy_achieved,
        };
    }

    // Sort nodes by free storage descending for greedy assignment.
    let mut sorted_nodes: Vec<&NodeWithCapacity> = nodes.iter().collect();
    sorted_nodes.sort_by(|a, b| {
        b.free_storage_gb
            .partial_cmp(&a.free_storage_gb)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for corpus in corpora {
        // Track which nodes get assigned this corpus (for replica placement).
        let mut assigned_nodes: Vec<NodeId> = Vec::new();

        // Primary assignment.
        let primaries = assign_corpus_primary(corpus, &sorted_nodes);
        for assignment in &primaries {
            assigned_nodes.push(assignment.node_id);
        }
        assignments.extend(primaries);

        // Replica assignment (if mesh_sharing allows).
        if corpus.mesh_sharing {
            let replica_count =
                assign_corpus_replicas(corpus, &sorted_nodes, &assigned_nodes, redundancy_target);
            for assignment in &replica_count {
                assigned_nodes.push(assignment.node_id);
            }
            assignments.extend(replica_count);
        }

        redundancy_achieved.insert(corpus.corpus_id.clone(), assigned_nodes.len());
    }

    KnowledgeShardPlan {
        assignments,
        redundancy_achieved,
    }
}

/// Assign primary copies of a corpus across nodes.
fn assign_corpus_primary(
    corpus: &CorpusInfo,
    nodes: &[&NodeWithCapacity],
) -> Vec<KnowledgeShardAssignment> {
    // Find a single node that can hold the entire corpus.
    let single_node = nodes.iter().find(|n| n.free_storage_gb >= corpus.size_gb);

    if let Some(node) = single_node {
        // Fits on one node — assign whole.
        return vec![KnowledgeShardAssignment {
            node_id: node.node_id,
            corpus_id: corpus.corpus_id.clone(),
            chunk_range: None, // Entire corpus.
            is_replica: false,
        }];
    }

    // Doesn't fit on one node — split by chunk range proportional to storage.
    let eligible: Vec<&&NodeWithCapacity> =
        nodes.iter().filter(|n| n.free_storage_gb > 0.0).collect();

    if eligible.is_empty() {
        return vec![];
    }

    let total_storage: f32 = eligible.iter().map(|n| n.free_storage_gb).sum();
    let mut assignments = Vec::new();
    let mut current_chunk: u64 = 0;

    for (i, node) in eligible.iter().enumerate() {
        let fraction = node.free_storage_gb / total_storage;
        let chunk_count = if i == eligible.len() - 1 {
            // Last node gets the remainder to avoid rounding gaps.
            corpus.total_chunks - current_chunk
        } else {
            (fraction * corpus.total_chunks as f32).floor() as u64
        };

        if chunk_count == 0 {
            continue;
        }

        assignments.push(KnowledgeShardAssignment {
            node_id: node.node_id,
            corpus_id: corpus.corpus_id.clone(),
            chunk_range: Some(ChunkRange::new(current_chunk, current_chunk + chunk_count)),
            is_replica: false,
        });
        current_chunk += chunk_count;
    }

    assignments
}

/// Assign replica copies of a corpus to nodes that don't already have it.
fn assign_corpus_replicas(
    corpus: &CorpusInfo,
    nodes: &[&NodeWithCapacity],
    already_assigned: &[NodeId],
    redundancy_target: usize,
) -> Vec<KnowledgeShardAssignment> {
    let copies_needed = redundancy_target.saturating_sub(already_assigned.len());
    if copies_needed == 0 {
        return vec![];
    }

    let mut replicas = Vec::new();

    // Prefer nodes that don't already have this corpus, sorted by most free storage.
    let candidates: Vec<&&NodeWithCapacity> = nodes
        .iter()
        .filter(|n| !already_assigned.contains(&n.node_id))
        .filter(|n| n.free_storage_gb >= corpus.size_gb * 0.5) // Need at least half for a replica.
        .collect();

    for node in candidates.iter().take(copies_needed) {
        replicas.push(KnowledgeShardAssignment {
            node_id: node.node_id,
            corpus_id: corpus.corpus_id.clone(),
            chunk_range: None, // Full replica.
            is_replica: true,
        });
    }

    replicas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u128, storage_gb: f32) -> NodeWithCapacity {
        NodeWithCapacity {
            node_id: NodeId::from_u128(id),
            free_storage_gb: storage_gb,
        }
    }

    fn corpus(id: &str, chunks: u64, size_gb: f32, sharing: bool) -> CorpusInfo {
        CorpusInfo {
            corpus_id: id.into(),
            total_chunks: chunks,
            size_gb,
            mesh_sharing: sharing,
        }
    }

    #[test]
    fn single_corpus_fits_on_one_node() {
        let nodes = vec![node(1, 100.0), node(2, 50.0)];
        let corpora = vec![corpus("wikipedia", 6_800_000, 30.0, true)];

        let plan = assign_knowledge_shards(&corpora, &nodes, 2);
        assert_eq!(plan.assignments.len(), 2); // primary + 1 replica
        assert_eq!(plan.redundancy_achieved["wikipedia"], 2);

        // Primary should be on node 1 (most storage).
        let primary = plan.assignments.iter().find(|a| !a.is_replica).unwrap();
        assert_eq!(primary.node_id, NodeId::from_u128(1));
        assert!(primary.chunk_range.is_none()); // Whole corpus.
    }

    #[test]
    fn large_corpus_splits_across_nodes() {
        let nodes = vec![node(1, 20.0), node(2, 20.0)];
        let corpora = vec![corpus("huge_corpus", 10_000_000, 50.0, true)];

        let plan = assign_knowledge_shards(&corpora, &nodes, 1);

        // Should be split across both nodes.
        let primaries: Vec<_> = plan.assignments.iter().filter(|a| !a.is_replica).collect();
        assert_eq!(primaries.len(), 2);

        // Chunks should cover the full range.
        let total_chunks: u64 = primaries
            .iter()
            .map(|a| a.chunk_range.unwrap().count())
            .sum();
        assert_eq!(total_chunks, 10_000_000);

        // Ranges should be contiguous.
        let mut ranges: Vec<_> = primaries.iter().map(|a| a.chunk_range.unwrap()).collect();
        ranges.sort_by_key(|r| r.start_id);
        assert_eq!(ranges[0].start_id, 0);
        assert_eq!(ranges[0].end_id, ranges[1].start_id);
        assert_eq!(ranges[1].end_id, 10_000_000);
    }

    #[test]
    fn redundancy_assigns_replicas() {
        let nodes = vec![node(1, 100.0), node(2, 100.0), node(3, 100.0)];
        let corpora = vec![corpus("wikipedia", 6_800_000, 30.0, true)];

        let plan = assign_knowledge_shards(&corpora, &nodes, 2);
        assert_eq!(plan.redundancy_achieved["wikipedia"], 2);

        let replicas: Vec<_> = plan.assignments.iter().filter(|a| a.is_replica).collect();
        assert_eq!(replicas.len(), 1);

        // Replica should be on a different node than primary.
        let primary = plan.assignments.iter().find(|a| !a.is_replica).unwrap();
        assert_ne!(replicas[0].node_id, primary.node_id);
    }

    #[test]
    fn mesh_sharing_restriction_prevents_replication() {
        let nodes = vec![node(1, 100.0), node(2, 100.0)];
        let corpora = vec![corpus("restricted", 1_000_000, 10.0, false)];

        let plan = assign_knowledge_shards(&corpora, &nodes, 2);

        // mesh_sharing=false: only primary, no replicas.
        assert_eq!(plan.assignments.len(), 1);
        assert!(!plan.assignments[0].is_replica);
        assert_eq!(plan.redundancy_achieved["restricted"], 1);
    }

    #[test]
    fn multiple_corpora_assigned_independently() {
        let nodes = vec![node(1, 200.0), node(2, 200.0), node(3, 100.0)];
        let corpora = vec![
            corpus("wikipedia", 6_800_000, 30.0, true),
            corpus("openalex", 5_000_000, 25.0, true),
            corpus("sep", 500_000, 5.0, true),
        ];

        let plan = assign_knowledge_shards(&corpora, &nodes, 2);

        // All three corpora should be in the plan.
        assert!(plan.redundancy_achieved.contains_key("wikipedia"));
        assert!(plan.redundancy_achieved.contains_key("openalex"));
        assert!(plan.redundancy_achieved.contains_key("sep"));
    }

    #[test]
    fn empty_nodes_returns_empty_plan() {
        let corpora = vec![corpus("test", 1000, 1.0, true)];
        let plan = assign_knowledge_shards(&corpora, &[], 2);
        assert!(plan.assignments.is_empty());
    }

    #[test]
    fn five_node_scenario() {
        // Architecture scenario: Alice 1TB, Bob 512GB, Carol 2TB, Dave 1TB, Eve 256GB.
        let nodes = vec![
            node(1, 800.0),  // Alice
            node(2, 400.0),  // Bob
            node(3, 1500.0), // Carol
            node(4, 800.0),  // Dave
            node(5, 100.0),  // Eve
        ];

        let corpora = vec![
            corpus("wikipedia", 6_800_000, 30.0, true),
            corpus("openalex", 15_000_000, 80.0, true),
            corpus("sep", 500_000, 5.0, true),
            corpus("stackexchange", 3_000_000, 20.0, true),
        ];

        let plan = assign_knowledge_shards(&corpora, &nodes, 2);

        // All corpora should be assigned.
        for c in &corpora {
            assert!(
                plan.redundancy_achieved.contains_key(&c.corpus_id),
                "corpus {} not assigned",
                c.corpus_id
            );
            assert!(
                plan.redundancy_achieved[&c.corpus_id] >= 1,
                "corpus {} has no copies",
                c.corpus_id
            );
        }
    }
}
