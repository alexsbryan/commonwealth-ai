// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation entity co-occurrence graph + Personalized PageRank.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md` §"T2 via reused
//! RAPTOR primary_entities" (Option A — zero new LLM cost).
//!
//! ## Idea
//!
//! The conv-tiered RAPTOR pipeline already extracts `primary_entities`
//! per leaf cluster as a byproduct of summarization (see
//! `RaptorNode.primary_entities` — "Union of GLiNER tags on member
//! chunks and entities the summarization prompt explicitly
//! identified"). On the `conversations-anthropic` corpus this gives
//! us ~877 nodes × ~5 entities/node = ~4400 raw entity mentions for
//! free — no per-conv `extract_action_atoms` LLM batch needed.
//!
//! This module turns those mentions into a graph and runs
//! Personalized PageRank (HippoRAG-1-style) at retrieval time:
//!
//! 1. **Construct** — entities = nodes, edges = co-occurrence within
//!    the same RAPTOR cluster, weighted by `cluster_coherence` so
//!    tight clusters' bonds dominate fuzzy ones.
//! 2. **Seed** — entities mentioned (substring match, word-boundary,
//!    case-insensitive) in the user's query.
//! 3. **Diffuse** — PageRank with restart on seeds, damping = 0.85,
//!    ~20 iterations.
//! 4. **Apply** — each chunk's PPR mass = sum of mass of entities in
//!    the RAPTOR node(s) that own the chunk. Blend with cosine via
//!    `final = (1-α)·cosine_norm + α·entity_norm`.
//!
//! Graphs are per-conversation: each conv builds its own small graph
//! (typically <30 entities). PPR on those is sub-millisecond. Cross-
//! conv scoring happens via the existing chunk-cosine normalisation,
//! not via a unified mega-graph.

use std::collections::HashMap;

use crate::conv_tiered::{ChunkEntityRow, ConvRaptorNodeRow};

/// Per-conv background co-occurrence weight (every entity pair in a
/// conv gets this much edge weight). Picked small relative to
/// `cluster_coherence` (typically 0.5–0.95) so RAPTOR's clustering
/// signal still dominates, but large enough that isolated single-
/// entity clusters can still receive PPR mass from co-conv seeds.
const CONV_CLIQUE_WEIGHT: f32 = 0.1;

/// Per-chunk co-occurrence weight added when two entities co-occur
/// in the same retrieved chunk. Used by the
/// `from_chunk_entities` constructor (Phase 1 — GliNER per-chunk
/// NER). Set higher than `CONV_CLIQUE_WEIGHT` (0.1) so a chunk-
/// level co-occurrence dominates the conv-wide baseline, while
/// still allowing edges to accumulate across many chunks. Without
/// this scale, the conv-clique would drown out the structural
/// signal that "these entities appear TOGETHER in retrieved
/// content" — the exact bridge a PPR seed needs.
const CHUNK_CO_OCCURRENCE_WEIGHT: f32 = 0.5;

/// Per-conversation entity co-occurrence graph plus the chunk
/// membership reverse-index needed to project entity mass back onto
/// retrieval results.
#[derive(Debug, Clone, Default)]
pub struct ConvEntityGraph {
    pub corpus_id: String,
    pub conv_uuid: String,

    /// Canonical entity name → its index. Lower-cased for matching;
    /// the original casing is preserved in `entity_names`.
    name_to_idx: HashMap<String, usize>,
    /// Original casing per index (for briefing render + debug).
    entity_names: Vec<String>,
    /// Outgoing-edge list per entity index. Each entry is
    /// `(neighbour_idx, weight)`. Symmetric: an edge appears in both
    /// directions.
    adjacency: Vec<Vec<(usize, f32)>>,
    /// For each entity, the set of RAPTOR `node_id`s containing it.
    /// Used to project PPR mass onto chunks at retrieval time.
    entity_to_nodes: Vec<Vec<String>>,
    /// For each RAPTOR `node_id`, the chunk ids it directly owns.
    /// Built once at construction so retrieval-time lookups don't
    /// re-parse JSON.
    node_to_chunks: HashMap<String, Vec<u64>>,
}

