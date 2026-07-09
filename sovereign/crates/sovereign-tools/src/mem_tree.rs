// SPDX-License-Identifier: AGPL-3.0-or-later
//! Incremental memory-tree maintenance — Phase 3 of the tiered-
//! retrieval memory port (spec `TIERED_RETRIEVAL_MEMORIES.md`,
//! "The hard part: incremental, not batch").
//!
//! The batch builder (`mem_atlas::build_memory_atlas`) is O(pool) LLM
//! calls per run — fine for a bench seed or the rare full rebuild,
//! unaffordable per new memory on a live stream. This module gives
//! each new memory an O(log N)-ish path instead: MemTree-style
//! top-down descent to the best leaf cluster, then a bounded
//! **trigger ladder** where the eager ops are cheap and the expensive
//! ops are gated:
//!
//! 1. **ATTACH** (no LLM) — absorb into the nearest leaf cluster;
//!    update its BIRCH cluster feature CF = (n, ls, ss) in O(1).
//! 2. **RE-SUMMARIZE** (1 LLM call/node, ≤ DEPTH_CAP nodes) — fire on
//!    a Page-Hinkley drift alarm over the residual signal
//!    dist(new_memory, centroid), OR when new mass since the last
//!    summary reaches DN_RATIO of the cluster. MemGPT recursive
//!    operator: `new = LLM(old_summary + newest members)`, never a
//!    full descendant re-read.
//! 3. **SPLIT** (2 LLM calls, local) — fire when the cluster is
//!    over-grown (member count > TAU_C) or too diffuse (CF radius >
//!    radius_limit). 2-means over member embeddings, two new
//!    siblings replace the node.
//! 4. **BULK-REBUILD** (rare) — when local repairs have degraded the
//!    global shape (root fan-out past FANOUT_REBUILD), rebuild the
//!    whole scope with the batch builder. This is also the bootstrap
//!    path for a pool that just crossed `MIN_MEMORIES_FOR_ATLAS`.
//!
//! **Glassbox:** every insert returns an [`InsertTrace`] naming the op
//! that fired, the metric that crossed which threshold, and the LLM
//! calls spent — the spec's honest position is that *when* to
//! re-summarize is past the published frontier, so the traces exist
//! precisely to tune these knobs against recall-bench outcomes.
//!
//! **Single mutation path:** all tree writes for a scope flow through
//! [`insert_memory`] / [`supersede_memory`] / the batch rebuild —
//! nothing else touches `mem_raptor_nodes`, so the CF/PH state cannot
//! drift from the node contents.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, MemoryScope, StateStore};
use sovereign_core::types::{
    CompletionRequest, MemRaptorNodeRow, Memory, Speed,
};

use crate::mem_atlas::{build_memory_atlas, MIN_MEMORIES_FOR_ATLAS};

// ── Knobs (spec "Starting knobs" — env-overridable, trace-tuned) ──

/// MemTree insertion threshold θ(d) = THETA0 · e^(LAMBDA·d), clamped
/// to 0.95. Deeper nodes demand tighter matches.
const THETA0: f32 = 0.4;
const LAMBDA: f32 = 0.5;
/// Split when a leaf cluster's member count exceeds this (adRAP ~11).
const TAU_C: usize = 11;
/// Re-summarize when new-mass-since-last-summary / cluster size
/// reaches this ratio (adRAP child-change gate, 0.3–0.5).
const DN_RATIO: f64 = 0.4;
/// Page-Hinkley admissible drift δ.
const PH_DELTA: f64 = 0.002;
/// Page-Hinkley alarm threshold λ.
const PH_LAMBDA: f64 = 0.5;
/// Radius head-room: split when CF radius exceeds this multiple of
/// the radius observed when the cluster was last (re)summarized.
const RADIUS_HEADROOM: f32 = 1.6;
/// Absolute floor for the radius limit so tight fresh clusters don't
/// split on the first slightly-off insert.
const RADIUS_FLOOR: f32 = 0.35;
/// Ancestor re-summarize propagation cap (adRAP ~5 levels).
const DEPTH_CAP: usize = 5;
/// Bulk-rebuild when a root's child fan-out degenerates past this.
const FANOUT_REBUILD: usize = 2 * TAU_C;
/// Consolidation gate (Mem0 NOOP / SAGE novelty): a new memory whose
/// nearest neighbour is this similar adds no retrieval value to the
/// tree — leaf cosine already surfaces it — so skip the tree insert.
const CONSOLIDATE_NOOP_COS: f32 = 0.97;

fn theta(depth: usize) -> f32 {
    (THETA0 * (LAMBDA * depth as f32).exp()).min(0.95)
}

