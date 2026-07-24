// SPDX-License-Identifier: AGPL-3.0-or-later
//! RAPTOR atlas — cluster-summarize-recurse tree replacing per-chunk
//! LLM skeleton extraction.
//!
//! Pipeline at a glance:
//!
//! 1. **K-means** clusters the chunk embeddings into ~50 leaf groups
//!    (target avg ~20 chunks per cluster, k = max(2, n/20)).
//! 2. For each leaf cluster:
//!    - Pick 3-5 verbatim **quote spans** from member chunks (sentences
//!      with highest cosine similarity to the cluster centroid).
//!    - One Slow-slot LLM call produces a paraphrase **summary** and a
//!      list of `primary_entities`. Grammar forbids `"` in the summary
//!      so the hallucination-detector contract holds downstream.
//! 3. Embed the resulting summaries. Cluster them. Summarize each new
//!    cluster. Recurse until the level has ≤4 nodes (target log₂(n)
//!    mid-level node count for adaptive depth).
//! 4. Each node carries:
//!    - `summary` + `summary_embedding` (paraphrase, query-matchable)
//!    - `centroid_embedding` (input-space centroid, for incremental
//!      re-scoring later)
//!    - `direct_member_chunk_ids` (leaves only)
//!    - `evidence_chunk_ids` (transitive union over the subtree —
//!      what to fetch for verbatim quotation)
//!    - `quote_spans` (verbatim, hallucination-safe)
//!    - `cluster_coherence` (mean cosine to centroid, in [0,1])
//!
//! The LLM dispatch uses `futures::stream::iter(...).buffered(N)` so the
//! mesh load balancer can fan summaries across peers. Single-machine
//! ingest serializes through one Slow slot but still benefits from
//! pipelined async work.

use std::sync::Arc;

use futures::stream::{self, StreamExt};
use sovereign_core::error::Result;
use sovereign_core::slot_policy::Workload;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::*;

use crate::raptor_checkpoint::{CheckpointDecision, LevelClustering, RaptorCheckpointHandle};
use corpus_engine::enrichment::state::{EnrichmentPhase, EnrichmentProgressSink};

/// Target average number of input items per leaf cluster. With 1006
/// Conrad chunks this produces ~50 leaf clusters; with 200 chunks
/// it produces ~10. Below ~40 items we drop to a flat tree (single
/// root) rather than force artificial structure.
const LEAF_TARGET_CLUSTER_SIZE: usize = 20;

/// Target fan-out for non-leaf levels. Smaller than the leaf cluster
/// size so the tree builds the right shape: 1006 chunks → ~50 leaves
/// → ~10 mid-level → 2-3 root. The first bench run (2026-05-22)
/// surfaced the failure mode this fixes: using LEAF_TARGET_CLUSTER_SIZE
/// at every level produced 50→3 in one step, skipping the scene-scale
/// mid-level the briefing wants to surface.
const NONLEAF_TARGET_FANOUT: usize = 5;

/// Maximum branching at any non-root level. Recursion stops once a
/// level fits under this — that level becomes the root layer (1-4
/// nodes) and we don't summarize over it.
const ROOT_BRANCHING_CEILING: usize = 4;

/// Concurrency for the summary calls. On a mesh the load balancer
/// spreads these across peers; on a solo node this is how many
/// summaries ride one continuous-batched decode together.
///
/// **8 because that is the batching lane's width.** The FastShort slot
/// is built with `n_seq_max=8` (`ModelSlot::from_existing_model`), so a
/// 9th concurrent call cannot join the batch — it waits for a seq slot
/// to retire. Raising this past 8 buys nothing and costs held memory
/// and queue depth; it was briefly 12 during the 2026-07-24 tuning arc
/// before that bound was checked. Lowering it below 8 leaves the lane
/// half-empty, which is what the original 6 did: the bench's per-call
/// log showed 15 leaf summaries dispatching in three visible waves as
/// slots freed, a 53.6s leaf level made of calls whose own median
/// latency was 7.7s.
///
/// If the batching lane is unavailable — `SOVEREIGN_FAST_SHORT_DISABLE`,
/// a vetoed arch (see `fast_short_gate`), or any host that serves these
/// on a single-sequence slot — the calls simply queue and this constant
/// stops mattering. It is a ceiling, never an assumption of parallelism.
const SUMMARIZE_BUFFER: usize = 8;

/// Hard cap on how many member previews are shown to the leaf
/// summarizer.
///
/// This bounds *the prompt*, not the node. Every member still lands in
/// `direct_member_chunk_ids` / `evidence_chunk_ids`, and `quote_spans`
/// are still extracted across all members — the retrieval-facing and
/// grounding-facing surfaces are untouched, which is the invariant that
/// matters (quote spans must stay verbatim-true; see the module docs).
///
/// **13 is sized against the batching lane's input gate, not against
/// any one machine's throughput.** A request whose prompt exceeds
/// `FAST_SHORT_MAX_INPUT_CHARS` (6000) is refused by the batched
/// FastShort lane and served on the single-sequence Fast slot instead.
/// 13 previews × 280 chars ≈ 3.6k, plus ~600 chars of instructions and
/// doc-type cue, leaves comfortable headroom under that gate. Budget
/// arithmetic to redo if either constant moves:
/// `MAX × preview_len + prompt_overhead < FAST_SHORT_MAX_INPUT_CHARS`.
///
/// What went wrong without it (2026-07-24): k-means gives a nominal 20
/// members per leaf but a real range of 1–65, so the biggest clusters
/// built ~18k-char prompts — 3× over the gate. Those leaves silently
/// fell off the batched lane and serialized one at a time, which is why
/// the leaf level's stragglers ran 30–45s while its small clusters
/// finished in 4–7s. Capping the prompt is what puts every leaf back on
/// the same lane; the prefill saving is the smaller half of the win.
///
/// Quality rationale for capping rather than shrinking `preview()`: a
/// 65-preview prompt is not 5× more informative than a 13-preview one.
/// Each preview is already clipped to 280 chars, so a huge cluster
/// contributes a long tail of near-duplicate fragments that wash the
/// summary out rather than sharpen it. Members are ranked by cosine to
/// the centroid, so what survives is the cluster's core, not a prefix.
const MAX_MEMBERS_IN_SUMMARY_PROMPT: usize = 13;

