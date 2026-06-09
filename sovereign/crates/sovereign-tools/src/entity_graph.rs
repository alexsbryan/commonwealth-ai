// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-document entity graph + Personalized PageRank as an additional
//! retrieval ranking signal.
//!
//! # Why
//!
//! Embedding cosine surfaces chunks similar to the query text; it
//! cannot traverse entity relationships. A question like "what
//! happens to X after Y?" requires multi-hop reasoning over the
//! document's entity structure — embedding alone misses the chunk
//! that mentions Y's consequence under a different name.
//!
//! HippoRAG (NeurIPS '24 / ICML '25) demonstrated that running
//! Personalized PageRank over an entity graph extracted from the
//! document approximates multi-hop retrieval in a single ranking
//! step. This module is a lean version of that idea built on data
//! we already have: `DocumentSkeleton.actions` (entity-verb-object
//! triples with chunk anchors) plus `DocumentSkeleton.entity_index`
//! (entity → chunk indices).
//!
//! # What's invisible to the model
//!
//! Per the prompt-parsimony lesson — small models are sensitive to
//! prompt bloat — this module never surfaces in the briefing. It
//! exists solely as a re-ranking signal inside
//! `attached_document_search::execute`. The model sees better-ranked
//! chunks; it doesn't see the graph or know PPR ran.
//!
//! # Cost
//!
//! Zero new LLM calls at ingest (uses existing skeleton data). At
//! query time: ~5ms graph build + ~15ms PPR + ~1ms chunk projection
//! for a typical document. Negligible next to the existing cosine
//! pass.

use std::collections::HashMap;

use sovereign_core::types::DocumentSkeleton;

/// Triple-bonus weight added to entity-entity edges when the
/// connection comes from an action atom (one entity is the actor,
/// the other is the object). Co-occurrence-only edges have weight
/// 1.0; triple-anchored edges get this extra weight on top.
const TRIPLE_BONUS_WEIGHT: f32 = 2.0;

/// Default damping factor for PPR. Standard PageRank uses 0.85.
pub const DEFAULT_DAMPING: f32 = 0.85;

/// Default convergence iteration cap. PPR typically converges in
/// 20-30 iterations on graphs this size.
pub const DEFAULT_MAX_ITERS: usize = 30;

/// Default L1 convergence threshold for early termination.
pub const DEFAULT_EPSILON: f32 = 1e-4;

/// Sparse entity graph built from a document's skeleton. Nodes are
/// entities (one per distinct name in `entity_index`). Edges are
/// weighted by entity co-occurrence in the same chunk plus a bonus
/// when the pair appears in an `actions` triple.
#[derive(Debug, Clone)]
pub struct EntityGraph {
    /// Canonical entity name → compact node id.
    name_to_id: HashMap<String, u32>,
    /// Reverse mapping. `id_to_name[id]` is the canonical name.
    id_to_name: Vec<String>,
    /// Adjacency list: `entity_edges[node] = Vec<(neighbor, weight)>`.
    /// Symmetric — edges are stored on both endpoints.
    entity_edges: Vec<Vec<(u32, f32)>>,
    /// Per-entity chunk indices. `entity_chunks[node]` is the list
    /// of chunks where this entity appears (deduped, sorted).
    entity_chunks: Vec<Vec<u32>>,
}