// ── Glassbox trace ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeOp {
    /// Pool below build threshold — no tree to maintain yet.
    FlatNoop,
    /// Near-duplicate of an existing memory — tree untouched.
    ConsolidateNoop,
    /// Absorbed into a leaf cluster, CF-only update.
    Attach,
    /// Attached as a NEW singleton cluster (no child cleared θ(d)).
    SpawnCluster,
    /// Node summaries refreshed along the touched path.
    Resummarize,
    /// A leaf cluster split in two.
    Split,
    /// Whole-scope batch rebuild (bootstrap or degeneration).
    BulkRebuild,
    /// Membership updated after a supersede/delete.
    Evict,
}

/// One structured trace per tree mutation. Serialized into bench
/// reports; also emitted via `tracing` at info level.
#[derive(Debug, Clone, Serialize)]
pub struct InsertTrace {
    pub op: TreeOp,
    /// Which metric crossed ("ph_alarm", "dn_ratio", "radius",
    /// "child_count", "fanout", "pool_threshold", "nn_cosine",
    /// "theta") — None for unconditional paths.
    pub metric_crossed: Option<String>,
    pub threshold: f64,
    pub value: f64,
    /// Members of the touched cluster (or nodes for BulkRebuild).
    pub descendant_count: usize,
    pub llm_calls: usize,
}

impl InsertTrace {
    fn emit(self, scope: &str) -> Self {
        tracing::info!(
            scope,
            op = ?self.op,
            metric = self.metric_crossed.as_deref().unwrap_or("-"),
            threshold = self.threshold,
            value = self.value,
            descendants = self.descendant_count,
            llm_calls = self.llm_calls,
            "mem_tree: trigger"
        );
        self
    }
}

// ── CF helpers (BIRCH cluster feature over member embeddings) ────

fn cf_add(node: &mut MemRaptorNodeRow, emb: &[f32]) {
    if node.cf_ls.len() != emb.len() {
        node.cf_ls = vec![0.0; emb.len()];
        node.cf_n = 0;
        node.cf_ss = 0.0;
    }
    for (l, x) in node.cf_ls.iter_mut().zip(emb) {
        *l += x;
    }
    node.cf_ss += emb.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>();
    node.cf_n += 1;
}

/// RMS distance of members to the CF centroid — the BIRCH radius,
/// O(1) from (n, ls, ss).
fn cf_radius(node: &MemRaptorNodeRow) -> f32 {
    if node.cf_n == 0 {
        return 0.0;
    }
    let n = node.cf_n as f64;
    let centroid_sq: f64 = node
        .cf_ls
        .iter()
        .map(|l| {
            let c = *l as f64 / n;
            c * c
        })
        .sum();
    let var = (node.cf_ss / n - centroid_sq).max(0.0);
    var.sqrt() as f32
}

fn cf_centroid(node: &MemRaptorNodeRow) -> Vec<f32> {
    if node.cf_n == 0 {
        return node.centroid_embedding.clone();
    }
    let n = node.cf_n as f32;
    node.cf_ls.iter().map(|l| l / n).collect()
}

/// Initialise CF state for a batch-built node that predates the
/// incremental path (cf_n == 0), from its members' stored embeddings.
fn cf_init_from_members(node: &mut MemRaptorNodeRow, members: &[&Memory]) {
    node.cf_n = 0;
    node.cf_ls.clear();
    node.cf_ss = 0.0;
    for m in members {
        if let Some(e) = &m.embedding {
            cf_add(node, e);
        }
    }
    // The radius at (re)build time anchors the split limit.
    node.radius_at_summary = cf_radius(node);
}

fn radius_limit(node: &MemRaptorNodeRow) -> f32 {
    (node.radius_at_summary * RADIUS_HEADROOM).max(RADIUS_FLOOR)
}

// ── Page-Hinkley drift over residuals dist(new, centroid) ────────

/// Returns true when the alarm fires (and resets the detector).
fn ph_update(node: &mut MemRaptorNodeRow, residual: f64) -> bool {
    let k = (node.n_since_summary + 1) as f64;
    node.ph_mean += (residual - node.ph_mean) / k;
    node.ph_cum += residual - node.ph_mean - PH_DELTA;
    node.ph_min = node.ph_min.min(node.ph_cum);
    if node.ph_cum - node.ph_min > PH_LAMBDA {
        node.ph_mean = 0.0;
        node.ph_cum = 0.0;
        node.ph_min = 0.0;
        true
    } else {
        false
    }
}

// ── Summarization (MemGPT recursive operator) ─────────────────────