/// Hard cap on quote spans per node — keeps briefing budget bounded
/// even for very tight clusters where many sentences match the
/// centroid.
const MAX_QUOTE_SPANS_PER_NODE: usize = 5;

/// Minimum verbatim-span length (in chars) before we consider it a
/// useful quotable signpost. Short spans don't anchor much.
const MIN_QUOTE_SPAN_CHARS: usize = 40;

/// Legacy entry point — no checkpoint, no progress sink. Forwards to
/// [`build_raptor_atlas_with_checkpoint`] so call sites that don't
/// need resume semantics keep working unchanged.
pub async fn build_raptor_atlas(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
) -> Result<Vec<RaptorNode>> {
    build_raptor_atlas_with_checkpoint(inference, chunks, embeddings, doc_type, None, None, None)
        .await
}

/// Variant with a caller-chosen leaf-cluster target size. The default
/// (20) is tuned for document chunks (~1-2k chars each); memory-pool
/// entries are one-to-two sentences, and 20 of them per cluster
/// washes the summary out to a generic period description — measured
/// on the inner-chaos recall probe (2026-07-08): every leaf summary
/// converged to "this period captures significant personal…" and the
/// tier boost had zero discriminating power. Memory callers pass ~7
/// so each summary stays thematically specific.
pub async fn build_raptor_atlas_with_leaf_target(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    leaf_target: usize,
) -> Result<Vec<RaptorNode>> {
    build_raptor_atlas_impl(
        inference,
        chunks,
        embeddings,
        doc_type,
        None,
        None,
        None,
        leaf_target.max(2),
    )
    .await
}