impl ConvEntityGraph {
    /// Build the graph for one conversation from its RAPTOR nodes.
    /// Empty conv (no nodes / no entities) → empty graph that
    /// returns `0.0` from every query without panicking.
    pub fn from_raptor_nodes(
        corpus_id: &str,
        conv_uuid: &str,
        nodes: &[ConvRaptorNodeRow],
    ) -> Self {
        let mut graph = ConvEntityGraph {
            corpus_id: corpus_id.to_string(),
            conv_uuid: conv_uuid.to_string(),
            ..Default::default()
        };

        // Pass 1: collect unique entities + cache node→chunks.
        for node in nodes {
            let entities = parse_entities(&node.primary_entities_json);
            for ent in &entities {
                graph.intern_entity(ent);
            }
            if let Some(chunk_ids_json) = node.direct_member_chunk_ids_json.as_deref() {
                if let Ok(chunk_ids) = serde_json::from_str::<Vec<u64>>(chunk_ids_json) {
                    if !chunk_ids.is_empty() {
                        graph.node_to_chunks.insert(node.node_id.clone(), chunk_ids);
                    }
                }
            }
        }

        // Pass 2: build adjacency + entity_to_nodes.
        // Two layers of edges:
        //   - **Cluster co-occurrence** (strong): pairs of entities
        //     that appear together in the same RAPTOR leaf, weighted
        //     by `cluster_coherence`. Captures "RAPTOR judged these
        //     entities topically tight."
        //   - **Conv-level clique** (weak): every pair of entities
        //     in this conv, regardless of which cluster, gets a
        //     small constant edge. The conv IS one conversation —
        //     all entities co-occur at conversation scope even
        //     when RAPTOR's per-leaf clustering boundary separates
        //     them. Without this layer, a single-entity cluster
        //     (e.g. an isolated "Gulliver's Travels" leaf) cannot
        //     receive PPR mass from a seed elsewhere in the same
        //     conv — observed live on the `Modern Swift Satirical
        //     Scenarios` query 2026-05-22.
        graph.adjacency = vec![Vec::new(); graph.entity_names.len()];
        graph.entity_to_nodes = vec![Vec::new(); graph.entity_names.len()];

        // Conv-level clique pass — runs first so cluster-bond weights
        // accumulate on top of the baseline.
        let n = graph.entity_names.len();
        for u in 0..n {
            for v in (u + 1)..n {
                add_or_accumulate_edge(&mut graph.adjacency[u], v, CONV_CLIQUE_WEIGHT);
                add_or_accumulate_edge(&mut graph.adjacency[v], u, CONV_CLIQUE_WEIGHT);
            }
        }

        for node in nodes {
            let entities = parse_entities(&node.primary_entities_json);
            // Map entity strings to indices, dedupe within node so a
            // duplicate name in one cluster doesn't create a self-loop
            // or double-weight an edge.
            let mut idx_set: Vec<usize> = Vec::with_capacity(entities.len());
            for ent in &entities {
                if let Some(&i) = graph.name_to_idx.get(&ent.to_lowercase()) {
                    if !idx_set.contains(&i) {
                        idx_set.push(i);
                    }
                }
            }
            // Each entity in this cluster belongs to this RAPTOR node.
            for &i in &idx_set {
                graph.entity_to_nodes[i].push(node.node_id.clone());
            }
            // Add a co-occurrence edge for every unordered pair in
            // this cluster, weighted by the cluster's coherence.
            // `cluster_coherence` is already in [0,1]; we floor at
            // a small positive value so even loose clusters
            // contribute *something* (zero-weight edges would make
            // PPR mass leak into the void).
            let weight = (node.cluster_coherence as f32).max(0.05);
            for a in 0..idx_set.len() {
                for b in (a + 1)..idx_set.len() {
                    let (u, v) = (idx_set[a], idx_set[b]);
                    add_or_accumulate_edge(&mut graph.adjacency[u], v, weight);
                    add_or_accumulate_edge(&mut graph.adjacency[v], u, weight);
                }
            }
        }

        graph
    }