/// `new_summary = LLM(old_summary + changed members)` — never a full
/// descendant re-read. Output sanitised to the RaptorNode contract
/// (no `"` characters).
async fn resummarize_node(
    inference: &Arc<dyn InferenceProvider>,
    old_summary: &str,
    changed: &[String],
) -> Result<String> {
    let changed_block = changed
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, c)| format!("[{i}] {}", c.chars().take(280).collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "You maintain a running theme-level summary over a person's private journal \
         entries. Update the existing summary to also cover the new entries — keep what \
         still holds, fold in what changed. 2-4 sentences, paraphrase only, do NOT use \
         quotation marks.\n\nExisting summary:\n{old_summary}\n\nNew entries:\n{changed_block}\n\n\
         Updated summary:"
    );
    let req = CompletionRequest {
        prompt,
        system_message: None,
        preferred_speed: Speed::Slow,
        ..Default::default()
    };
    let resp = inference.complete(&req).await?;
    let cleaned: String = resp
        .text
        .trim()
        .chars()
        .filter(|c| *c != '"')
        .collect();
    if cleaned.is_empty() {
        return Err(Error::Inference(
            "mem_tree: re-summarize returned empty text".to_string(),
        ));
    }
    Ok(cleaned)
}

/// Summary text for a brand-new singleton cluster — content-derived,
/// zero LLM calls (it gets a real summary when growth trips op-2).
fn singleton_summary(content: &str) -> String {
    content
        .chars()
        .filter(|c| *c != '"')
        .take(400)
        .collect()
}

// ── Tree view (in-memory index over the scope's rows) ────────────

struct TreeView {
    nodes: HashMap<String, MemRaptorNodeRow>,
    /// child node_id → parent node_id, derived from children lists so
    /// batch-built rows (which persist no parent pointer) still walk up.
    parent: HashMap<String, String>,
}

impl TreeView {
    fn load(rows: Vec<MemRaptorNodeRow>) -> Self {
        let mut parent = HashMap::new();
        for r in &rows {
            for c in &r.children_node_ids {
                parent.insert(c.clone(), r.node_id.clone());
            }
        }
        // Persisted parent pointers (incremental writes) win over the
        // derived map — they survive a child-list edit in flight.
        for r in &rows {
            if let Some(p) = &r.parent_node_id {
                parent.insert(r.node_id.clone(), p.clone());
            }
        }
        let nodes = rows.into_iter().map(|r| (r.node_id.clone(), r)).collect();
        Self { nodes, parent }
    }

    fn roots(&self) -> Vec<&MemRaptorNodeRow> {
        self.nodes
            .values()
            .filter(|n| !self.parent.contains_key(&n.node_id))
            .collect()
    }

