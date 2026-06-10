// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use commonwealth_core::ids::NodeId;
use commonwealth_core::knowledge::{
    ChunkRange, IngestionHandoff, IngestionPartition, KnowledgeShardAssignment, KnowledgeShardPlan,
    PartitionStatus, WorkUnit,
};
use commonwealth_core::mesh::MemberRecord;
use commonwealth_core::oicp::EmbedModelInfo;
use corpus_engine::SourceFileRecord;

// ─── Collaborative ingestion planner ─────────────────────────────────────────

/// Errors that prevent planning collaborative ingestion.
#[derive(Debug, thiserror::Error)]
pub enum CollaborativeIngestionError {
    #[error(
        "no compatible peers: embed model mismatch (local: {local}, candidates: {candidates})"
    )]
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
        return Err(CollaborativeIngestionError::AlreadyComplete(
            corpus_id.to_string(),
        ));
    }

    // Belt-and-suspenders filter. The coordinator
    // (`corpus_collaborate`) pre-filters candidates by gossiped
    // `embed_model` so the common call path feeds only compatible
    // peers in. But this function has several direct callers (tests,
    // the CLI's `sovereign mesh collaborate` subcommand) that pass
    // raw `MemberRecord` sets without running the coordinator's
    // filter. Repeating the check here means a mismatched peer
    // never ends up in `all_nodes` regardless of entry point.
    let compatible_peers: Vec<&MemberRecord> = candidates
        .iter()
        .filter(|peer| {
            if peer.capabilities.hardware.free_storage_gb == 0 {
                return false;
            }
            match peer.capabilities.embed_model.as_ref() {
                Some(em) => em == local_embed_model,
                None => false, // Peer hasn't advertised — excluded.
            }
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
        return Err(CollaborativeIngestionError::InsufficientStorage { needed_gb });
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
        return Err(CollaborativeIngestionError::AlreadyComplete(
            corpus_id.to_string(),
        ));
    }

    // Filter candidates whose gossiped embed_model matches ours.
    // Belt-and-suspenders: the coordinator also pre-filters, but
    // this function has other callers (tests, CLI) that may hand us
    // raw candidate sets. Without this check a mismatched peer
    // would get an article range it silently refuses to process,
    // leaving those articles stranded and the overall ingest
    // permanently incomplete.
    let compatible: Vec<&MemberRecord> = candidates
        .iter()
        .filter(|peer| match peer.capabilities.embed_model.as_ref() {
            Some(em) => em == local_embed_model,
            None => false,
        })
        .collect();
    let mut all_nodes: Vec<NodeId> = std::iter::once(local_node.node_id)
        .chain(compatible.iter().map(|p| p.node_id))
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
        partitions
            .last()
            .map(|p| p.article_range.unwrap_or((0, 0)).1),
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

/// Partition a set of JSONL ZIP shard indices across the local node and
/// compatible mesh peers, returning an `IngestionHandoff` ready to be
/// gossiped.
///
/// Unlike `plan_collaborative_ingestion_jsonl` (which partitions a single
/// merged JSONL by absolute article index and is only safe for single-shard
/// sources), this planner carves up the set of still-outstanding ZIP shard
/// entries. Shard boundaries come from the ZIP's table of contents, so two
/// peers with byte-identical ZIPs will produce the same article set from
/// the same shard index — correctness holds regardless of whether either
/// peer has a partial `extracted.jsonl` cache or a drifted snapshot.
///
/// ### Arguments
/// - `remaining_shards`: shard indices the coordinator has NOT yet
///   observed committed (derived from `CorpusEngine::corpus_processed_shards`).
///   Expected to be non-empty and sorted; the planner sorts+dedups
///   defensively.
///
/// Each partition carries `file_indices` = its assigned shard indices
/// and `article_range` = None.
pub fn plan_collaborative_ingestion_jsonl_sharded(
    corpus_id: &str,
    recipe_id: &str,
    remaining_shards: Vec<usize>,
    local_node: &MemberRecord,
    candidates: &[MemberRecord],
    local_embed_model: &EmbedModelInfo,
) -> Result<IngestionHandoff, CollaborativeIngestionError> {
    let mut shards = remaining_shards;
    shards.sort_unstable();
    shards.dedup();

    if shards.is_empty() {
        return Err(CollaborativeIngestionError::AlreadyComplete(
            corpus_id.to_string(),
        ));
    }

    // Embed-model filter: identical to the non-sharded JSONL planner.
    // A mismatched peer would accept the partition and then silently
    // reject at ingest_partition; keep them out of the split upfront.
    let compatible: Vec<&MemberRecord> = candidates
        .iter()
        .filter(|peer| match peer.capabilities.embed_model.as_ref() {
            Some(em) => em == local_embed_model,
            None => false,
        })
        .collect();
    let mut all_nodes: Vec<NodeId> = std::iter::once(local_node.node_id)
        .chain(compatible.iter().map(|p| p.node_id))
        .collect();
    all_nodes.dedup();
    let n = all_nodes.len();

    // Distribute shards in contiguous blocks so each peer pulls a
    // locality-adjacent slice of the ZIP's TOC — keeps on-the-fly
    // extraction cache-friendly and debug logs easy to read.
    let total = shards.len();
    let base = total / n;
    let extra = total % n;

    let mut partitions: Vec<IngestionPartition> = Vec::with_capacity(n);
    let mut cursor = 0usize;
    for (i, nid) in all_nodes.iter().enumerate() {
        let count = base + if i < extra { 1 } else { 0 };
        let slice = &shards[cursor..cursor + count];
        cursor += count;
        partitions.push(IngestionPartition {
            node_id: *nid,
            file_indices: slice.to_vec(),
            article_range: None,
            status: PartitionStatus::Assigned,
        });
    }

    // Drop empty partitions (happens when there are more nodes than
    // remaining shards). Peers with an empty slice have nothing to do;
    // better to omit them than to ship a no-op assignment that the
    // peer-side handler would have to special-case.
    partitions.retain(|p| !p.file_indices.is_empty());

    Ok(IngestionHandoff::new(
        corpus_id,
        recipe_id,
        local_embed_model.clone(),
        partitions,
    ))
}

// ─── Pull-based work-queue unit builders ────────────────────────────────────
//
// These helpers take the same inputs as the static planners above (remaining
// files / remaining shards / article range) and turn them into a flat
// `Vec<WorkUnit>` for the coordinator's `WorkQueueManager::register`. They
// do NOT slice the work across peers — the queue does that at pull time,
// weighted naturally by each peer's pull rate. Feasibility checks (embed-
// model match, storage capacity) still live at the collaborate handler.

/// Build work units for a Hugging Face parquet corpus from the list of
/// source files that still need processing. One unit per file — the unit's
/// payload is the file's index in the recipe's sorted manifest.
pub fn build_work_units_hf(remaining: &[SourceFileRecord]) -> Vec<WorkUnit> {
    remaining
        .iter()
        .map(|f| WorkUnit::HfFile(f.file_index))
        .collect()
}

/// Build work units for a multi-shard JSONL corpus (Wikipedia ZIP's 76
/// inner files). One unit per still-unprocessed shard index.
pub fn build_work_units_jsonl_sharded(remaining_shards: Vec<usize>) -> Vec<WorkUnit> {
    remaining_shards
        .into_iter()
        .map(WorkUnit::JsonlShard)
        .collect()
}

/// Build work units for a single-file JSONL corpus by slicing the article
/// index range `[start, end)` into roughly-equal sub-ranges. `target_units`
/// caps the queue size — 32 gives enough granularity for 2–4 peers without
/// being chatty. Returns a single unit when the range is shorter than
/// `target_units` (e.g. a small resume-tail).
pub fn build_work_units_jsonl_single(start: u64, end: u64, target_units: u32) -> Vec<WorkUnit> {
    if end <= start {
        return Vec::new();
    }
    let total = end - start;
    let target = target_units.max(1) as u64;
    let chunk_size = total.div_ceil(target); // ceil(total / target)
    let mut units = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let next = (cursor + chunk_size).min(end);
        units.push(WorkUnit::JsonlRange {
            start: cursor,
            end: next,
        });
        cursor = next;
    }
    units
}