impl EntityGraph {
    /// Build a graph from the skeleton. Returns an empty graph (zero
    /// nodes) if the skeleton has no entities — caller treats that
    /// as "PPR not applicable, fall back to pure cosine."
    pub fn build(skeleton: &DocumentSkeleton) -> Self {
        // 1) Assign node ids to every distinct entity name in
        // `entity_index`. Names are case-sensitive — the skeleton
        // builder already normalises them.
        let mut name_to_id: HashMap<String, u32> = HashMap::new();
        let mut id_to_name: Vec<String> = Vec::new();
        for name in skeleton.entity_index.keys() {
            let id = name_to_id.len() as u32;
            name_to_id.insert(name.clone(), id);
            id_to_name.push(name.clone());
        }
        let n = id_to_name.len();
        if n == 0 {
            return Self {
                name_to_id,
                id_to_name,
                entity_edges: Vec::new(),
                entity_chunks: Vec::new(),
            };
        }

        // 2) Per-entity chunk lists, deduped + sorted.
        let mut entity_chunks: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (name, app) in &skeleton.entity_index {
            if let Some(&id) = name_to_id.get(name) {
                let mut chunks: Vec<u32> = app.chunk_indices.iter().map(|i| *i as u32).collect();
                chunks.sort_unstable();
                chunks.dedup();
                entity_chunks[id as usize] = chunks;
            }
        }

        // 3) Co-occurrence weights: for each chunk, every pair of
        // entities appearing in it gets +1 on the edge between them.
        // Build a chunk → [entity_ids] map first, then walk pairs.
        let mut chunk_to_entities: HashMap<u32, Vec<u32>> = HashMap::new();
        for (id, chunks) in entity_chunks.iter().enumerate() {
            for &c in chunks {
                chunk_to_entities.entry(c).or_default().push(id as u32);
            }
        }
        // Dedup per chunk so the same entity doesn't double-edge itself.
        for ents in chunk_to_entities.values_mut() {
            ents.sort_unstable();
            ents.dedup();
        }
        let mut edge_weights: HashMap<(u32, u32), f32> = HashMap::new();
        for ents in chunk_to_entities.values() {
            for i in 0..ents.len() {
                for j in (i + 1)..ents.len() {
                    let (a, b) = (ents[i], ents[j]);
                    let key = if a < b { (a, b) } else { (b, a) };
                    *edge_weights.entry(key).or_insert(0.0) += 1.0;
                }
            }
        }

        // 4) Triple bonuses: for every action whose `object` resolves
        // to a known entity, add TRIPLE_BONUS_WEIGHT to the edge
        // between the action's actor and the object entity. The
        // resolver is case-insensitive substring match — actions
        // emit free-text objects so a strict equality match would
        // miss most. False positives are bounded by TRIPLE_BONUS_WEIGHT
        // being modest.
        let id_lower: Vec<String> = id_to_name.iter().map(|s| s.to_lowercase()).collect();
        for action in &skeleton.actions {
            let actor_id = name_to_id.get(&action.entity).copied();
            let object_lower = action.object.to_lowercase();
            if let Some(a_id) = actor_id {
                for (b_id, other_lower) in id_lower.iter().enumerate() {
                    let b_id = b_id as u32;
                    if b_id == a_id {
                        continue;
                    }
                    // Require non-trivial overlap (avoid matching "a" or "the").
                    if other_lower.len() < 3 {
                        continue;
                    }
                    if object_lower.contains(other_lower.as_str()) {
                        let key = if a_id < b_id {
                            (a_id, b_id)
                        } else {
                            (b_id, a_id)
                        };
                        *edge_weights.entry(key).or_insert(0.0) += TRIPLE_BONUS_WEIGHT;
                    }
                }
            }
        }

        // 5) Materialise the symmetric adjacency lists.
        let mut entity_edges: Vec<Vec<(u32, f32)>> = vec![Vec::new(); n];
        for ((a, b), w) in edge_weights {
            entity_edges[a as usize].push((b, w));
            entity_edges[b as usize].push((a, w));
        }

        Self {
            name_to_id,
            id_to_name,
            entity_edges,
            entity_chunks,
        }
    }

    /// Whether the graph has any nodes. Callers use this to short-
    /// circuit PPR when the skeleton lacks entity data.
    pub fn is_empty(&self) -> bool {
        self.id_to_name.is_empty()
    }

    /// Number of entity nodes. Useful for tests + debug.
    pub fn entity_count(&self) -> usize {
        self.id_to_name.len()
    }