    /// Root-to-node path (inclusive), following parent pointers up.
    fn path_to_root(&self, node_id: &str) -> Vec<String> {
        let mut path = vec![node_id.to_string()];
        let mut cur = node_id.to_string();
        while let Some(p) = self.parent.get(&cur) {
            path.push(p.clone());
            cur = p.clone();
            if path.len() > 32 {
                break; // cycle guard — a corrupt tree must not hang the turn
            }
        }
        path
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

// ── The single mutation path ──────────────────────────────────────

/// Fold one (already persisted) memory into the scope's tree. Returns
/// the glassbox trace of whichever ladder op fired.
///
/// The memory must already be in the store (`save_memory` /
/// `save_with_contradiction_check`) — this maintains the INDEX, never
/// the pool.
pub async fn insert_memory(
    inference: &Arc<dyn InferenceProvider>,
    store: &dyn StateStore,
    scope: &MemoryScope,
    memory: &Memory,
) -> Result<InsertTrace> {
    let key = scope.atlas_key();
    let embed_model = inference.embed_model_id();
    if embed_model == "unknown" {
        return Ok(InsertTrace {
            op: TreeOp::FlatNoop,
            metric_crossed: Some("embed_model_unknown".into()),
            threshold: 0.0,
            value: 0.0,
            descendant_count: 0,
            llm_calls: 0,
        }
        .emit(&key));
    }

    // Scope pool — needed for the consolidation gate, CF init on
    // batch rows, and split. Stored T1 embeddings make this cheap
    // (no embeds); it IS an O(N) read per insert, which is fine at
    // debounce cadence and is exactly what the traces will tell us
    // to optimise if it ever isn't.
    let pool = store.get_all_memories_for_scope(scope).await?;
    let by_id: HashMap<&str, &Memory> = pool.iter().map(|m| (m.id.as_str(), m)).collect();

    // This memory's embedding (stored when the write path computed it,
    // else embed now).
    let mem_emb = match (&memory.embedding, &memory.embedding_model) {
        (Some(e), Some(m)) if m == &embed_model => e.clone(),
        _ => {
            let mut embs = inference
                .embed_batch(std::slice::from_ref(&memory.content))
                .await?;
            let e = embs.pop().unwrap_or_default();
            if e.is_empty() {
                return Err(Error::Inference("mem_tree: empty embedding".into()));
            }
            let _ = store
                .update_memory_embedding(&memory.id, &e, &embed_model)
                .await;
            e
        }
    };

    // Consolidation gate (Mem0 NOOP): a near-duplicate adds no tree
    // value — leaf cosine already surfaces it.
    let mut nn_cos = 0.0f32;
    for m in &pool {
        if m.id == memory.id {
            continue;
        }
        if let (Some(e), Some(model)) = (&m.embedding, &m.embedding_model) {
            if model == &embed_model {
                nn_cos = nn_cos.max(cosine(&mem_emb, e));
            }
        }
    }
    if nn_cos >= CONSOLIDATE_NOOP_COS {
        return Ok(InsertTrace {
            op: TreeOp::ConsolidateNoop,
            metric_crossed: Some("nn_cosine".into()),
            threshold: CONSOLIDATE_NOOP_COS as f64,
            value: nn_cos as f64,
            descendant_count: 0,
            llm_calls: 0,
        }
        .emit(&key));
    }

    let rows = store.list_mem_raptor_nodes(&key).await?;

    // Bootstrap: no tree yet. Below the build threshold recall is
    // exact anyway; at/above it, pay one batch build.
    if rows.is_empty() {
        if pool.len() < MIN_MEMORIES_FOR_ATLAS {
            return Ok(InsertTrace {
                op: TreeOp::FlatNoop,
                metric_crossed: Some("pool_threshold".into()),
                threshold: MIN_MEMORIES_FOR_ATLAS as f64,
                value: pool.len() as f64,
                descendant_count: 0,
                llm_calls: 0,
            }
            .emit(&key));
        }
        let nodes = build_memory_atlas(inference, store, scope).await?;
        return Ok(InsertTrace {
            op: TreeOp::BulkRebuild,
            metric_crossed: Some("pool_threshold".into()),
            threshold: MIN_MEMORIES_FOR_ATLAS as f64,
            value: pool.len() as f64,
            descendant_count: nodes,
            llm_calls: nodes, // ~1 summary call per node
        }
        .emit(&key));
    }

    let mut view = TreeView::load(rows);

    // Degeneration check (op-4): a root whose fan-out blew past the
    // cap means local repairs have stopped preserving shape.
    let worst_fanout = view
        .roots()
        .iter()
        .map(|r| r.children_node_ids.len())
        .chain(std::iter::once(view.roots().len()))
        .max()
        .unwrap_or(0);
    if worst_fanout > FANOUT_REBUILD {
        let nodes = build_memory_atlas(inference, store, scope).await?;
        return Ok(InsertTrace {
            op: TreeOp::BulkRebuild,
            metric_crossed: Some("fanout".into()),
            threshold: FANOUT_REBUILD as f64,
            value: worst_fanout as f64,
            descendant_count: nodes,
            llm_calls: nodes,
        }
        .emit(&key));
    }

    // ── Top-down descent (MemTree): best root, then best child while
    // cosine clears the depth-adaptive threshold θ(d). ──────────────
    let start = view
        .roots()
        .iter()
        .max_by(|a, b| {
            cosine(&mem_emb, &a.centroid_embedding)
                .partial_cmp(&cosine(&mem_emb, &b.centroid_embedding))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|n| n.node_id.clone());
    let Some(mut cur_id) = start else {
        // Rows existed but no root resolved — corrupt tree; rebuild.
        let nodes = build_memory_atlas(inference, store, scope).await?;
        return Ok(InsertTrace {
            op: TreeOp::BulkRebuild,
            metric_crossed: Some("no_root".into()),
            threshold: 0.0,
            value: 0.0,
            descendant_count: nodes,
            llm_calls: nodes,
        }
        .emit(&key));
    };

    let mut depth = 0usize;
    let target_leaf: String = loop {
        let cur = view
            .nodes
            .get(&cur_id)
            .ok_or_else(|| Error::Storage("mem_tree: dangling node id".into()))?;
        if cur.level == 0 {
            break cur_id.clone();
        }
        let best_child = cur
            .children_node_ids
            .iter()
            .filter_map(|c| view.nodes.get(c))
            .map(|c| (cosine(&mem_emb, &cf_centroid(c)), c.node_id.clone()))
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        match best_child {
            Some((sim, child_id)) if sim >= theta(depth) => {
                cur_id = child_id;
                depth += 1;
            }
            _ => {
                // Nothing clears θ(d): spawn a fresh singleton leaf
                // cluster under the current node. No LLM — the
                // summary is content-derived until growth earns a
                // real one.
                let new_id = format!("memtree-{}", uuid::Uuid::new_v4());
                let mut node = MemRaptorNodeRow {
                    node_id: new_id.clone(),
                    scope: key.clone(),
                    level: 0,
                    summary: singleton_summary(&memory.content),
                    summary_embedding: mem_emb.clone(),
                    centroid_embedding: mem_emb.clone(),
                    children_node_ids: vec![],
                    direct_member_memory_ids: vec![memory.id.clone()],
                    evidence_memory_ids: vec![memory.id.clone()],
                    primary_entities: vec![],
                    cluster_coherence: 1.0,
                    embedding_model: embed_model.clone(),
                    created_at: memory.created_at,
                    parent_node_id: Some(cur_id.clone()),
                    ..Default::default()
                };
                cf_add(&mut node, &mem_emb);
                node.radius_at_summary = 0.0;
                store.upsert_mem_raptor_node(&node).await?;
                // Parent gains a child + evidence.
                let parent = view
                    .nodes
                    .get_mut(&cur_id)
                    .expect("cur node present — fetched above");
                parent.children_node_ids.push(new_id);
                parent.evidence_memory_ids.push(memory.id.clone());
                let parent_snapshot = parent.clone();
                store.upsert_mem_raptor_node(&parent_snapshot).await?;
                // Evidence propagates the rest of the way up.
                for anc_id in view.path_to_root(&cur_id).into_iter().skip(1) {
                    if let Some(anc) = view.nodes.get_mut(&anc_id) {
                        anc.evidence_memory_ids.push(memory.id.clone());
                        let snap = anc.clone();
                        store.upsert_mem_raptor_node(&snap).await?;
                    }
                }
                let best_sim = best_child_sim(&view, &cur_id, &mem_emb);
                return Ok(InsertTrace {
                    op: TreeOp::SpawnCluster,
                    metric_crossed: Some("theta".into()),
                    threshold: theta(depth) as f64,
                    value: best_sim as f64,
                    descendant_count: 1,
                    llm_calls: 0,
                }
                .emit(&key));
            }
        }
    };

    // ── Attach to the leaf cluster + trigger ladder ───────────────
    let leaf = view
        .nodes
        .get_mut(&target_leaf)
        .expect("leaf resolved by descent");

    // CF init for batch-built rows that predate incremental state.
    if leaf.cf_n == 0 && !leaf.direct_member_memory_ids.is_empty() {
        let members: Vec<&Memory> = leaf
            .direct_member_memory_ids
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .collect();
        cf_init_from_members(leaf, &members);
    }

    leaf.direct_member_memory_ids.push(memory.id.clone());
    leaf.evidence_memory_ids.push(memory.id.clone());
    let residual = {
        let c = cf_centroid(leaf);
        let sim = cosine(&mem_emb, &c);
        (1.0 - sim) as f64
    };
    cf_add(leaf, &mem_emb);
    leaf.n_since_summary += 1;
    let ph_alarm = ph_update(leaf, residual);

    let member_count = leaf.direct_member_memory_ids.len();
    let radius = cf_radius(leaf);
    let rlimit = radius_limit(leaf);
    let dn = leaf.n_since_summary as f64 / (leaf.cf_n.max(1)) as f64;

    // Op-3 SPLIT — checked before re-summarize: an over-grown or
    // diffuse cluster gets fresh summaries as part of the split, so
    // summarizing first would double-pay.
    if member_count > TAU_C || radius > rlimit {
        let (metric, threshold, value) = if member_count > TAU_C {
            ("child_count", TAU_C as f64, member_count as f64)
        } else {
            ("radius", rlimit as f64, radius as f64)
        };
        let leaf_snapshot = leaf.clone();
        let llm_calls = split_leaf(
            inference,
            store,
            &mut view,
            &key,
            leaf_snapshot,
            &by_id,
            (&memory.id, &mem_emb),
            &embed_model,
        )
        .await?;
        return Ok(InsertTrace {
            op: TreeOp::Split,
            metric_crossed: Some(metric.into()),
            threshold,
            value,
            descendant_count: member_count,
            llm_calls,
        }
        .emit(&key));
    }

    // Op-2 RE-SUMMARIZE — PH alarm or ΔN/N gate.
    if ph_alarm || dn >= DN_RATIO {
        let (metric, threshold, value) = if ph_alarm {
            ("ph_alarm", PH_LAMBDA, residual)
        } else {
            ("dn_ratio", DN_RATIO, dn)
        };
        // Refresh this node from its newest members, then walk up.
        let newest: Vec<String> = leaf
            .direct_member_memory_ids
            .iter()
            .rev()
            .take(6)
            .filter_map(|id| by_id.get(id.as_str()).map(|m| m.content.clone()))
            .chain(std::iter::once(memory.content.clone()))
            .collect();
        let old = leaf.summary.clone();
        let mut llm_calls = 0usize;
        match resummarize_node(inference, &old, &newest).await {
            Ok(new_summary) => {
                llm_calls += 1;
                leaf.summary = new_summary;
                if let Ok(mut e) = inference
                    .embed_batch(std::slice::from_ref(&leaf.summary))
                    .await
                {
                    if let Some(e) = e.pop() {
                        leaf.summary_embedding = e;
                    }
                }
                leaf.centroid_embedding = cf_centroid(leaf);
                leaf.n_since_summary = 0;
                leaf.radius_at_summary = cf_radius(leaf);
            }
            Err(e) => {
                tracing::warn!(error = %e, "mem_tree: leaf re-summarize failed — keeping old summary");
            }
        }
        let leaf_snapshot = leaf.clone();
        store.upsert_mem_raptor_node(&leaf_snapshot).await?;

        // Propagate up ≤ DEPTH_CAP: each ancestor folds the changed
        // child summary into its own (adRAP bounded propagation).
        let mut changed_child_summary = leaf_snapshot.summary.clone();
        for anc_id in view
            .path_to_root(&target_leaf)
            .into_iter()
            .skip(1)
            .take(DEPTH_CAP)
        {
            let Some(anc) = view.nodes.get_mut(&anc_id) else { break };
            anc.evidence_memory_ids.push(memory.id.clone());
            anc.n_since_summary += 1;
            if llm_calls > 0 {
                match resummarize_node(
                    inference,
                    &anc.summary,
                    std::slice::from_ref(&changed_child_summary),
                )
                .await
                {
                    Ok(s) => {
                        llm_calls += 1;
                        anc.summary = s.clone();
                        if let Ok(mut e) = inference
                            .embed_batch(std::slice::from_ref(&anc.summary))
                            .await
                        {
                            if let Some(e) = e.pop() {
                                anc.summary_embedding = e;
                            }
                        }
                        anc.n_since_summary = 0;
                        changed_child_summary = s;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "mem_tree: ancestor re-summarize failed");
                    }
                }
            }
            let snap = anc.clone();
            store.upsert_mem_raptor_node(&snap).await?;
        }
        return Ok(InsertTrace {
            op: TreeOp::Resummarize,
            metric_crossed: Some(metric.into()),
            threshold,
            value,
            descendant_count: member_count,
            llm_calls,
        }
        .emit(&key));
    }

    // Op-1 ATTACH — the common, LLM-free path.
    let leaf_snapshot = leaf.clone();
    store.upsert_mem_raptor_node(&leaf_snapshot).await?;
    for anc_id in view.path_to_root(&target_leaf).into_iter().skip(1) {
        if let Some(anc) = view.nodes.get_mut(&anc_id) {
            anc.evidence_memory_ids.push(memory.id.clone());
            anc.n_since_summary += 1;
            let snap = anc.clone();
            store.upsert_mem_raptor_node(&snap).await?;
        }
    }
    Ok(InsertTrace {
        op: TreeOp::Attach,
        metric_crossed: None,
        threshold: rlimit as f64,
        value: radius as f64,
        descendant_count: member_count,
        llm_calls: 0,
    }
    .emit(&key))
}

fn best_child_sim(view: &TreeView, node_id: &str, emb: &[f32]) -> f32 {
    view.nodes
        .get(node_id)
        .map(|n| {
            n.children_node_ids
                .iter()
                .filter_map(|c| view.nodes.get(c))
                .map(|c| cosine(emb, &cf_centroid(c)))
                .fold(0.0f32, f32::max)
        })
        .unwrap_or(0.0)
}

/// Op-3 body: 2-means over the leaf's member embeddings; two new
/// sibling clusters replace the node under its parent. Each new
/// cluster gets one fresh summary (2 LLM calls total).
#[allow(clippy::too_many_arguments)]
async fn split_leaf(
    inference: &Arc<dyn InferenceProvider>,
    store: &dyn StateStore,
    view: &mut TreeView,
    scope_key: &str,
    leaf: MemRaptorNodeRow,
    by_id: &HashMap<&str, &Memory>,
    inserted: (&str, &[f32]),
    embed_model: &str,
) -> Result<usize> {
    // Gather (id, embedding) for every member; the just-inserted
    // memory may not be in `by_id`'s stored-embedding form yet.
    let mut members: Vec<(String, Vec<f32>)> = Vec::new();
    for id in &leaf.direct_member_memory_ids {
        if id == inserted.0 {
            members.push((id.clone(), inserted.1.to_vec()));
        } else if let Some(m) = by_id.get(id.as_str()) {
            if let Some(e) = &m.embedding {
                members.push((id.clone(), e.clone()));
            }
        }
    }
    if members.len() < 2 {
        // Nothing to split — persist the grown leaf as-is.
        store.upsert_mem_raptor_node(&leaf).await?;
        return Ok(0);
    }

    // 2-means, seeded by the two most-distant members (deterministic —
    // kmeans random init would make bench runs unrepeatable).
    let (mut ca, mut cb) = {
        let mut worst = (0usize, 1usize, f32::MAX);
        for i in 0..members.len() {
            for j in (i + 1)..members.len() {
                let sim = cosine(&members[i].1, &members[j].1);
                if sim < worst.2 {
                    worst = (i, j, sim);
                }
            }
        }
        (members[worst.0].1.clone(), members[worst.1].1.clone())
    };
    let mut assign = vec![0u8; members.len()];
    for _ in 0..8 {
        for (i, (_, e)) in members.iter().enumerate() {
            assign[i] = if cosine(e, &ca) >= cosine(e, &cb) { 0 } else { 1 };
        }
        let mean = |side: u8| -> Vec<f32> {
            let sel: Vec<&Vec<f32>> = members
                .iter()
                .zip(&assign)
                .filter(|(_, a)| **a == side)
                .map(|((_, e), _)| e)
                .collect();
            if sel.is_empty() {
                return vec![0.0; members[0].1.len()];
            }
            let mut m = vec![0.0f32; sel[0].len()];
            for e in &sel {
                for (x, y) in m.iter_mut().zip(e.iter()) {
                    *x += y;
                }
            }
            for x in &mut m {
                *x /= sel.len() as f32;
            }
            m
        };
        ca = mean(0);
        cb = mean(1);
    }
    // Degenerate split (all one side) → keep as attach.
    if !assign.iter().any(|a| *a == 0) || !assign.iter().any(|a| *a == 1) {
        store.upsert_mem_raptor_node(&leaf).await?;
        return Ok(0);
    }

    let mut llm_calls = 0usize;
    let mut new_ids = Vec::new();
    for side in [0u8, 1u8] {
        let ids: Vec<String> = members
            .iter()
            .zip(&assign)
            .filter(|(_, a)| **a == side)
            .map(|((id, _), _)| id.clone())
            .collect();
        let contents: Vec<String> = ids
            .iter()
            .filter_map(|id| {
                if id == inserted.0 {
                    by_id.get(id.as_str()).map(|m| m.content.clone()).or(None)
                } else {
                    by_id.get(id.as_str()).map(|m| m.content.clone())
                }
            })
            .collect();
        let summary = match resummarize_node(
            inference,
            "(new cluster after split)",
            &contents,
        )
        .await
        {
            Ok(s) => {
                llm_calls += 1;
                s
            }
            Err(_) => singleton_summary(contents.first().map(|s| s.as_str()).unwrap_or("")),
        };
        let summary_embedding = inference
            .embed_batch(std::slice::from_ref(&summary))
            .await
            .ok()
            .and_then(|mut v| v.pop())
            .unwrap_or_else(|| leaf.summary_embedding.clone());
        let mut node = MemRaptorNodeRow {
            node_id: format!("memtree-{}", uuid::Uuid::new_v4()),
            scope: scope_key.to_string(),
            level: 0,
            summary,
            summary_embedding,
            centroid_embedding: vec![],
            children_node_ids: vec![],
            direct_member_memory_ids: ids.clone(),
            evidence_memory_ids: ids.clone(),
            primary_entities: leaf.primary_entities.clone(),
            cluster_coherence: leaf.cluster_coherence,
            embedding_model: embed_model.to_string(),
            created_at: leaf.created_at,
            parent_node_id: leaf.parent_node_id.clone(),
            ..Default::default()
        };
        for id in &ids {
            let emb = if id == inserted.0 {
                Some(inserted.1.to_vec())
            } else {
                by_id.get(id.as_str()).and_then(|m| m.embedding.clone())
            };
            if let Some(e) = emb {
                cf_add(&mut node, &e);
            }
        }
        node.centroid_embedding = cf_centroid(&node);
        node.radius_at_summary = cf_radius(&node);
        store.upsert_mem_raptor_node(&node).await?;
        new_ids.push(node.node_id.clone());
    }

    // Rewire the parent (or leave the two as roots when the split
    // node WAS a root).
    if let Some(pid) = leaf
        .parent_node_id
        .clone()
        .or_else(|| view.parent.get(&leaf.node_id).cloned())
    {
        if let Some(parent) = view.nodes.get_mut(&pid) {
            parent.children_node_ids.retain(|c| c != &leaf.node_id);
            parent.children_node_ids.extend(new_ids.clone());
            let snap = parent.clone();
            store.upsert_mem_raptor_node(&snap).await?;
        }
    }
    store.delete_mem_raptor_node(&leaf.node_id).await?;
    Ok(llm_calls)
}

/// Remove a superseded/deleted memory from the tree. Compaction is
/// just another stream event: membership shrinks, CF decrements
/// (approximately — the exact vector may be gone), and an emptied
/// cluster leaves the tree.
pub async fn supersede_memory(
    store: &dyn StateStore,
    scope: &MemoryScope,
    memory_id: &str,
) -> Result<InsertTrace> {
    let key = scope.atlas_key();
    let rows = store.list_mem_raptor_nodes(&key).await?;
    let mut view = TreeView::load(rows);

    let holder = view
        .nodes
        .values()
        .find(|n| n.level == 0 && n.direct_member_memory_ids.iter().any(|m| m == memory_id))
        .map(|n| n.node_id.clone());
    let Some(leaf_id) = holder else {
        return Ok(InsertTrace {
            op: TreeOp::Evict,
            metric_crossed: Some("not_in_tree".into()),
            threshold: 0.0,
            value: 0.0,
            descendant_count: 0,
            llm_calls: 0,
        }
        .emit(&key));
    };

    let path = view.path_to_root(&leaf_id);
    let leaf = view.nodes.get_mut(&leaf_id).expect("holder found above");
    leaf.direct_member_memory_ids.retain(|m| m != memory_id);
    leaf.evidence_memory_ids.retain(|m| m != memory_id);
    // Approximate CF decrement: scale the linear/square sums down by
    // the member ratio (the evicted vector may no longer be loadable).
    if leaf.cf_n > 1 {
        let ratio = (leaf.cf_n - 1) as f32 / leaf.cf_n as f32;
        for l in &mut leaf.cf_ls {
            *l *= ratio;
        }
        leaf.cf_ss *= ratio as f64;
        leaf.cf_n -= 1;
    } else {
        leaf.cf_n = 0;
        leaf.cf_ls.clear();
        leaf.cf_ss = 0.0;
    }
    let emptied = leaf.direct_member_memory_ids.is_empty();
    let remaining = leaf.direct_member_memory_ids.len();
    let leaf_snapshot = leaf.clone();

    if emptied {
        store.delete_mem_raptor_node(&leaf_id).await?;
    } else {
        store.upsert_mem_raptor_node(&leaf_snapshot).await?;
    }
    for anc_id in path.into_iter().skip(1) {
        if let Some(anc) = view.nodes.get_mut(&anc_id) {
            anc.evidence_memory_ids.retain(|m| m != memory_id);
            if emptied {
                anc.children_node_ids.retain(|c| c != &leaf_id);
            }
            let snap = anc.clone();
            store.upsert_mem_raptor_node(&snap).await?;
        }
    }
    Ok(InsertTrace {
        op: TreeOp::Evict,
        metric_crossed: None,
        threshold: 0.0,
        value: 0.0,
        descendant_count: remaining,
        llm_calls: 0,
    }
    .emit(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theta_grows_with_depth_and_clamps() {
        assert!((theta(0) - 0.4).abs() < 1e-6);
        assert!(theta(1) > theta(0));
        assert_eq!(theta(4), 0.95);
    }

    #[test]
    fn cf_radius_zero_for_identical_members() {
        let mut n = MemRaptorNodeRow::default();
        cf_add(&mut n, &[1.0, 0.0]);
        cf_add(&mut n, &[1.0, 0.0]);
        assert!(cf_radius(&n) < 1e-4);
        assert_eq!(n.cf_n, 2);
    }

    #[test]
    fn cf_radius_positive_for_spread_members() {
        let mut n = MemRaptorNodeRow::default();
        cf_add(&mut n, &[1.0, 0.0]);
        cf_add(&mut n, &[0.0, 1.0]);
        assert!(cf_radius(&n) > 0.4);
        let c = cf_centroid(&n);
        assert!((c[0] - 0.5).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn ph_alarm_fires_on_sustained_drift() {
        let mut n = MemRaptorNodeRow::default();
        // Stable residuals: no alarm.
        let mut fired = false;
        for _ in 0..50 {
            n.n_since_summary += 1;
            fired |= ph_update(&mut n, 0.1);
        }
        assert!(!fired, "stable stream must not alarm");
        // Sustained upward shift: alarm eventually fires.
        let mut fired = false;
        for _ in 0..200 {
            n.n_since_summary += 1;
            if ph_update(&mut n, 0.9) {
                fired = true;
                break;
            }
        }
        assert!(fired, "sustained drift must alarm");
    }

    #[test]
    fn tree_view_derives_parents_from_children_lists() {
        let mut root = MemRaptorNodeRow::default();
        root.node_id = "r".into();
        root.level = 1;
        root.children_node_ids = vec!["a".into(), "b".into()];
        let mut a = MemRaptorNodeRow::default();
        a.node_id = "a".into();
        let mut b = MemRaptorNodeRow::default();
        b.node_id = "b".into();
        let view = TreeView::load(vec![root, a, b]);
        assert_eq!(view.roots().len(), 1);
        assert_eq!(view.path_to_root("a"), vec!["a".to_string(), "r".to_string()]);
    }

    #[test]
    fn singleton_summary_strips_quotes() {
        assert_eq!(singleton_summary(r#"said "hi" there"#), "said hi there");
    }
}