/// Build the full RAPTOR atlas from a document's chunks + their
/// pre-computed embeddings. Returns the flat node list (leaves first,
/// then intermediate, then root) ready for `save_raptor_nodes`.
///
/// `chunks` and `embeddings` must be the same length and indexed
/// pari passu. Caller is responsible for chunk-id assignment via
/// `chunk.index`.
///
/// ## Checkpoint
///
/// When `checkpoint` is `Some(handle)`:
///   - On entry, a `completed` manifest short-circuits the build and
///     returns the previously-built nodes.
///   - At level 0, the clustering decision is persisted before any
///     LLM call. On a restart, the same clustering is reloaded so
///     cluster identity is stable across attempts.
///   - Each per-cluster `RaptorNode` is written immediately after the
///     LLM returns. On restart, cached nodes are loaded from disk and
///     the LLM call is skipped — the difference between hours of
///     re-work and minutes of catch-up.
///
/// When `progress` is `Some(sink)`:
///   - The sink receives `RaptorLeaves(done / total)` ticks after
///     every per-leaf completion (cached or freshly summarized) so
///     the desktop chip moves under the user's eyes.
///
/// Tree-level (non-leaf) recursion is NOT checkpointed today — those
/// passes are short (≤ 10 calls total) and re-doing them is cheap
/// compared to the leaves dominating wall time.
pub async fn build_raptor_atlas_with_checkpoint(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    checkpoint: Option<&RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn EnrichmentProgressSink>>,
    // Optional user-authored correction (the "flag a wrong summary"
    // revision loop) injected into every cluster's summarization prompt.
    correction_hint: Option<&str>,
) -> Result<Vec<RaptorNode>> {
    build_raptor_atlas_impl(
        inference,
        chunks,
        embeddings,
        doc_type,
        checkpoint,
        progress,
        correction_hint,
        LEAF_TARGET_CLUSTER_SIZE,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_raptor_atlas_impl(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    checkpoint: Option<&RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn EnrichmentProgressSink>>,
    // Note-level correction hint, re-applied at every RAPTOR tree level
    // (rides on each `ClusterSummarizationInput`).
    correction_hint: Option<&str>,
    leaf_target: usize,
) -> Result<Vec<RaptorNode>> {
    if chunks.len() != embeddings.len() {
        return Err(sovereign_core::error::Error::Storage(format!(
            "build_raptor_atlas: chunks.len()={} but embeddings.len()={}",
            chunks.len(),
            embeddings.len()
        )));
    }
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    // ── Checkpoint decide ────────────────────────────────────
    //
    // If a previous attempt finished, return its nodes verbatim — no
    // LLM calls. If it crashed mid-build, the clustering + per-leaf
    // RaptorNodes already on disk feed back in below, and only the
    // missing leaves get summarized this attempt.
    if let Some(handle) = checkpoint {
        match handle.decide() {
            CheckpointDecision::Resume(ref manifest) if manifest.completed_at.is_some() => {
                let cached = handle.load_all_nodes()?;
                tracing::info!(
                    cached_nodes = cached.len(),
                    "raptor_atlas: completed checkpoint found; skipping LLM build"
                );
                return Ok(cached);
            }
            CheckpointDecision::StaleAndReset => {
                tracing::info!(
                    "raptor_atlas: input hash changed since last attempt; \
                     wiping checkpoint and starting fresh"
                );
                handle.reset();
                let _ = handle.ensure_manifest();
            }
            CheckpointDecision::Resume(_) => {
                tracing::info!(
                    "raptor_atlas: resuming partial checkpoint — clustering + \
                     completed per-leaf nodes will be reused"
                );
                let _ = handle.ensure_manifest();
            }
            CheckpointDecision::Fresh => {
                let _ = handle.ensure_manifest();
            }
        }
    }

    // ── Level 0 — cluster raw chunks ─────────────────────────
    //
    // Clustering is persisted on first run so subsequent attempts see
    // the same cluster→member mapping. Without this, kmeans's random
    // init produces different clusters on retry and the cached
    // per-cluster nodes would no longer match the live cluster
    // identities.
    let (k_leaves, leaf_assignments) =
        match checkpoint.and_then(|h| h.read_clustering(0).ok().flatten()) {
            Some(c) => {
                tracing::debug!(
                    level = 0,
                    k = c.k,
                    "raptor_atlas: reusing persisted clustering"
                );
                (
                    c.k as usize,
                    c.assignments.into_iter().map(|a| a as usize).collect(),
                )
            }
            None => {
                let k = target_k(chunks.len(), leaf_target);
                let assignments = kmeans_cluster(embeddings, k, /* max_iters = */ 40);
                if let Some(handle) = checkpoint {
                    let record = LevelClustering {
                        k: k as u32,
                        assignments: assignments.iter().map(|a| *a as u32).collect(),
                    };
                    if let Err(e) = handle.write_clustering(0, &record) {
                        tracing::warn!(
                            error = %e,
                            "raptor_atlas: persist clustering failed; retry won't be deterministic"
                        );
                    }
                }
                (k, assignments)
            }
        };

    let mut leaf_inputs: Vec<LeafSummarizationInput> = Vec::with_capacity(k_leaves);
    for cluster_idx in 0..k_leaves {
        let member_indices: Vec<usize> = leaf_assignments
            .iter()
            .enumerate()
            .filter_map(|(i, &c)| (c == cluster_idx).then_some(i))
            .collect();
        if member_indices.is_empty() {
            // K-means can produce empty clusters on rare init collisions;
            // skip them — k effectively shrinks to occupied clusters.
            continue;
        }
        let centroid = mean_vector(
            &member_indices
                .iter()
                .map(|&i| &embeddings[i])
                .collect::<Vec<_>>(),
        );
        let coherence = mean_cosine_to_centroid(
            &member_indices
                .iter()
                .map(|&i| &embeddings[i])
                .collect::<Vec<_>>(),
            &centroid,
        );
        let quote_spans = extract_quote_spans_for_cluster(
            &member_indices
                .iter()
                .map(|&i| &chunks[i])
                .collect::<Vec<_>>(),
            &member_indices
                .iter()
                .map(|&i| &embeddings[i])
                .collect::<Vec<_>>(),
            &centroid,
            MAX_QUOTE_SPANS_PER_NODE,
        );
        leaf_inputs.push(LeafSummarizationInput {
            member_indices,
            centroid,
            coherence,
            quote_spans,
        });
    }

    // ── Per-leaf checkpoint: short-circuit cached, summarize the rest.
    //
    // For each cluster_idx we either (a) lift the persisted RaptorNode
    // off disk and skip the LLM call entirely, or (b) include it in
    // `inputs_to_summarize` for the buffered fan-out. Cached and
    // freshly-summarized results are merged back in `cluster_idx`
    // order so the rest of the tree-building code sees a contiguous
    // `leaf_nodes` vec exactly as before.
    let total_clusters = leaf_inputs.len();
    let mut cached_results: Vec<Option<RaptorNode>> = vec![None; total_clusters];
    let mut to_summarize: Vec<(usize, ClusterSummarizationInput)> = Vec::new();
    for (cluster_idx, inp) in leaf_inputs.into_iter().enumerate() {
        if let Some(handle) = checkpoint {
            if let Ok(Some(cached)) = handle.read_cluster_node(0, cluster_idx) {
                cached_results[cluster_idx] = Some(cached);
                continue;
            }
        }
        to_summarize.push((
            cluster_idx,
            ClusterSummarizationInput {
                level: 0,
                member_descriptors: descriptors_for_prompt(
                    &inp.member_indices,
                    &inp.centroid,
                    chunks,
                    embeddings,
                ),
                direct_member_chunk_ids: inp
                    .member_indices
                    .iter()
                    .filter_map(|&i| chunks.get(i).map(|c| c.chunk_id))
                    .collect(),
                evidence_chunk_ids: inp
                    .member_indices
                    .iter()
                    .filter_map(|&i| chunks.get(i).map(|c| c.chunk_id))
                    .collect(),
                children_node_ids: Vec::new(),
                quote_spans: inp.quote_spans,
                centroid_embedding: inp.centroid,
                cluster_coherence: inp.coherence,
                correction_hint: correction_hint.map(|s| s.to_string()),
            },
        ));
    }
    // ── Longest-processing-time-first dispatch.
    //
    // K-means on text embeddings produces badly unbalanced clusters: on
    // the 301-chunk bench subset the leaf prompts ranged from 204 to
    // 4590 tokens — a 22× spread around a nominal 20-members-per-leaf
    // target. Summarization cost is prefill-dominated, so that spread is
    // a spread in call latency (4.3s to 30.0s measured). Dispatched in
    // cluster_idx order, a big cluster that happens to sort late starts
    // only after a buffer slot frees, and the level's makespan becomes
    // queue-wait + the straggler rather than just the straggler.
    //
    // LPT is the classic fix: start the longest jobs first so the short
    // ones fill the tail. Purely a scheduling change — same prompts,
    // same set of calls — and safe because results are merged back by
    // `cluster_idx`, not by position.
    to_summarize.sort_by_key(|(_, inp)| {
        std::cmp::Reverse(
            inp.member_descriptors
                .iter()
                .map(|d| d.len())
                .sum::<usize>(),
        )
    });
    if let (Some((_, biggest)), Some((_, smallest))) = (to_summarize.first(), to_summarize.last()) {
        tracing::debug!(
            clusters = to_summarize.len(),
            largest_chars = biggest
                .member_descriptors
                .iter()
                .map(|d| d.len())
                .sum::<usize>(),
            smallest_chars = smallest
                .member_descriptors
                .iter()
                .map(|d| d.len())
                .sum::<usize>(),
            "raptor_atlas: leaf prompt-size spread (dispatching longest-first)"
        );
    }

    let already_cached = total_clusters - to_summarize.len();
    if already_cached > 0 {
        tracing::info!(
            cached = already_cached,
            remaining = to_summarize.len(),
            total = total_clusters,
            "raptor_atlas: leaf-level checkpoint hit; skipping LLM for cached leaves"
        );
    }
    if let Some(sink) = progress {
        sink.report(
            EnrichmentPhase::RaptorLeaves,
            already_cached as u64,
            total_clusters as u64,
            Some(&format!(
                "summarising leaves ({already_cached}/{total_clusters} done)"
            )),
        )
        .await;
    }

    // Fan the uncached leaves across the mesh, persisting + emitting
    // progress after each completion so the chip moves under the
    // user's eyes and a daemon restart leaves the just-finished leaves
    // on disk for the next attempt.
    let freshly_summarized = summarize_clusters_buffered_with_checkpoint(
        inference,
        to_summarize,
        doc_type.clone(),
        checkpoint,
        progress,
        already_cached,
        total_clusters,
    )
    .await;
    for (cluster_idx, node) in freshly_summarized {
        if cluster_idx < cached_results.len() {
            cached_results[cluster_idx] = Some(node);
        }
    }

    let leaf_nodes: Vec<RaptorNode> = cached_results.into_iter().flatten().collect();

    let mut all_nodes: Vec<RaptorNode> = leaf_nodes.clone();

    // ── Levels 1..N — recurse on summaries ───────────────────
    let mut current_level: u8 = 1;
    let mut current_layer: Vec<RaptorNode> = leaf_nodes;
    while current_layer.len() > ROOT_BRANCHING_CEILING {
        let layer_embeddings: Vec<Vec<f32>> = current_layer
            .iter()
            .map(|n| n.summary_embedding.clone())
            .collect();
        // Non-leaf layers use smaller fan-out so the recursion
        // produces a proper mid-level scene-scale layer rather than
        // collapsing straight to the root in one step.
        let k_next = target_k(current_layer.len(), NONLEAF_TARGET_FANOUT);
        // If k_next ≥ current_layer.len() we'd "cluster" with one node
        // per cluster — degenerate. Force at least 2× compression.
        let k_next = k_next.max(2).min(current_layer.len() / 2);
        let assignments = kmeans_cluster(&layer_embeddings, k_next, 40);

        let mut next_inputs: Vec<ClusterSummarizationInput> = Vec::with_capacity(k_next);
        for cluster_idx in 0..k_next {
            let member_indices: Vec<usize> = assignments
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| (c == cluster_idx).then_some(i))
                .collect();
            if member_indices.is_empty() {
                continue;
            }
            let centroid = mean_vector(
                &member_indices
                    .iter()
                    .map(|&i| &layer_embeddings[i])
                    .collect::<Vec<_>>(),
            );
            let coherence = mean_cosine_to_centroid(
                &member_indices
                    .iter()
                    .map(|&i| &layer_embeddings[i])
                    .collect::<Vec<_>>(),
                &centroid,
            );
            let children_ids: Vec<String> = member_indices
                .iter()
                .map(|&i| current_layer[i].node_id.clone())
                .collect();
            // Union evidence chunk IDs from all child subtrees.
            let mut evidence: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
            for &i in &member_indices {
                evidence.extend(current_layer[i].evidence_chunk_ids.iter().copied());
            }
            let evidence_chunk_ids: Vec<u32> = evidence.into_iter().collect();

            // Pick quote spans from child nodes' spans — promote the
            // 3-5 with the longest length (proxy for substance).
            let mut all_child_spans: Vec<QuoteSpan> = member_indices
                .iter()
                .flat_map(|&i| current_layer[i].quote_spans.clone())
                .collect();
            all_child_spans.sort_by(|a, b| b.text.len().cmp(&a.text.len()));
            all_child_spans.truncate(MAX_QUOTE_SPANS_PER_NODE);

            // The "member descriptors" for a non-leaf summarization
            // are the child summaries themselves — that's RAPTOR's
            // recursion.
            let member_descriptors: Vec<String> = member_indices
                .iter()
                .map(|&i| {
                    let n = &current_layer[i];
                    let title_hint = n.primary_entities.first().cloned().unwrap_or_default();
                    if title_hint.is_empty() {
                        n.summary.clone()
                    } else {
                        format!("[{title_hint}] {}", n.summary)
                    }
                })
                .collect();

            next_inputs.push(ClusterSummarizationInput {
                level: current_level,
                member_descriptors,
                direct_member_chunk_ids: Vec::new(),
                evidence_chunk_ids,
                children_node_ids: children_ids,
                quote_spans: all_child_spans,
                centroid_embedding: centroid,
                cluster_coherence: coherence,
                correction_hint: correction_hint.map(|s| s.to_string()),
            });
        }

        let next_layer =
            summarize_clusters_buffered(inference, next_inputs, doc_type.clone()).await;
        if next_layer.is_empty() {
            // All summarization failed at this level. Treat the
            // existing layer as the root layer (no parent gets built).
            break;
        }
        all_nodes.extend(next_layer.clone());
        current_layer = next_layer;
        current_level = current_level.saturating_add(1);
        if current_level >= 8 {
            // Safety cap — recursion depth > 8 only on pathological
            // inputs; force-terminate to keep storage bounded.
            tracing::warn!(
                level = current_level,
                "raptor_atlas: hit depth cap, terminating recursion"
            );
            break;
        }
    }

    // Persist any non-leaf nodes the tree-level passes produced so a
    // future `load_all_nodes()` returns them too. Levels >0 aren't
    // checkpointed per-cluster during the in-build loop (the tree is
    // short), but the final mark_complete read at restart wouldn't
    // include them unless we write them here.
    if let Some(handle) = checkpoint {
        for node in &all_nodes {
            if node.level == 0 {
                continue;
            }
            // Use the node's index within its level as the cluster_idx
            // for the on-disk filename. Walk the level group on demand.
            let same_level_predecessors = all_nodes
                .iter()
                .take_while(|n| !std::ptr::eq(*n, node))
                .filter(|n| n.level == node.level)
                .count();
            if let Err(e) = handle.write_cluster_node(node.level, same_level_predecessors, node) {
                tracing::warn!(
                    level = node.level,
                    error = %e,
                    "raptor_atlas: persist non-leaf node failed"
                );
            }
        }
        if let Err(e) = handle.mark_complete() {
            tracing::warn!(
                error = %e,
                "raptor_atlas: mark_complete failed; next restart will re-summarize tree layers"
            );
        }
    }

    tracing::info!(
        nodes = all_nodes.len(),
        leaves = all_nodes.iter().filter(|n| n.level == 0).count(),
        max_level = all_nodes.iter().map(|n| n.level).max().unwrap_or(0),
        "raptor_atlas: build complete"
    );

    Ok(all_nodes)
}

/// Per-cluster-aware variant of `summarize_clusters_buffered`.
/// Persists each freshly-summarized `RaptorNode` to the checkpoint
/// (when provided) immediately on completion + emits a progress tick
/// through the optional sink. Returns `(cluster_idx, RaptorNode)`
/// pairs so the caller can place each result back in its slot.
async fn summarize_clusters_buffered_with_checkpoint(
    inference: &Arc<dyn InferenceProvider>,
    inputs: Vec<(usize, ClusterSummarizationInput)>,
    doc_type: DocumentTypeTag,
    checkpoint: Option<&RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn EnrichmentProgressSink>>,
    already_cached: usize,
    total_clusters: usize,
) -> Vec<(usize, RaptorNode)> {
    let inference = Arc::clone(inference);
    let doc_type = doc_type.clone();
    let mut stream = stream::iter(inputs)
        .map(|(cluster_idx, input)| {
            let inf = Arc::clone(&inference);
            let dt = doc_type.clone();
            async move {
                let node = summarize_one_cluster(&inf, input, dt).await;
                (cluster_idx, node)
            }
        })
        .buffered(SUMMARIZE_BUFFER);

    let mut out: Vec<(usize, RaptorNode)> = Vec::new();
    let mut completed = already_cached;
    while let Some((cluster_idx, maybe_node)) = stream.next().await {
        if let Some(node) = maybe_node {
            if let Some(handle) = checkpoint {
                if let Err(e) = handle.write_cluster_node(0, cluster_idx, &node) {
                    tracing::warn!(
                        cluster_idx,
                        error = %e,
                        "raptor_atlas: persist leaf failed; this leaf will re-summarize on retry"
                    );
                } else {
                    let _ = handle.touch();
                }
            }
            out.push((cluster_idx, node));
        }
        completed += 1;
        if let Some(sink) = progress {
            sink.report(
                EnrichmentPhase::RaptorLeaves,
                completed as u64,
                total_clusters as u64,
                Some(&format!(
                    "summarising leaves ({completed}/{total_clusters})"
                )),
            )
            .await;
        }
    }
    out
}

/// Compute the target number of clusters for an input layer.
/// Saturates at 2 below, no upper bound (k will be capped by caller
/// when degenerate).
fn target_k(n: usize, avg_cluster_size: usize) -> usize {
    if n <= avg_cluster_size {
        return 2.min(n);
    }
    (n + avg_cluster_size / 2) / avg_cluster_size
}

/// A document chunk packaged for RAPTOR ingest. `chunk_id` is the
/// stable index used in retrieval (matches what the DocumentStore
/// uses as `chunk_index`). `content` is the raw text used for both
/// quote-span extraction and as input to the leaf summarization
/// prompt.
#[derive(Debug, Clone)]
pub struct ChunkInput {
    pub chunk_id: u32,
    pub content: String,
}

impl ChunkInput {
    /// First ~280 chars — used as the "member descriptor" handed to
    /// the leaf summarization prompt. Keeps prefill bounded for the
    /// summarizer regardless of original chunk size.
    fn preview(&self) -> String {
        self.content.chars().take(280).collect()
    }
}

struct LeafSummarizationInput {
    member_indices: Vec<usize>,
    centroid: Vec<f32>,
    coherence: f32,
    quote_spans: Vec<QuoteSpan>,
}

struct ClusterSummarizationInput {
    level: u8,
    /// Descriptors of the cluster members. For leaves: chunk previews.
    /// For intermediate levels: child summaries (this is the RAPTOR
    /// recursion).
    member_descriptors: Vec<String>,
    direct_member_chunk_ids: Vec<u32>,
    evidence_chunk_ids: Vec<u32>,
    children_node_ids: Vec<String>,
    quote_spans: Vec<QuoteSpan>,
    centroid_embedding: Vec<f32>,
    cluster_coherence: f32,
    /// Optional user-authored correction (the "flag a wrong summary"
    /// revision loop). Populated for every cluster of a note that has an
    /// active correction; injected into the summarization prompt so
    /// regeneration is guided, not a blind re-roll.
    correction_hint: Option<String>,
}

/// Dispatch summarization for many clusters in parallel via
/// `buffered(SUMMARIZE_BUFFER)`. Each inflight call goes through
/// `inference.complete(Speed::Slow)` which routes to the mesh load
/// balancer — buffering at the dispatch layer is what lets the
/// balancer actually fan across peers instead of serializing on
/// awaited futures.
async fn summarize_clusters_buffered(
    inference: &Arc<dyn InferenceProvider>,
    inputs: Vec<ClusterSummarizationInput>,
    doc_type: DocumentTypeTag,
) -> Vec<RaptorNode> {
    let doc_type = doc_type.clone();
    let inference = Arc::clone(inference);
    let summarized: Vec<Option<RaptorNode>> = stream::iter(inputs)
        .map(|input| {
            let inf = Arc::clone(&inference);
            let dt = doc_type.clone();
            async move { summarize_one_cluster(&inf, input, dt).await }
        })
        .buffered(SUMMARIZE_BUFFER)
        .collect()
        .await;
    summarized.into_iter().flatten().collect()
}

/// Summarize a single cluster via one Slow-slot LLM call. The grammar
/// constraint on the summary field (no quote characters) makes the
/// hallucination-detector contract enforceable downstream: any
/// quoted span in a model answer must have come from a quote_span
/// or a retrieved chunk, never from a summary.
async fn summarize_one_cluster(
    inference: &Arc<dyn InferenceProvider>,
    input: ClusterSummarizationInput,
    doc_type: DocumentTypeTag,
) -> Option<RaptorNode> {
    let body = input
        .member_descriptors
        .iter()
        .enumerate()
        .map(|(i, d)| format!("[{i}] {d}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    let doc_cue = match doc_type {
        DocumentTypeTag::Narrative => {
            "scene-level summary: who is present, what happens, what shifts"
        }
        DocumentTypeTag::Argument => {
            "claim-level summary: which claim is advanced, what reasoning supports it"
        }
        DocumentTypeTag::Evidence => "result-level summary: what was tested, what was measured",
        DocumentTypeTag::Chronicle => "episode-level summary: who, when, what occurred",
        DocumentTypeTag::Technical => {
            "procedure-level summary: what step or component is described"
        }
        DocumentTypeTag::Journal => {
            "feeling-level summary: name the specific emotions and the concrete anchors \
             that carry them — the people, places, objects, and times of day these \
             entries keep returning to. Do NOT lead with month ranges or the word \
             period; a reader should recognise the felt experience, not the calendar \
             span"
        }
        DocumentTypeTag::Unknown => "section-level summary: topic and what is said about it",
    };

    // A user-authored correction (the "flag a wrong summary" revision
    // loop) is authoritative — inject it so regeneration fixes the
    // specific error instead of re-rolling into the same mistake. Rides
    // on every rebuild via the ledger lookup in enrich_conversation.
    let correction_block = match input.correction_hint.as_deref() {
        Some(hint) if !hint.trim().is_empty() => format!(
            "IMPORTANT — a reader reviewed a previous summary of this material and gave an \
             authoritative correction. Honor it precisely and let it override any conflicting \
             reading of the passages below:\n{}\n\n",
            hint.trim()
        ),
        _ => String::new(),
    };

    let prompt = format!(
        "You are summarizing a group of related passages from a {doc_type} document.\n\
         Produce a {cue}. The summary is a paraphrase — do NOT include any quotation marks \
         or verbatim quotations; we hold the source separately. Also list the primary entities \
         (characters, organizations, places, key concepts) by their canonical names as they \
         appear in the passages.\n\n\
         Respond with a single JSON object only:\n\
         {{\"summary\": \"<2-4 sentences, no quote marks>\", \"primary_entities\": [\"Name1\", \"Name2\"]}}\n\n\
         {correction_block}Passages:\n{body}\n\nJSON:",
        doc_type = doc_type.label(),
        cue = doc_cue,
    );

    // Grammar constraint: enforce JSON shape AND forbid the `\"` byte
    // inside the summary field. The summary string must consist of
    // non-quote characters only. Primary entities is an array of
    // capitalized names. Without this, models will helpfully insert
    // "quoted phrases" from the source even when told not to.
    let lark_grammar = r#"
start: "{\"summary\": \"" summary "\", \"primary_entities\": [" entities "]}"
summary: NOQUOTE+
entities: (entity (", " entity)*)?
entity: "\"" CAP_NAME "\""
NOQUOTE: /[^"\\]/
CAP_NAME: /[A-Z][A-Za-z'.]*( [A-Z][A-Za-z'.]*)*/
"#
    .to_string();

    // SLOT_POLICY §3 EnrichBulk (was ExtractDurable until 2026-07-24,
    // turbocharge arc): high-volume grammar-constrained summarization —
    // Fast-class routing lets `summarize_clusters_buffered`'s fan-out
    // ride the FastShort batching lane instead of serializing on the
    // pinned chat slot. Durability is protected by the grammar (shape
    // cannot drift) and the 500-token budget fits the bundle's 512 cap.
    let mut req =
        CompletionRequest::for_workload(Workload::EnrichBulk, prompt).with_output_budget(500);
    req.temperature = Some(0.2);
    // Grammar constraint preserved verbatim (see the lark_grammar above):
    // enforces the JSON shape AND forbids the `\"` byte inside the summary
    // field.
    req.lark_grammar = Some(lark_grammar);
    // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): Some(0) preserved for P1
    // neutrality (bundle is None); P5 confirms.
    req.think_budget = Some(0);

    let resp = match inference.complete(&req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                level = input.level,
                error = %e,
                "raptor_atlas: summary LLM call failed; dropping cluster"
            );
            return None;
        }
    };

    let parsed = match parse_cluster_summary(&resp.text) {
        Some(p) => p,
        None => {
            tracing::warn!(
                level = input.level,
                "raptor_atlas: summary parse failed; dropping cluster"
            );
            return None;
        }
    };

    // Embed the summary so query-time matching can hit this node.
    let summary_embedding = match inference.embed(&parsed.summary).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                level = input.level,
                error = %e,
                "raptor_atlas: summary embed failed; dropping cluster"
            );
            return None;
        }
    };

    Some(RaptorNode {
        node_id: uuid::Uuid::new_v4().to_string(),
        level: input.level,
        summary: parsed.summary,
        summary_embedding,
        centroid_embedding: input.centroid_embedding,
        children_node_ids: input.children_node_ids,
        direct_member_chunk_ids: input.direct_member_chunk_ids,
        evidence_chunk_ids: input.evidence_chunk_ids,
        quote_spans: input.quote_spans,
        primary_entities: parsed.primary_entities,
        cluster_coherence: input.cluster_coherence,
        created_at: chrono::Utc::now(),
    })
}