    /// Resolve query → seed entity ids via case-insensitive substring
    /// match against known entity names. Each name in the graph that
    /// appears in the query (whole-word or substring) becomes a seed.
    /// Returns empty if no entity name appears — caller treats that
    /// as "no multi-hop signal available, skip PPR."
    pub fn seeds_from_query(&self, query: &str) -> Vec<u32> {
        if self.is_empty() {
            return Vec::new();
        }
        let q_lower = query.to_lowercase();
        let mut seeds: Vec<u32> = Vec::new();
        for (id, name) in self.id_to_name.iter().enumerate() {
            let name_lower = name.to_lowercase();
            // Skip very short names — "Mr" or "A" would over-seed.
            if name_lower.len() < 3 {
                continue;
            }
            if q_lower.contains(&name_lower) {
                seeds.push(id as u32);
            }
        }
        seeds
    }

    /// Run Personalized PageRank with teleport probability mass
    /// concentrated on the given seed nodes.
    ///
    /// Standard PPR iteration:
    /// ```text
    /// PR_t+1[v] = (1 - damping) * teleport[v]
    ///           + damping * sum(PR_t[u] * w(u,v) / out_weight(u) for u in neighbors(v))
    /// ```
    ///
    /// `teleport[v] = 1/|seeds|` for v in seeds, 0 otherwise.
    ///
    /// Returns the steady-state PR vector (one score per entity).
    /// Returns an empty vec if `seeds` is empty.
    pub fn personalized_pagerank(
        &self,
        seeds: &[u32],
        damping: f32,
        max_iters: usize,
        epsilon: f32,
    ) -> Vec<f32> {
        let n = self.id_to_name.len();
        if n == 0 || seeds.is_empty() {
            return Vec::new();
        }
        let teleport_mass = 1.0 / seeds.len() as f32;
        let mut teleport = vec![0.0f32; n];
        for &s in seeds {
            if (s as usize) < n {
                teleport[s as usize] = teleport_mass;
            }
        }
        // Out-weight per node (sum of edge weights). Used to normalise
        // the contribution each node makes to its neighbours.
        let out_weight: Vec<f32> = self
            .entity_edges
            .iter()
            .map(|adj| adj.iter().map(|(_, w)| w).sum())
            .collect();

        let mut pr = teleport.clone();
        let mut next = vec![0.0f32; n];
        for _ in 0..max_iters {
            // Teleport component.
            for i in 0..n {
                next[i] = (1.0 - damping) * teleport[i];
            }
            // Random-walk component: each node distributes its score
            // proportionally to edge weights.
            for u in 0..n {
                let ow = out_weight[u];
                if ow <= f32::EPSILON {
                    // Dead-end node: distribute its mass uniformly back
                    // to teleport so probability is conserved.
                    let dead_mass = damping * pr[u] / n as f32;
                    for slot in next.iter_mut() {
                        *slot += dead_mass;
                    }
                    continue;
                }
                let pr_u = pr[u];
                for &(v, w) in &self.entity_edges[u] {
                    next[v as usize] += damping * pr_u * (w / ow);
                }
            }
            // Convergence check (L1 norm of diff).
            let mut delta = 0.0f32;
            for i in 0..n {
                delta += (next[i] - pr[i]).abs();
            }
            std::mem::swap(&mut pr, &mut next);
            if delta < epsilon {
                break;
            }
        }
        pr
    }

