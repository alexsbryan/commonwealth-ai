//! Curated atom-graph "landscape" subgraph for the desktop Atlas **Map** view.
//!
//! Turns a corpus's typed-atom atlas (`atoms.json` + `edges.json`) into a
//! `{ nodes, edges }` payload the desktop's force-directed `AtlasGraph`
//! renders — foregrounding the epistemic structure (`Tension` "fault lines",
//! `ArgumentReconstruction`s, open `Question`s) and capping node count so a
//! large corpus reads as a *map*, not a hairball.
//!
//! The pure shaping ([`build_subgraph`]) works over light `NodeIn`/`EdgeIn`
//! inputs so it's trivially unit-testable; [`FileAtlasReader::subgraph`] maps
//! the heavy [`AtomSummary`]/[`Edge`] structs into those inputs by reusing the
//! existing `list_atoms` (type/name/salience mapping) + `read_atlas_edges`.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomType, ResolutionStatus};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgeType};
use corpus_engine::enrichment::atlas::read_atlas_edges;

use super::atom_browse::{cached_atoms, AtomBrowseError, AtomFilter, PageCursor};
use super::reader::FileAtlasReader;

/// Light node input — just the fields the shaping needs, decoupled from the
/// full `AtomSummary` so tests don't have to build evidence/stable-key/etc.
#[derive(Debug, Clone)]
pub struct NodeIn {
    pub id: AtomId,
    pub atom_type: AtomType,
    pub label: String,
    pub salience: Option<f32>,
}

/// Light edge input — `crux` is already extracted from a `Tension` edge's
/// `sub_question` by the caller.
#[derive(Debug, Clone)]
pub struct EdgeIn {
    pub source: AtomId,
    pub target: AtomId,
    pub edge_type: EdgeType,
    pub crux: Option<String>,
}

/// A node in the landscape map — one atom, with its in-corpus degree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasNode {
    pub id: AtomId,
    pub label: String,
    pub atom_type: AtomType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salience: Option<f32>,
    pub degree: u32,
}

/// An edge — `crux` is the disagreement a `Tension` turns on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasEdge {
    pub source: AtomId,
    pub target: AtomId,
    pub edge_type: EdgeType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crux: Option<String>,
}

/// Corpus-wide totals (true totals, not just what survived the node cap), so
/// the UI can show "118 atoms · 6 tensions · 17 questions · 12 arguments".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubgraphCensus {
    pub atom_total: u32,
    pub shown: u32,
    pub tensions: u32,
    pub questions: u32,
    pub arguments: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtlasSubgraph {
    pub nodes: Vec<AtlasNode>,
    pub edges: Vec<AtlasEdge>,
    pub census: SubgraphCensus,
}

/// Default cap on rendered nodes. Above this a force layout becomes a
/// hairball; we keep the epistemic spine (tension endpoints + every argument
/// + every question) plus the highest-salience/most-connected remainder.
pub const DEFAULT_MAX_NODES: usize = 280;