#[derive(Debug)]
struct ParsedSummary {
    summary: String,
    primary_entities: Vec<String>,
}

/// Parse the summarizer's JSON response. Lenient — accepts the JSON
/// wrapped in any preamble, drops trailing prose.
fn parse_cluster_summary(text: &str) -> Option<ParsedSummary> {
    #[derive(serde::Deserialize)]
    struct Raw {
        summary: String,
        #[serde(default)]
        primary_entities: Vec<String>,
    }
    let start = text.find('{')?;
    // Find the matching `}` by scanning forward — JSON has no nested
    // objects in this schema, so the first `}` after `{` is the close.
    let end_rel = text[start..].find('}')?;
    let json_slice = &text[start..=start + end_rel];
    let raw: Raw = serde_json::from_str(json_slice).ok()?;
    Some(ParsedSummary {
        summary: raw.summary,
        primary_entities: raw.primary_entities,
    })
}

/// Pick the highest-cosine-to-centroid sentences from the member
/// chunks as verbatim quote spans. These become the node's
/// hallucination-safe quotable surface.
fn extract_quote_spans_for_cluster(
    member_chunks: &[&ChunkInput],
    _member_embeddings: &[&Vec<f32>],
    _centroid: &[f32],
    max_spans: usize,
) -> Vec<QuoteSpan> {
    // V1 heuristic: walk each member chunk, take the longest sentence
    // (by char count) above the minimum length, dedupe across chunks
    // by first 40 chars. A future iteration can re-embed sentences
    // and rank by cosine-to-centroid for tighter signposts; for v1
    // longest-sentence-per-chunk is a strong proxy because the
    // chunker already prefers paragraph-coherent boundaries.
    let mut spans: Vec<QuoteSpan> = Vec::new();
    let mut seen_prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();
    for chunk in member_chunks {
        // Split on sentence-terminators, keep the offsets for
        // QuoteSpan accuracy.
        let mut best: Option<(usize, usize, String)> = None;
        let mut cursor = 0usize;
        for piece in chunk.content.split_terminator(['.', '!', '?']) {
            let len = piece.len();
            let start = cursor;
            let end = cursor + len;
            cursor = end + 1; // +1 for the terminator
            let trimmed = piece.trim();
            if trimmed.len() < MIN_QUOTE_SPAN_CHARS {
                continue;
            }
            if best
                .as_ref()
                .map(|(_, _, t)| trimmed.len() > t.len())
                .unwrap_or(true)
            {
                best = Some((start, end.min(chunk.content.len()), trimmed.to_string()));
            }
        }
        if let Some((start, end, text)) = best {
            let prefix: String = text.chars().take(40).collect();
            if seen_prefixes.insert(prefix) {
                spans.push(QuoteSpan {
                    chunk_id: chunk.chunk_id,
                    char_start: start as u32,
                    char_end: end as u32,
                    text,
                });
            }
        }
        if spans.len() >= max_spans {
            break;
        }
    }
    spans
}