    /// Layered builder — combines RAPTOR `primary_entities` AND
    /// GliNER per-chunk entities into ONE graph. The two sources
    /// carry orthogonal signals: RAPTOR captures LLM-judged
    /// distinctiveness at cluster scale, GliNER captures raw NER
    /// recall at chunk scale. A pair of entities mentioned in
    /// both layers gets edge weight accumulated from BOTH —
    /// cross-source agreement = strongest bond in the graph.
    ///
    /// Edge layers, applied additively:
    /// 1. **Conv-clique baseline** (weight 0.1 per pair) —
    ///    structural background so isolated entities stay
    ///    PPR-reachable.
    /// 2. **RAPTOR cluster co-occurrence** (weight =
    ///    `cluster_coherence.max(0.05)` per shared cluster) —
    ///    LLM-confirmed topical grouping.
    /// 3. **GliNER chunk co-occurrence** (weight 0.5 per shared
    ///    chunk) — surface-form proximity.
    ///
    /// Either argument can be empty: passing only `raptor_nodes`
    /// behaves equivalently to `from_raptor_nodes`; passing only
    /// `chunk_entities` matches `from_chunk_entities`. The
    /// runtime path can call `from_layered` unconditionally and let
    /// the empty-collection cases collapse the unused layers.
    ///
    /// `node_to_chunks` carries entries from BOTH sources:
    /// RAPTOR leaf nodes own their `direct_member_chunk_ids`;
    /// GliNER `chunk-<id>` synthetic nodes each own themselves.
    /// Mass projection walks both paths.
    pub fn from_layered(
        corpus_id: &str,
        conv_uuid: &str,
        raptor_nodes: &[ConvRaptorNodeRow],
        chunk_entities: &[ChunkEntityRow],
    ) -> Self {
        let mut graph = ConvEntityGraph {
            corpus_id: corpus_id.to_string(),
            conv_uuid: conv_uuid.to_string(),
            ..Default::default()
        };

        // Pass 1: intern all entities across both sources + cache
        // node_to_chunks for the RAPTOR side.
        for node in raptor_nodes {
            for ent in parse_entities(&node.primary_entities_json) {
                graph.intern_entity(&ent);
            }
            if let Some(chunk_ids_json) = node.direct_member_chunk_ids_json.as_deref() {
                if let Ok(chunk_ids) = serde_json::from_str::<Vec<u64>>(chunk_ids_json) {
                    if !chunk_ids.is_empty() {
                        graph.node_to_chunks.insert(node.node_id.clone(), chunk_ids);
                    }
                }
            }
        }

        // Group chunk_entities by chunk_id (dedup within chunk) +
        // intern + register chunk-node ownership.
        let mut by_chunk: HashMap<u64, Vec<String>> = HashMap::new();
        for row in chunk_entities {
            let text = row.text.trim();
            if text.is_empty() {
                continue;
            }
            graph.intern_entity(text);
            let entry = by_chunk.entry(row.chunk_id).or_default();
            let key = text.to_lowercase();
            if !entry.contains(&key) {
                entry.push(key);
            }
        }
        for chunk_id in by_chunk.keys() {
            let node_id = format!("chunk-{chunk_id}");
            graph.node_to_chunks.insert(node_id, vec![*chunk_id]);
        }

        let n = graph.entity_names.len();
        graph.adjacency = vec![Vec::new(); n];
        graph.entity_to_nodes = vec![Vec::new(); n];

        // Layer 1: conv-clique baseline.
        for u in 0..n {
            for v in (u + 1)..n {
                add_or_accumulate_edge(&mut graph.adjacency[u], v, CONV_CLIQUE_WEIGHT);
                add_or_accumulate_edge(&mut graph.adjacency[v], u, CONV_CLIQUE_WEIGHT);
            }
        }

        // Layer 2: RAPTOR cluster co-occurrence + node membership.
        for node in raptor_nodes {
            let entities = parse_entities(&node.primary_entities_json);
            let mut idx_set: Vec<usize> = Vec::with_capacity(entities.len());
            for ent in &entities {
                if let Some(&i) = graph.name_to_idx.get(&ent.to_lowercase()) {
                    if !idx_set.contains(&i) {
                        idx_set.push(i);
                    }
                }
            }
            for &i in &idx_set {
                graph.entity_to_nodes[i].push(node.node_id.clone());
            }
            let weight = (node.cluster_coherence as f32).max(0.05);
            for a in 0..idx_set.len() {
                for b in (a + 1)..idx_set.len() {
                    let (u, v) = (idx_set[a], idx_set[b]);
                    add_or_accumulate_edge(&mut graph.adjacency[u], v, weight);
                    add_or_accumulate_edge(&mut graph.adjacency[v], u, weight);
                }
            }
        }

        // Layer 3: GliNER chunk co-occurrence + chunk-node
        // membership.
        for (chunk_id, ent_keys) in &by_chunk {
            let node_id = format!("chunk-{chunk_id}");
            let mut idx_set: Vec<usize> = Vec::with_capacity(ent_keys.len());
            for key in ent_keys {
                if let Some(&i) = graph.name_to_idx.get(key) {
                    if !idx_set.contains(&i) {
                        idx_set.push(i);
                    }
                }
            }
            for &i in &idx_set {
                graph.entity_to_nodes[i].push(node_id.clone());
            }
            for a in 0..idx_set.len() {
                for b in (a + 1)..idx_set.len() {
                    let (u, v) = (idx_set[a], idx_set[b]);
                    add_or_accumulate_edge(&mut graph.adjacency[u], v, CHUNK_CO_OCCURRENCE_WEIGHT);
                    add_or_accumulate_edge(&mut graph.adjacency[v], u, CHUNK_CO_OCCURRENCE_WEIGHT);
                }
            }
        }

        graph
    }

    /// Build the graph from GliNER per-chunk extractions (spec
    /// Phase 1). Same struct shape as `from_raptor_nodes` so the
    /// downstream PPR + chunk-mass projection paths work
    /// unchanged. Two edge layers:
    ///
    /// 1. **Chunk co-occurrence** (strong): every pair of entities
    ///    appearing in the same chunk gets
    ///    `CHUNK_CO_OCCURRENCE_WEIGHT` per shared chunk. Accumulates
    ///    across chunks, so entities consistently mentioned together
    ///    end up with high edge weight.
    /// 2. **Conv-level clique** (weak): same baseline as the
    ///    RAPTOR backing. Ensures isolated entities (mentioned in
    ///    only one chunk with no co-occurrence) can still receive
    ///    PPR mass.
    ///
    /// Node abstraction: each chunk is one node, `node_id =
    /// "chunk-<id>"`. `node_to_chunks` maps to a single-entry
    /// vector — keeps the same downstream API as RAPTOR backing
    /// where one node owns multiple chunks.
    ///
    /// Entity keys are case-insensitive on `text` only — we
    /// deliberately collapse `("Swift", Person)` and
    /// `("SWIFT", Organization)` into one node since they share
    /// surface form and PPR diffusion of "what's related to
    /// swift" should reach both. Future refinement: typed nodes
    /// when label-typed retrieval becomes load-bearing.
    pub fn from_chunk_entities(corpus_id: &str, conv_uuid: &str, rows: &[ChunkEntityRow]) -> Self {
        let mut graph = ConvEntityGraph {
            corpus_id: corpus_id.to_string(),
            conv_uuid: conv_uuid.to_string(),
            ..Default::default()
        };
        if rows.is_empty() {
            return graph;
        }

        // Group entity rows by chunk_id. Dedupe within a chunk so
        // a duplicated mention (same text, different label or
        // multiple offsets) contributes once to the co-occurrence
        // graph.
        let mut by_chunk: HashMap<u64, Vec<String>> = HashMap::new();
        for row in rows {
            let text = row.text.trim();
            if text.is_empty() {
                continue;
            }
            graph.intern_entity(text);
            let entry = by_chunk.entry(row.chunk_id).or_default();
            let key = text.to_lowercase();
            if !entry.contains(&key) {
                entry.push(key);
            }
        }

        // Cache node_to_chunks: each chunk = one node owning
        // exactly itself.
        for chunk_id in by_chunk.keys() {
            let node_id = format!("chunk-{chunk_id}");
            graph.node_to_chunks.insert(node_id, vec![*chunk_id]);
        }

        let n = graph.entity_names.len();
        graph.adjacency = vec![Vec::new(); n];
        graph.entity_to_nodes = vec![Vec::new(); n];

        // Conv-clique baseline (same rationale as the RAPTOR
        // builder — prevents isolated entities from being
        // PPR-invisible).
        for u in 0..n {
            for v in (u + 1)..n {
                add_or_accumulate_edge(&mut graph.adjacency[u], v, CONV_CLIQUE_WEIGHT);
                add_or_accumulate_edge(&mut graph.adjacency[v], u, CONV_CLIQUE_WEIGHT);
            }
        }

        // Chunk-level co-occurrence layer.
        for (chunk_id, ent_keys) in &by_chunk {
            let node_id = format!("chunk-{chunk_id}");
            // Map each entity in this chunk to its index + record
            // node membership.
            let mut idx_set: Vec<usize> = Vec::with_capacity(ent_keys.len());
            for key in ent_keys {
                if let Some(&i) = graph.name_to_idx.get(key) {
                    if !idx_set.contains(&i) {
                        idx_set.push(i);
                    }
                }
            }
            for &i in &idx_set {
                graph.entity_to_nodes[i].push(node_id.clone());
            }
            // Pairwise edges within this chunk.
            for a in 0..idx_set.len() {
                for b in (a + 1)..idx_set.len() {
                    let (u, v) = (idx_set[a], idx_set[b]);
                    add_or_accumulate_edge(&mut graph.adjacency[u], v, CHUNK_CO_OCCURRENCE_WEIGHT);
                    add_or_accumulate_edge(&mut graph.adjacency[v], u, CHUNK_CO_OCCURRENCE_WEIGHT);
                }
            }
        }

        graph
    }

