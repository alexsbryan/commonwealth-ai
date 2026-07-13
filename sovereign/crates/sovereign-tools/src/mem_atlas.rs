// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory-pool RAPTOR atlas — the T3 tier of the tiered-retrieval
//! memory port (spec `sovereign/docs/specs/TIERED_RETRIEVAL_MEMORIES.md`).
//!
//! Treats one memory scope (`MemoryScope::General` or
//! `Scoped(<skill>)`) as a corpus: each live memory is a "chunk", the
//! corpus-agnostic [`build_raptor_atlas`] clusters + LLM-summarizes
//! them into a tree, and the nodes persist to `mem_raptor_nodes`
//! keyed by `MemoryScope::atlas_key()`.
//!
//! Sequestration is by construction: the input set comes from
//! `get_all_memories_for_scope`, whose SQL-level wall already excludes
//! other scopes and superseded/deleted rows — a node can never
//! summarize a memory the scope isn't allowed to see.
//!
//! Recall integration lives in
//! `sovereign_core::memory::recall_relevant_memories_embed`: when
//! nodes exist for the scope (and their `embedding_model` matches the
//! live provider), a query that lands near a summary node boosts that
//! node's member leaves — the vocabulary bridge for oblique callbacks
//! ("that night in the spring" → "ongoing grief for their father,
//! hardest through spring" → the March grief memory).

use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::traits::{InferenceProvider, MemoryScope, StateStore};
use sovereign_core::types::{DocumentTypeTag, MemRaptorNodeRow, Memory, RaptorNode};

use crate::raptor_atlas::{build_raptor_atlas_with_leaf_target, ChunkInput};

/// Below this many live memories the tree adds nothing — flat cosine
/// over a dozen rows is already exact — so we skip the LLM cost and
/// clear any stale nodes. `build_raptor_atlas` itself degrades to a
/// flat single root below ~40 items, which is still useful (one
/// summary node bridging the whole pool) between these two bounds.
pub const MIN_MEMORIES_FOR_ATLAS: usize = 12;

/// Leaf-cluster target for the memory pool — far below the document
/// default (20). Memory entries are one-to-two sentences; twenty
/// unrelated ones per cluster wash the summary out to a generic
/// period description with zero discriminating power (measured on
/// the recall probe, 2026-07-08). ~7 keeps each summary thematic.
pub const MEM_LEAF_CLUSTER_TARGET: usize = 7;

/// The two handles a memory-atlas rebuild needs. Installed onto the
/// knowledge-view debouncer after construction (the manager is built
/// from a db PATH, not a store handle, so the store arrives later —
/// same post-Arc installer pattern as `SqliteStateStore::set_observer`).
#[derive(Clone)]
pub struct MemAtlasHandles {
    pub store: Arc<dyn StateStore>,
    pub inference: Arc<dyn InferenceProvider>,
}

/// Rebuild the atlas for EVERY scope present in the pool (General +
/// each skill scope). The Phase-2 production cadence: called from the
/// knowledge-view debouncer's MemoryTouched window, never from the
/// synchronous witness turn. Loading all rows to enumerate scopes is
/// acceptable at debounce cadence; the incremental tree (Phase 3)
/// replaces this whole-pool sweep.
pub async fn rebuild_all_scopes(
    inference: &Arc<dyn InferenceProvider>,
    store: &dyn StateStore,
) -> Result<usize> {
    let mut skill_ids: std::collections::BTreeSet<Option<String>> =
        std::collections::BTreeSet::new();
    for m in store.get_all_memories().await? {
        skill_ids.insert(m.source_skill_id);
    }
    // An empty pool still means General should be (cleared) visited.
    skill_ids.insert(None);
    let mut total = 0usize;
    for skill in skill_ids {
        let scope = match skill {
            None => MemoryScope::General,
            Some(id) => MemoryScope::Scoped(id),
        };
        total += build_memory_atlas(inference, store, &scope).await?;
    }
    Ok(total)
}

/// Build (or rebuild) the RAPTOR atlas for one memory scope. Returns
/// the number of nodes persisted (0 = pool too small / provider can't
/// vouch for embeddings; any prior nodes are cleared so recall never
/// reads a tree that drifted from the pool).
///
/// Reuses stored T1 embeddings where the model matches; batch-embeds
/// the rest (and backfills them, so the next build and the next recall
/// are both O(1) on those rows).
pub async fn build_memory_atlas(
    inference: &Arc<dyn InferenceProvider>,
    store: &dyn StateStore,
    scope: &MemoryScope,
) -> Result<usize> {
    let key = scope.atlas_key();
    let memories = store.get_all_memories_for_scope(scope).await?;

    let embed_model = inference.embed_model_id();
    if memories.len() < MIN_MEMORIES_FOR_ATLAS || embed_model == "unknown" {
        if embed_model == "unknown" && memories.len() >= MIN_MEMORIES_FOR_ATLAS {
            tracing::warn!(
                scope = %key,
                "mem_atlas: provider cannot identify its embed model — \
                 skipping atlas build (recall could never trust the node embeddings)"
            );
        }
        store.delete_mem_raptor_nodes_for_scope(&key).await?;
        return Ok(0);
    }

    let embeddings = resolve_embeddings(inference, store, &memories, &embed_model).await?;

    // Parallel id table: `build_raptor_atlas` speaks u32 chunk ids, the
    // memory pool speaks string ids. chunk_id i ↔ memories[i].id.
    let id_table: Vec<String> = memories.iter().map(|m| m.id.clone()).collect();
    let chunks: Vec<ChunkInput> = memories
        .iter()
        .enumerate()
        .map(|(i, m)| ChunkInput {
            chunk_id: i as u32,
            content: m.content.clone(),
        })
        .collect();

    let nodes = build_raptor_atlas_with_leaf_target(
        inference,
        &chunks,
        &embeddings,
        DocumentTypeTag::Journal,
        MEM_LEAF_CLUSTER_TARGET,
    )
    .await?;
    let rows: Vec<MemRaptorNodeRow> = nodes
        .into_iter()
        .map(|n| translate_node(n, &id_table, &key, &embed_model))
        .collect();

    store.save_mem_raptor_nodes(&key, &rows).await?;
    tracing::info!(
        scope = %key,
        memories = memories.len(),
        nodes = rows.len(),
        "mem_atlas: memory RAPTOR atlas built"
    );
    Ok(rows.len())
}