/// Pure shaping. Curates `nodes` down to `max_nodes`, always keeping the
/// endpoints of `Tension` edges + every `ArgumentReconstruction` and
/// `Question` (the debate's spine), then filling by salience then degree.
/// Edges are kept when both endpoints survive. The census reflects the full
/// corpus, not just the kept subset.
pub fn build_subgraph(nodes: &[NodeIn], edges: &[EdgeIn], max_nodes: usize) -> AtlasSubgraph {
    let id_set: HashSet<&AtomId> = nodes.iter().map(|n| &n.id).collect();

    // Degree over edges whose both endpoints are real atoms.
    let mut degree: HashMap<&AtomId, u32> = HashMap::new();
    for e in edges {
        if id_set.contains(&e.source) && id_set.contains(&e.target) {
            *degree.entry(&e.source).or_default() += 1;
            *degree.entry(&e.target).or_default() += 1;
        }
    }

    // Spine: endpoints of every Tension edge.
    let mut spine: HashSet<&AtomId> = HashSet::new();
    for e in edges {
        if matches!(e.edge_type, EdgeType::Tension) {
            spine.insert(&e.source);
            spine.insert(&e.target);
        }
    }

    // Rank: spine + arguments + questions float to the top, then salience,
    // then degree. Keep the top `max_nodes`.
    let score = |n: &NodeIn| -> f64 {
        let on_spine = spine.contains(&n.id)
            || matches!(
                n.atom_type,
                AtomType::ArgumentReconstruction | AtomType::Question
            );
        let boost = if on_spine { 1e9 } else { 0.0 };
        boost + n.salience.unwrap_or(0.0) as f64 * 1000.0
            + *degree.get(&n.id).unwrap_or(&0) as f64
    };
    let mut ranked: Vec<&NodeIn> = nodes.iter().collect();
    ranked.sort_by(|a, b| {
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(max_nodes.max(1));
    let kept: HashSet<&AtomId> = ranked.iter().map(|n| &n.id).collect();

    let out_nodes: Vec<AtlasNode> = ranked
        .iter()
        .map(|n| AtlasNode {
            id: n.id.clone(),
            label: n.label.clone(),
            atom_type: n.atom_type,
            salience: n.salience,
            degree: *degree.get(&n.id).unwrap_or(&0),
        })
        .collect();

    let out_edges: Vec<AtlasEdge> = edges
        .iter()
        .filter(|e| kept.contains(&e.source) && kept.contains(&e.target))
        .map(|e| AtlasEdge {
            source: e.source.clone(),
            target: e.target.clone(),
            edge_type: e.edge_type.clone(),
            crux: e.crux.clone(),
        })
        .collect();

    let census = SubgraphCensus {
        atom_total: nodes.len() as u32,
        shown: out_nodes.len() as u32,
        tensions: edges
            .iter()
            .filter(|e| matches!(e.edge_type, EdgeType::Tension))
            .count() as u32,
        questions: nodes
            .iter()
            .filter(|n| matches!(n.atom_type, AtomType::Question))
            .count() as u32,
        arguments: nodes
            .iter()
            .filter(|n| matches!(n.atom_type, AtomType::ArgumentReconstruction))
            .count() as u32,
    };

    AtlasSubgraph {
        nodes: out_nodes,
        edges: out_edges,
        census,
    }
}

impl FileAtlasReader {
    /// Build the curated landscape subgraph for one corpus. Reuses
    /// `list_atoms` for the atom→(type/name/salience) mapping + cache, then
    /// reads `edges.json` and shapes via [`build_subgraph`].
    pub async fn subgraph(
        &self,
        corpus_id: &str,
        max_nodes: usize,
    ) -> Result<AtlasSubgraph, AtomBrowseError> {
        // All atoms as summaries (one big page — atoms.json is already cached).
        let page = self
            .list_atoms(
                corpus_id,
                AtomFilter::default(),
                PageCursor {
                    offset: 0,
                    limit: 1_000_000,
                },
            )
            .await?;
        let atlas_dir = self
            .atlas_dir(corpus_id)
            .ok_or_else(|| AtomBrowseError::UnknownCorpus(corpus_id.to_string()))?;
        let (edges_file, envelopes) = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
            let edges = read_atlas_edges(&atlas_dir)?;
            let atoms = cached_atoms(&atlas_dir)?;
            Ok((edges, atoms))
        })
        .await
        .map_err(|e| AtomBrowseError::Task(e.to_string()))?
        .map_err(AtomBrowseError::ReadAtoms)?;

        let nodes_in: Vec<NodeIn> = page
            .items
            .iter()
            .map(|s| NodeIn {
                id: s.atom_id.clone(),
                atom_type: s.atom_type,
                label: s.display_name.clone(),
                salience: s.salience,
            })
            .collect();
        let mut edges_in: Vec<EdgeIn> = edges_file
            .edges
            .iter()
            .map(|e: &Edge| EdgeIn {
                source: e.source.clone(),
                target: e.target.clone(),
                edge_type: e.edge_type.clone(),
                crux: if matches!(e.edge_type, EdgeType::Tension) {
                    e.sub_question.clone()
                } else {
                    None
                },
            })
            .collect();

        // ── Synthesize "spine" edges so Questions and Arguments aren't
        // isolated floating nodes ──────────────────────────────────────
        //
        // Materialized `edges.json` rows only ever target Claims / Entities /
        // Events / Positions (Grounds, Tension, Causes, …) — never a Question
        // or ArgumentReconstruction. Those atoms carry their relationships in
        // their own FIELDS, so without synthesis they render as disconnected
        // dots. We wire two tiers, authoritative-first:
        //
        //   1. Authoritative links, when the extractor populated them:
        //      Question → answering Claims (`addressed_by` ∪ the claim ids in
        //      `resolution_status`), Argument → `proponent`.
        //   2. Co-occurrence FALLBACK when (1) is empty — the common case on
        //      current SEP corpora, where every Question is `Open`. Two atoms
        //      citing the same evidence `chunk_id` discuss the same passage,
        //      so we link a floating Question/Argument to the most salient
        //      atoms sharing its chunks, preferring Claims/Positions (the
        //      answers being debated) over background Entities, capped at
        //      `COOCCUR_K` to keep the map a spine, not a hairball.
        //
        // Edges reuse quiet, non-Tension variants so they read as connective
        // tissue; the added degree also pulls a question's neighbours into the
        // kept set under the node cap.

        // Per-atom (type, salience) for ranking co-occurrence neighbours.
        let meta: HashMap<AtomId, (AtomType, Option<f32>)> = nodes_in
            .iter()
            .map(|n| (n.id.clone(), (n.atom_type, n.salience)))
            .collect();

        // Generic chunk-id walk — future-proof vs. matching every variant's
        // chunk-bearing fields (first_appearance / evidence / raised_at / …).
        fn collect_chunk_ids(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(m) => {
                    for (k, vv) in m {
                        if k == "chunk_id" {
                            if let serde_json::Value::String(s) = vv {
                                out.push(s.clone());
                            }
                        } else {
                            collect_chunk_ids(vv, out);
                        }
                    }
                }
                serde_json::Value::Array(a) => {
                    a.iter().for_each(|x| collect_chunk_ids(x, out))
                }
                _ => {}
            }
        }

        let mut chunk_atoms: HashMap<String, Vec<AtomId>> = HashMap::new();
        let mut atom_chunks: HashMap<AtomId, Vec<String>> = HashMap::new();
        for env in envelopes.iter() {
            let mut cids = Vec::new();
            if let Ok(v) = serde_json::to_value(env) {
                collect_chunk_ids(&v, &mut cids);
            }
            cids.sort();
            cids.dedup();
            for c in &cids {
                chunk_atoms
                    .entry(c.clone())
                    .or_default()
                    .push(env.id().clone());
            }
            atom_chunks.insert(env.id().clone(), cids);
        }

        // Rank co-occurrence neighbours: Claims/Positions first (the actual
        // answers), then argument structure, then events/states, then
        // background entities — and within a tier, by descending salience.
        fn type_rank(t: AtomType) -> u8 {
            match t {
                AtomType::Claim | AtomType::Position => 0,
                AtomType::ArgumentReconstruction | AtomType::Opposition => 1,
                AtomType::Event | AtomType::State | AtomType::Relation => 2,
                _ => 3,
            }
        }
        const COOCCUR_K: usize = 4;
        let cooccur = |aid: &AtomId| -> Vec<AtomId> {
            let mut seen: HashSet<AtomId> = HashSet::new();
            let mut cands: Vec<AtomId> = Vec::new();
            if let Some(cs) = atom_chunks.get(aid) {
                for c in cs {
                    if let Some(list) = chunk_atoms.get(c) {
                        for other in list {
                            if other != aid && seen.insert(other.clone()) {
                                cands.push(other.clone());
                            }
                        }
                    }
                }
            }
            cands.sort_by(|a, b| {
                let (ta, sa) = meta.get(a).copied().unwrap_or((AtomType::Entity, None));
                let (tb, sb) = meta.get(b).copied().unwrap_or((AtomType::Entity, None));
                type_rank(ta).cmp(&type_rank(tb)).then(
                    sb.unwrap_or(0.0)
                        .partial_cmp(&sa.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            });
            cands.truncate(COOCCUR_K);
            cands
        };

        for env in envelopes.iter() {
            match env {
                AtomEnvelope::Question(q) => {
                    // Authoritative answering claims, if the extractor linked any.
                    let mut targets: HashSet<AtomId> = q.addressed_by.iter().cloned().collect();
                    match &q.resolution_status {
                        ResolutionStatus::Resolved { claim_id } => {
                            targets.insert(claim_id.clone());
                        }
                        ResolutionStatus::Contested { claim_ids } => {
                            targets.extend(claim_ids.iter().cloned());
                        }
                        _ => {}
                    }
                    if targets.is_empty() {
                        // Fallback: co-occurrence — link to salient atoms
                        // sharing this question's passage.
                        for t in cooccur(&q.id) {
                            edges_in.push(EdgeIn {
                                source: q.id.clone(),
                                target: t,
                                edge_type: EdgeType::Involves,
                                crux: None,
                            });
                        }
                    } else {
                        for t in targets {
                            edges_in.push(EdgeIn {
                                source: q.id.clone(),
                                target: t,
                                edge_type: EdgeType::Grounds,
                                crux: None,
                            });
                        }
                    }
                }
                AtomEnvelope::ArgumentReconstruction(a) => {
                    if let Some(proponent) = &a.proponent {
                        edges_in.push(EdgeIn {
                            source: a.id.clone(),
                            target: proponent.clone(),
                            edge_type: EdgeType::Involves,
                            crux: None,
                        });
                    } else {
                        for t in cooccur(&a.id) {
                            edges_in.push(EdgeIn {
                                source: a.id.clone(),
                                target: t,
                                edge_type: EdgeType::Involves,
                                crux: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(build_subgraph(&nodes_in, &edges_in, max_nodes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(i: usize, t: AtomType, sal: Option<f32>) -> NodeIn {
        NodeIn {
            id: AtomId::entity(i),
            atom_type: t,
            label: format!("n{i}"),
            salience: sal,
        }
    }
    fn tension(a: usize, b: usize, crux: &str) -> EdgeIn {
        EdgeIn {
            source: AtomId::entity(a),
            target: AtomId::entity(b),
            edge_type: EdgeType::Tension,
            crux: Some(crux.to_string()),
        }
    }

    #[test]
    fn census_counts_full_corpus() {
        let nodes = vec![
            node(0, AtomType::Entity, Some(0.9)),
            node(1, AtomType::Claim, None),
            node(2, AtomType::Question, None),
            node(3, AtomType::ArgumentReconstruction, None),
        ];
        let edges = vec![tension(0, 1, "free will vs determinism")];
        let g = build_subgraph(&nodes, &edges, 100);
        assert_eq!(g.census.atom_total, 4);
        assert_eq!(g.census.tensions, 1);
        assert_eq!(g.census.questions, 1);
        assert_eq!(g.census.arguments, 1);
        assert_eq!(g.census.shown, 4);
        // Crux survives onto the edge.
        assert_eq!(g.edges[0].crux.as_deref(), Some("free will vs determinism"));
    }

    #[test]
    fn cap_keeps_the_spine_drops_filler() {
        // 2 low-salience entities sit in a Tension; many higher-salience
        // entities are pure filler. With a tight cap, the tension endpoints
        // + the argument + the question must survive even though their
        // salience is lower than the filler's.
        let mut nodes = vec![
            node(0, AtomType::Entity, Some(0.01)), // tension endpoint
            node(1, AtomType::Entity, Some(0.01)), // tension endpoint
            node(2, AtomType::ArgumentReconstruction, None),
            node(3, AtomType::Question, None),
        ];
        for i in 10..40 {
            nodes.push(node(i, AtomType::Entity, Some(0.9))); // high-salience filler
        }
        let edges = vec![tension(0, 1, "the crux")];
        let g = build_subgraph(&nodes, &edges, 6);
        let kept: HashSet<&AtomId> = g.nodes.iter().map(|n| &n.id).collect();
        assert!(kept.contains(&AtomId::entity(0)), "tension endpoint kept");
        assert!(kept.contains(&AtomId::entity(1)), "tension endpoint kept");
        assert!(kept.contains(&AtomId::entity(2)), "argument kept");
        assert!(kept.contains(&AtomId::entity(3)), "question kept");
        assert_eq!(g.nodes.len(), 6, "cap honored");
        // The tension edge survives because both endpoints did.
        assert_eq!(g.edges.len(), 1);
    }
}