    pub fn is_empty(&self) -> bool {
        self.entity_names.is_empty()
    }

    pub fn entity_count(&self) -> usize {
        self.entity_names.len()
    }

    /// Original-case entity name at `idx`. Used by the PPR rerank's
    /// provenance stamping (A3-lite) to record which entity bridge
    /// surfaced a chunk.
    pub fn entity_name(&self, idx: usize) -> Option<String> {
        self.entity_names.get(idx).cloned()
    }

    pub fn edge_count(&self) -> usize {
        // Each undirected edge is stored twice in adjacency.
        self.adjacency.iter().map(|v| v.len()).sum::<usize>() / 2
    }

    /// Identify entities in the query. Three escalating match
    /// strategies — first hit wins, no double-counting:
    ///
    /// 1. **Full-phrase word-boundary match** — entity name appears
    ///    verbatim in the query as a word-bounded token sequence.
    ///    Catches `"Toni Morrison"` in `"reread Toni Morrison"`.
    /// 2. **Token-level word-boundary match** — for multi-word
    ///    entities, any token ≥4 chars from the entity name
    ///    matched against query tokens. Catches `Jonathan Swift`
    ///    when the query just says `"Swift"`.
    /// 3. **Prefix-stem match** — entity token shares ≥4-char
    ///    common prefix with a query token. Catches `"Satire"`
    ///    against `"satirical"` and `"economics"` against
    ///    `"economy"`.
    ///
    /// `"tao"` will not seed on `"taonomy"` (alphanumeric boundary
    /// required); stop-word tokens shorter than 4 chars are skipped
    /// to keep noise out (`"the"`, `"and"`, etc.).
    pub fn seed_indices_from_query(&self, query: &str) -> Vec<usize> {
        let q_lower = query.to_lowercase();
        let query_tokens: Vec<&str> = q_lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().count() >= 4)
            .collect();
        let mut out = Vec::new();
        for (i, name) in self.entity_names.iter().enumerate() {
            let needle = name.to_lowercase();
            // Strategy 1: full-phrase word-boundary match.
            if is_word_boundary_match(&q_lower, &needle) {
                out.push(i);
                continue;
            }
            // Strategies 2 + 3 operate on per-token aliases of the
            // entity name. Single-word entities skip strategy 2
            // (already covered by strategy 1) but still pick up
            // strategy 3 prefix-stem matches.
            let entity_tokens: Vec<String> = needle
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.chars().count() >= 4)
                .map(|t| t.to_string())
                .collect();
            let mut hit = false;
            for ent_tok in &entity_tokens {
                if hit {
                    break;
                }
                for q_tok in &query_tokens {
                    if tokens_share_prefix(ent_tok, q_tok, 4) {
                        hit = true;
                        break;
                    }
                }
            }
            if hit {
                out.push(i);
            }
        }
        out
    }

    /// Run Personalized PageRank. Returns one mass value per entity
    /// in `[0, 1]` summing to ~1.0 (modulo float drift). Empty seeds
    /// → uniform random walk over the graph (still useful as a
    /// "centrality" prior, but the conv-tiered retrieval path checks
    /// for empty seeds and skips the blend in that case).
    pub fn personalized_pagerank(
        &self,
        seeds: &[usize],
        damping: f32,
        iterations: usize,
    ) -> Vec<f32> {
        let n = self.entity_names.len();
        if n == 0 {
            return Vec::new();
        }
        // Seed distribution: uniform across provided seeds, or uniform
        // across all nodes when seeds is empty.
        let mut restart = vec![0f32; n];
        if seeds.is_empty() {
            let v = 1.0 / n as f32;
            for r in restart.iter_mut() {
                *r = v;
            }
        } else {
            let v = 1.0 / seeds.len() as f32;
            for &s in seeds {
                if s < n {
                    restart[s] = v;
                }
            }
        }
        // Out-weight sums per node (denominator for transition).
        let out_sums: Vec<f32> = self
            .adjacency
            .iter()
            .map(|adj| adj.iter().map(|(_, w)| *w).sum::<f32>())
            .collect();

        // Initialize mass uniformly OR with restart vector for fast
        // convergence on focused queries. Restart is the better
        // initializer when seeds is small.
        let mut mass = restart.clone();
        let mut next = vec![0f32; n];

        for _ in 0..iterations {
            for slot in next.iter_mut() {
                *slot = 0.0;
            }
            // Distribute current mass through edges.
            for (i, adj) in self.adjacency.iter().enumerate() {
                if out_sums[i] == 0.0 {
                    // Dead-end (no edges) — leak mass to the restart
                    // vector so PPR converges instead of vanishing.
                    for j in 0..n {
                        next[j] += mass[i] * restart[j];
                    }
                    continue;
                }
                for &(j, w) in adj {
                    next[j] += mass[i] * w / out_sums[i];
                }
            }
            // Apply damping + restart.
            for j in 0..n {
                next[j] = damping * next[j] + (1.0 - damping) * restart[j];
            }
            std::mem::swap(&mut mass, &mut next);
        }
        mass
    }

    /// Project PPR entity mass onto chunks: each chunk_id gets the
    /// sum of mass of entities present in any RAPTOR node owning
    /// that chunk. Returns `chunk_id → mass`. Chunks not owned by
    /// any node (RAPTOR didn't cluster them — shouldn't happen for
    /// healthy T1+T3 corpora) are absent from the map.
    pub fn chunk_mass(&self, entity_mass: &[f32]) -> HashMap<u64, f32> {
        let mut out: HashMap<u64, f32> = HashMap::new();
        if entity_mass.len() != self.entity_names.len() {
            return out;
        }
        // For each entity, walk its node memberships, then each
        // node's direct chunks, accumulating mass.
        for (i, m) in entity_mass.iter().enumerate() {
            if *m == 0.0 {
                continue;
            }
            for node_id in &self.entity_to_nodes[i] {
                if let Some(chunks) = self.node_to_chunks.get(node_id) {
                    for &chunk_id in chunks {
                        *out.entry(chunk_id).or_insert(0.0) += *m;
                    }
                }
            }
        }
        out
    }

    fn intern_entity(&mut self, raw: &str) {
        let key = raw.to_lowercase();
        if key.trim().is_empty() {
            return;
        }
        if !self.name_to_idx.contains_key(&key) {
            self.name_to_idx
                .insert(key.clone(), self.entity_names.len());
            self.entity_names.push(raw.to_string());
        }
    }
}