/// Hard-assignment k-means on f32 vectors. Returns `assignments[i] =
/// cluster_idx` for each input. Deterministic given fixed input
/// (init picks the first k vectors as initial centroids — adequate
/// for v1; future work: k-means++ for better convergence).
fn kmeans_cluster(embeddings: &[Vec<f32>], k: usize, max_iters: usize) -> Vec<usize> {
    let n = embeddings.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    if k >= n {
        return (0..n).collect();
    }
    let dim = embeddings[0].len();
    // Init: pick k evenly-spaced vectors. Beats first-k for
    // documents where the first chunks are similar (e.g. a long
    // book opens with the same setting).
    let mut centroids: Vec<Vec<f32>> = (0..k).map(|i| embeddings[i * n / k].clone()).collect();
    let mut assignments = vec![0usize; n];

    for _iter in 0..max_iters {
        let mut changed = false;
        for (i, v) in embeddings.iter().enumerate() {
            let mut best_idx = 0usize;
            let mut best_sim = f32::NEG_INFINITY;
            for (c_idx, c) in centroids.iter().enumerate() {
                let s = cosine_sim(v, c);
                if s > best_sim {
                    best_sim = s;
                    best_idx = c_idx;
                }
            }
            if assignments[i] != best_idx {
                assignments[i] = best_idx;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        // Recompute centroids as mean of members.
        let mut sums: Vec<Vec<f32>> = vec![vec![0.0; dim]; k];
        let mut counts: Vec<usize> = vec![0; k];
        for (i, v) in embeddings.iter().enumerate() {
            let c = assignments[i];
            counts[c] += 1;
            for d in 0..dim.min(v.len()) {
                sums[c][d] += v[d];
            }
        }
        for c in 0..k {
            if counts[c] > 0 {
                for d in 0..dim {
                    centroids[c][d] = sums[c][d] / counts[c] as f32;
                }
            }
            // If a cluster goes empty, leave its centroid where it was;
            // it'll either pick up members next iter or stay empty
            // (caller filters empty clusters out).
        }
    }
    assignments
}

/// The member previews shown to the leaf summarizer: at most
/// [`MAX_MEMBERS_IN_SUMMARY_PROMPT`], the ones closest to the cluster
/// centroid, returned in document order.
///
/// Document order matters for a narrative: the summarizer is asked
/// "what happens, what shifts", and reading the retained previews in
/// the order they occur in the text preserves that. Selection is by
/// centrality; presentation is chronological.
fn descriptors_for_prompt(
    member_indices: &[usize],
    centroid: &[f32],
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
) -> Vec<String> {
    let mut kept: Vec<usize> = if member_indices.len() <= MAX_MEMBERS_IN_SUMMARY_PROMPT {
        member_indices.to_vec()
    } else {
        let mut ranked: Vec<(usize, f32)> = member_indices
            .iter()
            .map(|&i| {
                let sim = embeddings
                    .get(i)
                    .map(|e| cosine_sim(e, centroid))
                    .unwrap_or(f32::MIN);
                (i, sim)
            })
            .collect();
        // Descending similarity; ties broken by index so the choice is
        // deterministic across runs (checkpoint resume must reproduce it).
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        ranked.truncate(MAX_MEMBERS_IN_SUMMARY_PROMPT);
        ranked.into_iter().map(|(i, _)| i).collect()
    };
    kept.sort_unstable();
    kept.iter()
        .filter_map(|&i| chunks.get(i).map(|c| c.preview()))
        .collect()
}

/// Element-wise mean of a slice of references to vectors. Returns
/// an empty vec if input is empty.
fn mean_vector(vecs: &[&Vec<f32>]) -> Vec<f32> {
    if vecs.is_empty() {
        return Vec::new();
    }
    let dim = vecs[0].len();
    let mut out = vec![0.0f32; dim];
    for v in vecs {
        for d in 0..dim.min(v.len()) {
            out[d] += v[d];
        }
    }
    let n = vecs.len() as f32;
    for slot in out.iter_mut() {
        *slot /= n;
    }
    out
}

/// Mean cosine similarity of each vector to the centroid. In [0,1]
/// for normalized embeddings, [-1,1] in general; we clamp to [0,1]
/// for use as the `cluster_coherence` field (higher = tighter).
fn mean_cosine_to_centroid(vecs: &[&Vec<f32>], centroid: &[f32]) -> f32 {
    if vecs.is_empty() {
        return 0.0;
    }
    let sum: f32 = vecs.iter().map(|v| cosine_sim(v, centroid)).sum();
    let mean = sum / vecs.len() as f32;
    mean.clamp(0.0, 1.0)
}

/// Local cosine-similarity helper (the one in document_asset.rs is
/// not pub-visible).
fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ortho_clusters(cluster_count: usize, per_cluster: usize) -> Vec<Vec<f32>> {
        // Each cluster shares a near-orthogonal one-hot signature.
        let mut out = Vec::new();
        for c in 0..cluster_count {
            for k in 0..per_cluster {
                let mut v = vec![0.05; 8];
                v[c % 8] = 1.0;
                v[(c + k + 1) % 8] += 0.02;
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn kmeans_recovers_orthogonal_clusters() {
        let embs = ortho_clusters(3, 5); // 15 vectors, 3 true clusters
        let assignments = kmeans_cluster(&embs, 3, 50);
        assert_eq!(assignments.len(), 15);
        // Every input from the same true cluster must end up in the
        // same predicted cluster.
        let true_cluster = |i: usize| i / 5;
        for true_c in 0..3 {
            let preds: std::collections::HashSet<usize> = (0..15)
                .filter(|&i| true_cluster(i) == true_c)
                .map(|i| assignments[i])
                .collect();
            assert_eq!(
                preds.len(),
                1,
                "true cluster {true_c} should map to one predicted cluster, got {preds:?}"
            );
        }
    }

    #[test]
    fn kmeans_handles_k_geq_n() {
        // k >= n: degenerate, every input is its own cluster.
        let embs = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let assignments = kmeans_cluster(&embs, 5, 10);
        assert_eq!(assignments, vec![0, 1]);
    }

    #[test]
    fn target_k_picks_sensible_counts() {
        assert_eq!(target_k(1006, 20), 50);
        assert_eq!(target_k(200, 20), 10);
        assert_eq!(target_k(15, 20), 2); // tiny doc → minimum
        assert_eq!(target_k(0, 20), 0);
    }

    #[test]
    fn mean_vector_averages_elementwise() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![3.0, 2.0, 1.0];
        let m = mean_vector(&[&a, &b]);
        assert_eq!(m, vec![2.0, 2.0, 2.0]);
    }

    /// A cluster larger than the prompt cap keeps only its most-central
    /// members in the prompt — and keeps them in document order.
    #[test]
    fn descriptors_for_prompt_caps_by_centrality_and_reorders_chronologically() {
        // 40 chunks. Even indices sit on the centroid; odd indices are
        // orthogonal to it. 20 evens > the cap, so centrality alone
        // decides and no odd member can slip in to fill a spare slot.
        let chunks: Vec<ChunkInput> = (0..40)
            .map(|i| ChunkInput {
                chunk_id: i as u32,
                content: format!("chunk {i} body text"),
            })
            .collect();
        let embeddings: Vec<Vec<f32>> = (0..40)
            .map(|i| {
                if i % 2 == 0 {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect();
        let members: Vec<usize> = (0..40).collect();
        let centroid = vec![1.0, 0.0];

        let out = descriptors_for_prompt(&members, &centroid, &chunks, &embeddings);
        assert_eq!(out.len(), MAX_MEMBERS_IN_SUMMARY_PROMPT);
        // Only central (even) chunks survive…
        for d in &out {
            let n: usize = d
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .expect("preview starts `chunk <n>`");
            assert_eq!(n % 2, 0, "kept an off-centroid member: {d}");
        }
        // …and they read in document order, not similarity order.
        let order: Vec<usize> = out
            .iter()
            .map(|d| d.split_whitespace().nth(1).unwrap().parse().unwrap())
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "previews must stay chronological");
    }

    /// Small clusters are untouched — the cap is a ceiling, not a quota.
    #[test]
    fn descriptors_for_prompt_leaves_small_clusters_whole() {
        let chunks: Vec<ChunkInput> = (0..4)
            .map(|i| ChunkInput {
                chunk_id: i as u32,
                content: format!("chunk {i} body text"),
            })
            .collect();
        let embeddings: Vec<Vec<f32>> = (0..4).map(|_| vec![1.0, 0.0]).collect();
        let members: Vec<usize> = (0..4).collect();
        let out = descriptors_for_prompt(&members, &[1.0, 0.0], &chunks, &embeddings);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn extract_quote_spans_pulls_longest_sentence_per_chunk() {
        let chunks = [
            ChunkInput {
                chunk_id: 1,
                content: "Short. This is the load-bearing sentence with quite a few words. Tiny."
                    .to_string(),
            },
            ChunkInput {
                chunk_id: 2,
                content: "Another chunk where this longer sentence is the one to anchor on. End."
                    .to_string(),
            },
        ];
        let embs = [vec![1.0, 0.0], vec![0.0, 1.0]];
        let refs: Vec<&Vec<f32>> = embs.iter().collect();
        let chunk_refs: Vec<&ChunkInput> = chunks.iter().collect();
        let centroid = vec![0.5, 0.5];
        let spans = extract_quote_spans_for_cluster(&chunk_refs, &refs, &centroid, 5);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].text.contains("load-bearing"));
        assert!(spans[1].text.contains("longer sentence"));
        // chunk_id preserved.
        assert_eq!(spans[0].chunk_id, 1);
        assert_eq!(spans[1].chunk_id, 2);
    }

    #[test]
    fn extract_quote_spans_dedupes_by_prefix() {
        let chunks = [
            ChunkInput {
                chunk_id: 1,
                content:
                    "The professor walked through London streets alone and unsuspected by men."
                        .to_string(),
            },
            ChunkInput {
                chunk_id: 2,
                content:
                    "The professor walked through London streets alone and unsuspected by men."
                        .to_string(),
            },
        ];
        let embs = [vec![1.0, 0.0], vec![1.0, 0.0]];
        let refs: Vec<&Vec<f32>> = embs.iter().collect();
        let chunk_refs: Vec<&ChunkInput> = chunks.iter().collect();
        let centroid = vec![1.0, 0.0];
        let spans = extract_quote_spans_for_cluster(&chunk_refs, &refs, &centroid, 5);
        assert_eq!(
            spans.len(),
            1,
            "identical spans across chunks should dedupe"
        );
    }

    #[test]
    fn parse_cluster_summary_extracts_json_from_preamble() {
        let resp = r#"Here you go: {"summary": "Winnie kills Verloc in the parlour after learning of Stevie's death.", "primary_entities": ["Winnie", "Verloc", "Stevie"]} done."#;
        let parsed = parse_cluster_summary(resp).expect("should parse");
        assert!(parsed.summary.contains("Winnie kills Verloc"));
        assert_eq!(parsed.primary_entities, vec!["Winnie", "Verloc", "Stevie"]);
    }

    #[test]
    fn parse_cluster_summary_returns_none_on_garbage() {
        assert!(parse_cluster_summary("no JSON here, just prose").is_none());
        assert!(parse_cluster_summary("{ malformed").is_none());
    }

    #[test]
    fn mean_cosine_to_centroid_is_in_zero_one() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let c = vec![1.0, 1.0];
        let refs: Vec<&Vec<f32>> = vec![&a, &b];
        let m = mean_cosine_to_centroid(&refs, &c);
        // a→c and b→c each have cosine ~0.707; mean clamped to [0,1].
        assert!(m > 0.6 && m < 0.8, "expected ~0.707, got {m}");
    }
}