/// One embedding per memory, index-aligned. Stored T1 vectors are
/// reused when the producing model matches; everything else is
/// batch-embedded through the document-side path (identical to what
/// recall computes) and written back best-effort.
async fn resolve_embeddings(
    inference: &Arc<dyn InferenceProvider>,
    store: &dyn StateStore,
    memories: &[Memory],
    embed_model: &str,
) -> Result<Vec<Vec<f32>>> {
    let mut embs: Vec<Option<Vec<f32>>> = memories
        .iter()
        .map(|m| {
            (m.embedding_model.as_deref() == Some(embed_model))
                .then(|| m.embedding.clone())
                .flatten()
        })
        .collect();

    let missing_idx: Vec<usize> = embs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.is_none().then_some(i))
        .collect();
    if !missing_idx.is_empty() {
        let texts: Vec<String> = missing_idx
            .iter()
            .map(|&i| memories[i].content.clone())
            .collect();
        let fresh = inference.embed_batch(&texts).await?;
        if fresh.len() != texts.len() {
            return Err(sovereign_core::error::Error::Inference(format!(
                "mem_atlas: embed_batch returned {} vectors for {} texts",
                fresh.len(),
                texts.len()
            )));
        }
        for (&i, emb) in missing_idx.iter().zip(fresh) {
            if let Err(e) = store
                .update_memory_embedding(&memories[i].id, &emb, embed_model)
                .await
            {
                tracing::debug!(
                    id = %memories[i].id,
                    error = %e,
                    "mem_atlas: embedding backfill write failed"
                );
            }
            embs[i] = Some(emb);
        }
    }
    Ok(embs.into_iter().map(|e| e.unwrap_or_default()).collect())
}

/// u32 chunk ids → memory id strings, plus the scope/model stamps.
/// Out-of-range ids (impossible unless the builder misbehaves) are
/// dropped rather than silently mapped to a wrong memory.
fn translate_node(
    node: RaptorNode,
    id_table: &[String],
    scope_key: &str,
    embed_model: &str,
) -> MemRaptorNodeRow {
    let to_ids = |chunk_ids: &[u32]| -> Vec<String> {
        chunk_ids
            .iter()
            .filter_map(|&c| id_table.get(c as usize).cloned())
            .collect()
    };
    MemRaptorNodeRow {
        node_id: node.node_id,
        scope: scope_key.to_string(),
        level: node.level,
        summary: node.summary,
        summary_embedding: node.summary_embedding,
        centroid_embedding: node.centroid_embedding,
        children_node_ids: node.children_node_ids,
        direct_member_memory_ids: to_ids(&node.direct_member_chunk_ids),
        evidence_memory_ids: to_ids(&node.evidence_chunk_ids),
        primary_entities: node.primary_entities,
        cluster_coherence: node.cluster_coherence,
        embedding_model: embed_model.to_string(),
        created_at: node.created_at.timestamp(),
        // Incremental-tree state starts blank on batch rows; `mem_tree`
        // initialises CF lazily from member embeddings on first touch.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_with_members(direct: Vec<u32>, evidence: Vec<u32>) -> RaptorNode {
        RaptorNode {
            node_id: "n1".into(),
            level: 0,
            summary: "s".into(),
            summary_embedding: vec![0.1],
            centroid_embedding: vec![0.2],
            children_node_ids: vec![],
            direct_member_chunk_ids: direct,
            evidence_chunk_ids: evidence,
            quote_spans: vec![],
            primary_entities: vec!["A".into()],
            cluster_coherence: 0.9,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn translate_maps_u32_ids_through_the_table() {
        let table = vec!["mem-a".to_string(), "mem-b".to_string()];
        let row = translate_node(
            node_with_members(vec![1, 0], vec![0, 1]),
            &table,
            "mem:inner-work",
            "test-embedder",
        );
        assert_eq!(row.direct_member_memory_ids, vec!["mem-b", "mem-a"]);
        assert_eq!(row.evidence_memory_ids, vec!["mem-a", "mem-b"]);
        assert_eq!(row.scope, "mem:inner-work");
        assert_eq!(row.embedding_model, "test-embedder");
    }

    #[test]
    fn translate_drops_out_of_range_ids_instead_of_misattributing() {
        let table = vec!["mem-a".to_string()];
        let row = translate_node(
            node_with_members(vec![0, 7], vec![7]),
            &table,
            "mem:general",
            "m",
        );
        assert_eq!(row.direct_member_memory_ids, vec!["mem-a"]);
        assert!(row.evidence_memory_ids.is_empty());
    }
}