fn parse_entities(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json)
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn add_or_accumulate_edge(adj: &mut Vec<(usize, f32)>, target: usize, weight: f32) {
    if let Some(existing) = adj.iter_mut().find(|(j, _)| *j == target) {
        existing.1 += weight;
    } else {
        adj.push((target, weight));
    }
}

/// Two tokens share a `min_shared`-char common prefix. Match is
/// bidirectional: either token may be the shorter one. Used to
/// stem-bridge `Satire ↔ satirical` and `economy ↔ economics`
/// without pulling in a full Porter stemmer (~150 LOC for ~3 cases
/// we actually care about). The trade-off is occasional false
/// positives — `manager ↔ manage ↔ management` are all bridged via
/// `manag` even when only one is what the writer meant — but
/// PPR's seed restart vector treats false-positive seeds as
/// gentle additions to the diffusion, not as authoritative anchors,
/// so the cost is recall noise rather than precision collapse.
fn tokens_share_prefix(a: &str, b: &str, min_shared: usize) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let max_len = a_bytes.len().min(b_bytes.len());
    if max_len < min_shared {
        return false;
    }
    let mut shared = 0;
    while shared < max_len && a_bytes[shared] == b_bytes[shared] {
        shared += 1;
    }
    shared >= min_shared
}

/// Word-boundary substring match: `needle` matches in `haystack`
/// only if the surrounding characters are non-alphanumeric (or
/// edges). Both strings should already be lower-cased.
fn is_word_boundary_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let needle_bytes = needle.as_bytes();
    let haystack_bytes = haystack.as_bytes();
    let mut start = 0usize;
    while start + needle_bytes.len() <= haystack_bytes.len() {
        if let Some(found) = haystack[start..].find(needle) {
            let i = start + found;
            let j = i + needle_bytes.len();
            let before_ok = i == 0 || !haystack_bytes[i - 1].is_ascii_alphanumeric();
            let after_ok = j == haystack_bytes.len() || !haystack_bytes[j].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            start = i + 1;
        } else {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_node(id: &str, entities: &[&str], coherence: f64, chunks: &[u64]) -> ConvRaptorNodeRow {
        let ents_json =
            serde_json::to_string(&entities.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap();
        let chunks_json = if chunks.is_empty() {
            None
        } else {
            Some(serde_json::to_string(chunks).unwrap())
        };
        ConvRaptorNodeRow {
            node_id: id.into(),
            corpus_id: "c".into(),
            conv_uuid: "u".into(),
            level: 0,
            summary: String::new(),
            summary_embedding: vec![],
            centroid_embedding: vec![],
            children_node_ids_json: "[]".into(),
            direct_member_chunk_ids_json: chunks_json,
            evidence_chunk_ids_json: "[]".into(),
            quote_spans_json: "[]".into(),
            primary_entities_json: ents_json,
            cluster_coherence: coherence,
            created_at: 0,
        }
    }

    #[test]
    fn empty_graph_when_no_nodes() {
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &[]);
        assert!(g.is_empty());
        assert_eq!(g.entity_count(), 0);
        assert_eq!(g.edge_count(), 0);
        assert!(g.personalized_pagerank(&[], 0.85, 10).is_empty());
    }

    #[test]
    fn single_node_yields_one_edge_per_pair() {
        let nodes = vec![mk_node("n1", &["Borges", "Labyrinth"], 0.9, &[1, 2])];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        assert_eq!(g.entity_count(), 2);
        // Conv-clique edge + RAPTOR-cluster edge sum to one undirected
        // edge (both layers contribute to the same pair).
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn conv_clique_bridges_isolated_clusters() {
        // Two separate single-entity RAPTOR nodes — without the
        // conv-clique layer, neither entity would have any edge.
        // With it, they share a baseline-weight edge and PPR mass
        // can flow between them.
        let nodes = vec![
            mk_node("n1", &["Alpha"], 0.9, &[10]),
            mk_node("n2", &["Beta"], 0.9, &[20]),
        ];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        assert_eq!(g.edge_count(), 1);
        let alpha = g.name_to_idx["alpha"];
        let beta = g.name_to_idx["beta"];
        let mass = g.personalized_pagerank(&[alpha], 0.85, 30);
        // Beta should receive *some* mass via the clique edge,
        // strictly greater than zero. Without the clique it'd stay
        // at the restart-vector value only.
        assert!(mass[beta] > 0.0, "beta got no mass: {mass:?}");
    }

    #[test]
    fn cross_node_co_occurrence_accumulates_weight() {
        let nodes = vec![
            mk_node("n1", &["A", "B"], 0.8, &[1]),
            mk_node("n2", &["A", "B"], 0.6, &[2]),
            mk_node("n3", &["A", "C"], 0.7, &[3]),
        ];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        // 3 entities → 3 conv-clique pairs (A-B, A-C, B-C) each at
        // baseline 0.1. Cluster bonds add on top: A-B += 0.8+0.6
        // (two nodes), A-C += 0.7. B-C has no cluster bond — only
        // the clique baseline survives.
        assert_eq!(g.edge_count(), 3);
        let a_idx = g.name_to_idx["a"];
        let b_idx = g.name_to_idx["b"];
        let c_idx = g.name_to_idx["c"];
        let a_b_w = g.adjacency[a_idx]
            .iter()
            .find(|(j, _)| *j == b_idx)
            .unwrap()
            .1;
        let a_c_w = g.adjacency[a_idx]
            .iter()
            .find(|(j, _)| *j == c_idx)
            .unwrap()
            .1;
        let b_c_w = g.adjacency[b_idx]
            .iter()
            .find(|(j, _)| *j == c_idx)
            .unwrap()
            .1;
        // A-B: 0.1 (clique) + 0.8 + 0.6 = 1.5
        assert!((a_b_w - 1.5).abs() < 0.001, "a_b_w = {a_b_w}");
        // A-C: 0.1 (clique) + 0.7 = 0.8
        assert!((a_c_w - 0.8).abs() < 0.001, "a_c_w = {a_c_w}");
        // B-C: 0.1 (clique only)
        assert!((b_c_w - 0.1).abs() < 0.001, "b_c_w = {b_c_w}");
    }

    #[test]
    fn seed_indices_word_boundary_match() {
        let nodes = vec![mk_node("n1", &["Borges", "Bach"], 0.9, &[1])];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        // Word-boundary for single tokens.
        assert_eq!(g.seed_indices_from_query("tell me about Borges").len(), 1);
        // Multi-entity query.
        let hits = g.seed_indices_from_query("Borges and Bach");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn seed_indices_token_split_for_multi_word_entities() {
        let nodes = vec![mk_node(
            "n1",
            &["Jonathan Swift", "Toni Morrison"],
            0.9,
            &[1],
        )];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        // Surname-only query should still hit the multi-word entity.
        assert_eq!(
            g.seed_indices_from_query("modern Swift satirical works")
                .len(),
            1
        );
        assert_eq!(g.seed_indices_from_query("Morrison's prose style").len(), 1);
        // Both surnames in one query.
        assert_eq!(g.seed_indices_from_query("Swift and Morrison").len(), 2);
    }

    #[test]
    fn seed_indices_prefix_stem_match() {
        let nodes = vec![mk_node("n1", &["Satire", "Economics", "Borges"], 0.9, &[1])];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        // Stem-bridge: `satire ↔ satirical`.
        assert_eq!(g.seed_indices_from_query("satirical scenarios").len(), 1);
        // Stem-bridge: `economics ↔ economy`.
        assert_eq!(g.seed_indices_from_query("the economy lately").len(), 1);
        // Borges still seeds via full-phrase match.
        assert_eq!(g.seed_indices_from_query("Borges' work").len(), 1);
    }

    #[test]
    fn seed_indices_no_false_positive_on_short_query_words() {
        let nodes = vec![mk_node("n1", &["Tao", "Maya"], 0.9, &[1])];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        // 3-char tokens get dropped from the comparison pool, so
        // `tao` doesn't false-positive on `taonomy`.
        assert_eq!(g.seed_indices_from_query("taonomy is a typo").len(), 0);
    }

    #[test]
    fn ppr_concentrates_mass_on_seed_in_tight_graph() {
        // Tight triangle: A-B-C-A; seed = A.
        let nodes = vec![
            mk_node("n1", &["A", "B"], 0.9, &[1]),
            mk_node("n2", &["B", "C"], 0.9, &[2]),
            mk_node("n3", &["A", "C"], 0.9, &[3]),
        ];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        let a_idx = g.name_to_idx["a"];
        let mass = g.personalized_pagerank(&[a_idx], 0.85, 30);
        // Mass on A should be the largest in the distribution.
        let max_idx = mass
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.partial_cmp(y.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(max_idx, a_idx);
        // Sum to ~1.0 (modulo dead-end correction).
        let total: f32 = mass.iter().sum();
        assert!((total - 1.0).abs() < 0.05, "total mass = {total}");
    }

    #[test]
    fn ppr_with_no_seeds_returns_uniform_walk() {
        let nodes = vec![mk_node("n1", &["A", "B", "C"], 0.9, &[1])];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        let mass = g.personalized_pagerank(&[], 0.85, 30);
        // On a triangle with all-uniform restart, mass should be ~1/3 each.
        for m in &mass {
            assert!((m - 1.0 / 3.0).abs() < 0.05, "uneven mass: {mass:?}");
        }
    }

    #[test]
    fn chunk_mass_projects_via_node_membership() {
        let nodes = vec![
            mk_node("n1", &["A", "B"], 0.9, &[10, 11]),
            mk_node("n2", &["B", "C"], 0.9, &[12]),
        ];
        let g = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        // Construct a mass vector where A gets all the mass.
        let mut mass = vec![0f32; g.entity_count()];
        let a_idx = g.name_to_idx["a"];
        mass[a_idx] = 1.0;
        let chunk_mass = g.chunk_mass(&mass);
        // A is only in n1, which owns chunks 10 + 11. Chunk 12 has no
        // A-mass.
        assert_eq!(chunk_mass.get(&10), Some(&1.0));
        assert_eq!(chunk_mass.get(&11), Some(&1.0));
        assert_eq!(chunk_mass.get(&12), None);
    }

    #[test]
    fn parse_entities_filters_empty_and_whitespace() {
        let json = r#"["A", "", "  ", "B"]"#;
        let ents = parse_entities(json);
        assert_eq!(ents, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn word_boundary_match_edges() {
        assert!(is_word_boundary_match("bach", "bach"));
        assert!(is_word_boundary_match("the bach", "bach"));
        assert!(is_word_boundary_match("bach is here", "bach"));
        assert!(!is_word_boundary_match("bachelor", "bach"));
        assert!(!is_word_boundary_match("rebach", "bach"));
        assert!(is_word_boundary_match("read bach's fugue", "bach"));
        assert!(!is_word_boundary_match("", "bach"));
    }

    fn mk_chunk_entity(chunk_id: u64, text: &str, label: &str) -> ChunkEntityRow {
        ChunkEntityRow {
            corpus_id: "c".into(),
            chunk_id,
            text: text.into(),
            label: label.into(),
            char_start: 0,
            char_end: text.len() as i64,
            score: 0.9,
            conv_uuid: Some("u".into()),
            extracted_at: 0,
        }
    }

    #[test]
    fn from_chunk_entities_empty_returns_empty_graph() {
        let g = ConvEntityGraph::from_chunk_entities("c", "u", &[]);
        assert!(g.is_empty());
        assert_eq!(g.entity_count(), 0);
    }

    #[test]
    fn from_chunk_entities_single_chunk_pair() {
        let rows = vec![
            mk_chunk_entity(10, "Borges", "Person"),
            mk_chunk_entity(10, "Labyrinth", "Concept"),
        ];
        let g = ConvEntityGraph::from_chunk_entities("c", "u", &rows);
        assert_eq!(g.entity_count(), 2);
        // Conv-clique + chunk-co-occurrence collapse onto one edge.
        assert_eq!(g.edge_count(), 1);
        // Mass projects to chunk 10.
        let mut mass = vec![0f32; g.entity_count()];
        let borges_idx = g.name_to_idx["borges"];
        mass[borges_idx] = 1.0;
        let chunk_mass = g.chunk_mass(&mass);
        assert_eq!(chunk_mass.get(&10), Some(&1.0));
    }

    #[test]
    fn from_chunk_entities_chunk_co_occurrence_outweighs_clique() {
        // Borges + Bach appear in chunk 10 (strong edge).
        // Borges + Calvino only at the conv-clique baseline.
        let rows = vec![
            mk_chunk_entity(10, "Borges", "Person"),
            mk_chunk_entity(10, "Bach", "Person"),
            mk_chunk_entity(11, "Calvino", "Person"),
        ];
        let g = ConvEntityGraph::from_chunk_entities("c", "u", &rows);
        let borges = g.name_to_idx["borges"];
        let bach = g.name_to_idx["bach"];
        let calvino = g.name_to_idx["calvino"];
        let weight_pair = |a: usize, b: usize| -> f32 {
            g.adjacency[a]
                .iter()
                .find(|(j, _)| *j == b)
                .map(|(_, w)| *w)
                .unwrap_or(0.0)
        };
        // borges↔bach should be CONV_CLIQUE_WEIGHT + CHUNK_CO_OCCURRENCE_WEIGHT = 0.6
        // borges↔calvino only conv-clique = 0.1
        let bb = weight_pair(borges, bach);
        let bc = weight_pair(borges, calvino);
        assert!(
            bb > bc,
            "expected borges↔bach ({bb}) > borges↔calvino ({bc})"
        );
        assert!((bb - 0.6).abs() < 0.001, "bb = {bb}");
        assert!((bc - 0.1).abs() < 0.001, "bc = {bc}");
    }

    #[test]
    fn from_chunk_entities_chunk_mass_distributes_across_owning_chunks() {
        // Swift appears in chunks 10 + 12. Bach only in chunk 12.
        let rows = vec![
            mk_chunk_entity(10, "Swift", "Person"),
            mk_chunk_entity(12, "Swift", "Person"),
            mk_chunk_entity(12, "Bach", "Person"),
        ];
        let g = ConvEntityGraph::from_chunk_entities("c", "u", &rows);
        // Seed mass on Swift only.
        let mut mass = vec![0f32; g.entity_count()];
        let swift = g.name_to_idx["swift"];
        mass[swift] = 1.0;
        let chunk_mass = g.chunk_mass(&mass);
        // Swift is in both chunks 10 + 12 → both get the mass.
        assert_eq!(chunk_mass.get(&10), Some(&1.0));
        assert_eq!(chunk_mass.get(&12), Some(&1.0));
    }

    #[test]
    fn from_layered_empty_inputs_yields_empty_graph() {
        let g = ConvEntityGraph::from_layered("c", "u", &[], &[]);
        assert!(g.is_empty());
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn from_layered_collapses_to_raptor_only_when_no_chunk_entities() {
        let nodes = vec![mk_node("n1", &["A", "B"], 0.8, &[10, 11])];
        let layered = ConvEntityGraph::from_layered("c", "u", &nodes, &[]);
        let raptor_only = ConvEntityGraph::from_raptor_nodes("c", "u", &nodes);
        assert_eq!(layered.entity_count(), raptor_only.entity_count());
        assert_eq!(layered.edge_count(), raptor_only.edge_count());
    }

    #[test]
    fn from_layered_collapses_to_chunk_entities_only_when_no_raptor() {
        let rows = vec![
            mk_chunk_entity(10, "A", "Person"),
            mk_chunk_entity(10, "B", "Person"),
        ];
        let layered = ConvEntityGraph::from_layered("c", "u", &[], &rows);
        let chunk_only = ConvEntityGraph::from_chunk_entities("c", "u", &rows);
        assert_eq!(layered.entity_count(), chunk_only.entity_count());
        assert_eq!(layered.edge_count(), chunk_only.edge_count());
    }

    #[test]
    fn from_layered_cross_source_agreement_accumulates_weight() {
        // Entity pair (Swift, Satire) co-occurs in BOTH:
        //   - RAPTOR leaf cluster with coherence 0.8
        //   - GliNER chunk 10
        // Conv-clique baseline 0.1. RAPTOR adds 0.8. GliNER adds
        // 0.5. Total expected edge: 1.4.
        let raptor_nodes = vec![mk_node("n1", &["Swift", "Satire"], 0.8, &[10])];
        let chunk_rows = vec![
            mk_chunk_entity(10, "Swift", "Person"),
            mk_chunk_entity(10, "Satire", "Concept"),
        ];
        let g = ConvEntityGraph::from_layered("c", "u", &raptor_nodes, &chunk_rows);
        let swift = g.name_to_idx["swift"];
        let satire = g.name_to_idx["satire"];
        let w = g.adjacency[swift]
            .iter()
            .find(|(j, _)| *j == satire)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);
        assert!((w - 1.4).abs() < 0.001, "edge weight = {w} (expected 1.4)");
    }

    #[test]
    fn from_layered_chunk_only_entity_still_seeds_via_clique_to_raptor_entity() {
        // RAPTOR has only Borges. GliNER catches Borges + Calvino
        // in same chunk. Calvino should reach Borges via chunk
        // co-occurrence + Borges → other RAPTOR nodes still
        // accessible. PPR seeded on Calvino should reach Borges.
        let raptor_nodes = vec![mk_node("n1", &["Borges"], 0.9, &[10])];
        let chunk_rows = vec![
            mk_chunk_entity(10, "Borges", "Person"),
            mk_chunk_entity(10, "Calvino", "Person"),
        ];
        let g = ConvEntityGraph::from_layered("c", "u", &raptor_nodes, &chunk_rows);
        assert_eq!(g.entity_count(), 2);
        let calvino = g.name_to_idx["calvino"];
        let mass = g.personalized_pagerank(&[calvino], 0.85, 30);
        let borges_mass = mass[g.name_to_idx["borges"]];
        assert!(borges_mass > 0.0, "Borges got zero mass from Calvino seed");
    }

    #[test]
    fn from_chunk_entities_dedupes_within_chunk() {
        // Same entity text mentioned twice in one chunk shouldn't
        // double-count its node membership.
        let rows = vec![
            mk_chunk_entity(10, "Borges", "Person"),
            mk_chunk_entity(10, "Borges", "Person"),
            mk_chunk_entity(10, "Borges", "Work"),
        ];
        let g = ConvEntityGraph::from_chunk_entities("c", "u", &rows);
        assert_eq!(g.entity_count(), 1);
        let mut mass = vec![0f32; 1];
        mass[0] = 1.0;
        let chunk_mass = g.chunk_mass(&mass);
        // Even though three rows reference Borges + chunk 10, the
        // chunk's accumulated mass should still be 1.0 (single
        // entity_to_nodes entry).
        assert_eq!(chunk_mass.get(&10), Some(&1.0));
    }
}