#[cfg(test)]
mod work_unit_tests {
    use super::*;

    #[test]
    fn hf_builder_one_unit_per_file() {
        let files: Vec<SourceFileRecord> = (0..5)
            .map(|i| SourceFileRecord {
                file_index: i,
                filename: format!("shard-{i}.parquet"),
                size_bytes: 0,
                status: corpus_engine::SourceFileStatus::Pending,
            })
            .collect();
        let units = build_work_units_hf(&files);
        assert_eq!(units.len(), 5);
        assert_eq!(units[0], WorkUnit::HfFile(0));
        assert_eq!(units[4], WorkUnit::HfFile(4));
    }

    #[test]
    fn jsonl_sharded_builder_one_unit_per_shard() {
        let units = build_work_units_jsonl_sharded(vec![3, 7, 11]);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], WorkUnit::JsonlShard(3));
        assert_eq!(units[2], WorkUnit::JsonlShard(11));
    }

    #[test]
    fn jsonl_single_builder_slices_range() {
        let units = build_work_units_jsonl_single(0, 1000, 4);
        assert_eq!(units.len(), 4);
        assert_eq!(units[0], WorkUnit::JsonlRange { start: 0, end: 250 });
        assert_eq!(
            units[3],
            WorkUnit::JsonlRange {
                start: 750,
                end: 1000,
            }
        );
    }

    #[test]
    fn jsonl_single_builder_short_range_collapses() {
        let units = build_work_units_jsonl_single(0, 5, 32);
        // With chunk_size = ceil(5/32) = 1, we'd get 5 units of length 1.
        assert_eq!(units.len(), 5);
    }

    #[test]
    fn jsonl_single_builder_empty_on_degenerate_range() {
        assert!(build_work_units_jsonl_single(100, 100, 8).is_empty());
        assert!(build_work_units_jsonl_single(200, 100, 8).is_empty());
    }
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

    // ── collaborative-ingestion embed_model filter ─────────────────

    fn qwen_embed() -> EmbedModelInfo {
        use commonwealth_core::oicp::{NormalizationStrategy, PoolingStrategy};
        EmbedModelInfo {
            model_id: "qwen3-embedding-0.6b".into(),
            dimensions: 1024,
            pooling: PoolingStrategy::Mean,
            normalization: NormalizationStrategy::Application,
        }
    }

    fn other_embed() -> EmbedModelInfo {
        use commonwealth_core::oicp::{NormalizationStrategy, PoolingStrategy};
        EmbedModelInfo {
            model_id: "nomic-embed-text-v2".into(),
            dimensions: 768,
            pooling: PoolingStrategy::Mean,
            normalization: NormalizationStrategy::Application,
        }
    }

    fn member(id: u128, embed: Option<EmbedModelInfo>) -> MemberRecord {
        use commonwealth_core::capabilities::{
            AvailableResources, HardwareProfile, NodeCapabilities,
        };
        use commonwealth_core::mesh::NodeStatus;
        MemberRecord {
            node_pubkey: None,
            node_id: NodeId::from_u128(id),
            name: format!("node-{id}"),
            invited_by: NodeId::from_u128(1),
            joined_at: 100,
            last_seen: 100,
            status: NodeStatus::Online,
            capabilities: NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 16,
                    cpu_cores: 8,
                    total_storage_gb: 500,
                    free_storage_gb: 200,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: 100,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],
                embed_model: embed,
                benchmark: None,
                current_in_flight: None,
            },
            addresses: vec!["192.168.1.10:9742".parse().unwrap()],
        }
    }

    #[test]
    fn jsonl_plan_excludes_peer_with_mismatched_embed_model() {
        // Regression: pre-fix, a Machine B running nomic would get a
        // partition assigned and silently reject it. Now the planner
        // excludes it upfront so the split goes to local + compatible
        // peers only.
        let local = member(1, Some(qwen_embed()));
        let peer_match = member(2, Some(qwen_embed()));
        let peer_mismatch = member(3, Some(other_embed()));
        let handoff = plan_collaborative_ingestion_jsonl(
            "wikipedia",
            "wikipedia",
            0,
            1000,
            &local,
            &[peer_match.clone(), peer_mismatch.clone()],
            &qwen_embed(),
        )
        .unwrap();
        let assigned_nodes: Vec<NodeId> = handoff.partitions.iter().map(|p| p.node_id).collect();
        assert!(assigned_nodes.contains(&local.node_id));
        assert!(assigned_nodes.contains(&peer_match.node_id));
        assert!(
            !assigned_nodes.contains(&peer_mismatch.node_id),
            "peer with other_embed must not receive a partition"
        );
        assert_eq!(handoff.partitions.len(), 2);
    }

    #[test]
    fn jsonl_plan_excludes_peer_that_has_not_advertised_embed_model() {
        // Pre-bootstrap peer — gossiped capabilities but embed_model
        // is still None. Exclude conservatively; they'll re-evaluate
        // when they complete bootstrap and re-gossip.
        let local = member(1, Some(qwen_embed()));
        let peer_bootstrapping = member(2, None);
        let handoff = plan_collaborative_ingestion_jsonl(
            "wikipedia",
            "wikipedia",
            0,
            1000,
            &local,
            std::slice::from_ref(&peer_bootstrapping),
            &qwen_embed(),
        )
        .unwrap();
        assert_eq!(
            handoff.partitions.len(),
            1,
            "only local should be assigned when peer hasn't advertised embed_model"
        );
        assert_eq!(handoff.partitions[0].node_id, local.node_id);
    }

    // ── sharded-JSONL planner tests ─────────────────────────

    #[test]
    fn sharded_plan_splits_76_shards_across_2_nodes() {
        let local = member(1, Some(qwen_embed()));
        let peer = member(2, Some(qwen_embed()));
        let remaining: Vec<usize> = (0..76).collect();
        let handoff = plan_collaborative_ingestion_jsonl_sharded(
            "wikipedia",
            "wikipedia",
            remaining,
            &local,
            std::slice::from_ref(&peer),
            &qwen_embed(),
        )
        .unwrap();

        assert_eq!(handoff.partitions.len(), 2);
        let local_shards = &handoff
            .partitions
            .iter()
            .find(|p| p.node_id == local.node_id)
            .unwrap()
            .file_indices;
        let peer_shards = &handoff
            .partitions
            .iter()
            .find(|p| p.node_id == peer.node_id)
            .unwrap()
            .file_indices;
        // Contiguous, covers all 76, no overlap.
        assert_eq!(local_shards.len() + peer_shards.len(), 76);
        let mut union: Vec<usize> = local_shards
            .iter()
            .chain(peer_shards.iter())
            .copied()
            .collect();
        union.sort_unstable();
        assert_eq!(union, (0..76).collect::<Vec<_>>());
        assert_eq!(local_shards.first(), Some(&0));
        assert_eq!(peer_shards.last(), Some(&75));
    }

    #[test]
    fn sharded_plan_distributes_across_3_nodes() {
        let local = member(1, Some(qwen_embed()));
        let p2 = member(2, Some(qwen_embed()));
        let p3 = member(3, Some(qwen_embed()));
        let remaining: Vec<usize> = (0..10).collect();
        let handoff = plan_collaborative_ingestion_jsonl_sharded(
            "wikipedia",
            "wikipedia",
            remaining,
            &local,
            &[p2.clone(), p3.clone()],
            &qwen_embed(),
        )
        .unwrap();

        assert_eq!(handoff.partitions.len(), 3);
        let counts: Vec<usize> = handoff
            .partitions
            .iter()
            .map(|p| p.file_indices.len())
            .collect();
        // 10 = 4 + 3 + 3
        let total: usize = counts.iter().sum();
        assert_eq!(total, 10);
        assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1);
    }

    #[test]
    fn sharded_plan_excludes_mismatched_embed_peer() {
        let local = member(1, Some(qwen_embed()));
        let peer_ok = member(2, Some(qwen_embed()));
        let peer_bad = member(3, Some(other_embed()));
        let handoff = plan_collaborative_ingestion_jsonl_sharded(
            "wikipedia",
            "wikipedia",
            (0..4).collect(),
            &local,
            &[peer_ok.clone(), peer_bad.clone()],
            &qwen_embed(),
        )
        .unwrap();
        assert_eq!(handoff.partitions.len(), 2);
        assert!(
            handoff
                .partitions
                .iter()
                .all(|p| p.node_id != peer_bad.node_id),
            "mismatched peer must not receive a partition"
        );
    }

    #[test]
    fn sharded_plan_honors_remaining_subset() {
        // Coordinator has already committed shards 0..37. Remaining =
        // 37..76 should be split across local and peer.
        let local = member(1, Some(qwen_embed()));
        let peer = member(2, Some(qwen_embed()));
        let remaining: Vec<usize> = (37..76).collect();
        let handoff = plan_collaborative_ingestion_jsonl_sharded(
            "wikipedia",
            "wikipedia",
            remaining.clone(),
            &local,
            std::slice::from_ref(&peer),
            &qwen_embed(),
        )
        .unwrap();

        let mut union: Vec<usize> = handoff
            .partitions
            .iter()
            .flat_map(|p| p.file_indices.iter().copied())
            .collect();
        union.sort_unstable();
        assert_eq!(union, remaining);
        // No shard < 37 should appear.
        assert!(union.iter().all(|&i| i >= 37));
    }

    #[test]
    fn sharded_plan_rejects_empty_remaining() {
        let local = member(1, Some(qwen_embed()));
        let err = plan_collaborative_ingestion_jsonl_sharded(
            "wikipedia",
            "wikipedia",
            vec![],
            &local,
            &[],
            &qwen_embed(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CollaborativeIngestionError::AlreadyComplete(_)
        ));
    }

    #[test]
    fn sharded_plan_drops_empty_partitions_when_nodes_exceed_shards() {
        // 2 shards, 3 nodes → only 2 partitions with work.
        let local = member(1, Some(qwen_embed()));
        let p2 = member(2, Some(qwen_embed()));
        let p3 = member(3, Some(qwen_embed()));
        let handoff = plan_collaborative_ingestion_jsonl_sharded(
            "wikipedia",
            "wikipedia",
            vec![0, 1],
            &local,
            &[p2.clone(), p3.clone()],
            &qwen_embed(),
        )
        .unwrap();
        assert_eq!(handoff.partitions.len(), 2);
        for p in &handoff.partitions {
            assert!(!p.file_indices.is_empty());
            assert!(p.article_range.is_none());
        }
    }

    #[test]
    fn jsonl_plan_splits_across_local_and_matching_peer() {
        // Happy path: equal split of the remaining range across two
        // compatible nodes.
        let local = member(1, Some(qwen_embed()));
        let peer = member(2, Some(qwen_embed()));
        let handoff = plan_collaborative_ingestion_jsonl(
            "wikipedia",
            "wikipedia",
            0,
            1000,
            &local,
            std::slice::from_ref(&peer),
            &qwen_embed(),
        )
        .unwrap();
        assert_eq!(handoff.partitions.len(), 2);
        let local_range = handoff
            .partitions
            .iter()
            .find(|p| p.node_id == local.node_id)
            .and_then(|p| p.article_range)
            .unwrap();
        let peer_range = handoff
            .partitions
            .iter()
            .find(|p| p.node_id == peer.node_id)
            .and_then(|p| p.article_range)
            .unwrap();
        // Ranges must cover [0, 1000) with no gap or overlap.
        assert_eq!(local_range.0, 0);
        assert_eq!(local_range.1, peer_range.0);
        assert_eq!(peer_range.1, 1000);
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