    /// Project per-entity PPR scores onto per-chunk scores: each
    /// chunk's score is the sum of PPR for entities appearing in it.
    /// `chunk_count` bounds the output vector length (chunks beyond
    /// it are silently ignored).
    pub fn score_chunks(&self, ppr: &[f32], chunk_count: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; chunk_count];
        for (id, chunks) in self.entity_chunks.iter().enumerate() {
            let score = ppr.get(id).copied().unwrap_or(0.0);
            if score <= 0.0 {
                continue;
            }
            for &c in chunks {
                if (c as usize) < chunk_count {
                    out[c as usize] += score;
                }
            }
        }
        out
    }
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::types::{ActionAtom, DocumentSkeleton, EntityAppearances};

    fn skel() -> DocumentSkeleton {
        // 4 entities, 5 chunks. Designed so Winnie and Ossipon do
        // NOT co-occur — the only path between them is via Verloc.
        // Co-occurrence:
        // - Winnie + Verloc: chunks 0, 1 (weight 2)
        // - Winnie + Stevie: chunks 1, 2 (weight 2)
        // - Verloc + Stevie: chunk 1 (weight 1)
        // - Verloc + Ossipon: chunk 3 (weight 1)
        // - Ossipon alone in chunk 4
        // Plus actions: Winnie killed Verloc; Verloc misled Stevie.
        let mut entity_index = std::collections::HashMap::new();
        entity_index.insert(
            "Winnie".to_string(),
            EntityAppearances {
                chunk_indices: vec![0, 1, 2],
                quote_samples: vec![],
            },
        );
        entity_index.insert(
            "Verloc".to_string(),
            EntityAppearances {
                chunk_indices: vec![0, 1, 3],
                quote_samples: vec![],
            },
        );
        entity_index.insert(
            "Stevie".to_string(),
            EntityAppearances {
                chunk_indices: vec![1, 2],
                quote_samples: vec![],
            },
        );
        entity_index.insert(
            "Ossipon".to_string(),
            EntityAppearances {
                chunk_indices: vec![3, 4],
                quote_samples: vec![],
            },
        );
        DocumentSkeleton {
            sections: vec![],
            main_entities: vec![],
            entity_index,
            structural_moments: vec![],
            overview: String::new(),
            actions: vec![
                ActionAtom {
                    entity: "Winnie".to_string(),
                    verb: "killed".to_string(),
                    object: "Verloc".to_string(),
                    chunk_index: 1,
                    evidence: "she killed him".to_string(),
                },
                ActionAtom {
                    entity: "Verloc".to_string(),
                    verb: "misled".to_string(),
                    object: "Stevie".to_string(),
                    chunk_index: 1,
                    evidence: "Verloc misled Stevie".to_string(),
                },
            ],
            segments: vec![],
            built_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn build_creates_node_per_entity() {
        let g = EntityGraph::build(&skel());
        assert_eq!(g.entity_count(), 4);
        assert!(!g.is_empty());
    }

    #[test]
    fn empty_skeleton_yields_empty_graph() {
        let mut s = skel();
        s.entity_index.clear();
        s.actions.clear();
        let g = EntityGraph::build(&s);
        assert!(g.is_empty());
        assert_eq!(g.entity_count(), 0);
    }

    #[test]
    fn coocurrence_creates_symmetric_edges() {
        let g = EntityGraph::build(&skel());
        // Winnie and Verloc co-occur in chunks 0, 1 (weight 2)
        // plus triple bonus from "Winnie killed Verloc" (+2.0) = 4.0.
        let winnie_id = *g.name_to_id.get("Winnie").unwrap();
        let verloc_id = *g.name_to_id.get("Verloc").unwrap();
        let w_to_v: f32 = g.entity_edges[winnie_id as usize]
            .iter()
            .find(|(n, _)| *n == verloc_id)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);
        let v_to_w: f32 = g.entity_edges[verloc_id as usize]
            .iter()
            .find(|(n, _)| *n == winnie_id)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);
        assert!(w_to_v > 0.0, "Winnie should have edge to Verloc");
        assert_eq!(w_to_v, v_to_w, "edges must be symmetric");
        // 2 co-occurrences + 2.0 triple bonus = 4.0.
        assert!(w_to_v >= 2.0, "expected co-occ ≥ 2, got {w_to_v}");
        assert!(
            w_to_v >= 4.0,
            "expected triple bonus to push weight ≥ 4, got {w_to_v}"
        );
    }

    #[test]
    fn seeds_from_query_matches_substring() {
        let g = EntityGraph::build(&skel());
        let seeds = g.seeds_from_query("What happened to Winnie at the end?");
        assert_eq!(seeds.len(), 1);
        let seed_name = &g.id_to_name[seeds[0] as usize];
        assert_eq!(seed_name, "Winnie");
    }

    #[test]
    fn seeds_from_query_returns_multiple_when_query_names_multiple() {
        let g = EntityGraph::build(&skel());
        let seeds = g.seeds_from_query("Verloc and Stevie in the kitchen");
        let names: Vec<&str> = seeds
            .iter()
            .map(|i| g.id_to_name[*i as usize].as_str())
            .collect();
        assert!(names.contains(&"Verloc"));
        assert!(names.contains(&"Stevie"));
    }

    #[test]
    fn ppr_concentrates_mass_on_seed_neighborhood() {
        let g = EntityGraph::build(&skel());
        let winnie_id = *g.name_to_id.get("Winnie").unwrap();
        let ossipon_id = *g.name_to_id.get("Ossipon").unwrap();
        let ppr = g.personalized_pagerank(&[winnie_id], 0.85, 30, 1e-4);
        assert_eq!(ppr.len(), 4);
        // Sum should be ~1.0 (probability conservation).
        let sum: f32 = ppr.iter().sum();
        assert!(
            (sum - 1.0).abs() < 0.01,
            "PPR sum should be ~1.0, got {sum}"
        );
        // Winnie self should be the highest (seed).
        let winnie_score = ppr[winnie_id as usize];
        let ossipon_score = ppr[ossipon_id as usize];
        assert!(
            winnie_score > ossipon_score,
            "seed (Winnie, {winnie_score}) should outrank distant neighbour (Ossipon, {ossipon_score})"
        );
    }

    #[test]
    fn ppr_walks_through_intermediate_entities() {
        // Winnie has no direct edge to Ossipon (they don't co-occur).
        // But Winnie ↔ Verloc ↔ Ossipon (Verloc and Ossipon co-occur in chunk 4).
        // After PPR, Ossipon should get non-zero score via 2-hop walk.
        let g = EntityGraph::build(&skel());
        let winnie_id = *g.name_to_id.get("Winnie").unwrap();
        let ossipon_id = *g.name_to_id.get("Ossipon").unwrap();
        // Verify no direct edge.
        let direct_w_to_o: f32 = g.entity_edges[winnie_id as usize]
            .iter()
            .find(|(n, _)| *n == ossipon_id)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);
        assert_eq!(
            direct_w_to_o, 0.0,
            "Winnie should have NO direct edge to Ossipon"
        );
        // But PPR should still surface Ossipon via Verloc.
        let ppr = g.personalized_pagerank(&[winnie_id], 0.85, 30, 1e-4);
        let ossipon_score = ppr[ossipon_id as usize];
        assert!(
            ossipon_score > 0.01,
            "PPR should walk Winnie → Verloc → Ossipon; got Ossipon score {ossipon_score}"
        );
    }

    #[test]
    fn score_chunks_sums_entity_scores() {
        let g = EntityGraph::build(&skel());
        // Manually craft a PPR-like vector and verify projection.
        // Set Winnie to 0.5, others to 0.
        let mut ppr = vec![0.0f32; g.entity_count()];
        let winnie_id = *g.name_to_id.get("Winnie").unwrap();
        ppr[winnie_id as usize] = 0.5;
        let chunk_scores = g.score_chunks(&ppr, 5);
        // Winnie appears in chunks 0, 1, 2 → those get 0.5; 3 and 4 are 0.
        assert_eq!(chunk_scores[0], 0.5);
        assert_eq!(chunk_scores[1], 0.5);
        assert_eq!(chunk_scores[2], 0.5);
        assert_eq!(chunk_scores[3], 0.0);
        assert_eq!(chunk_scores[4], 0.0);
    }

    #[test]
    fn empty_seeds_returns_empty_ppr() {
        let g = EntityGraph::build(&skel());
        let ppr = g.personalized_pagerank(&[], 0.85, 30, 1e-4);
        assert!(ppr.is_empty());
    }
}
