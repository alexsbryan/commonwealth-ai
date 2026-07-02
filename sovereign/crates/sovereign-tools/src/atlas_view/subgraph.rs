// SPDX-License-Identifier: AGPL-3.0-or-later
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
        boost + n.salience.unwrap_or(0.0) as f64 * 1000.0 + *degree.get(&n.id).unwrap_or(&0) as f64
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

        // Per-atom (type, salience), used to populate the light SpineAtoms.
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
                serde_json::Value::Array(a) => a.iter().for_each(|x| collect_chunk_ids(x, out)),
                _ => {}
            }
        }

        // Extract the envelope-coupled bits into light `SpineAtom`s, then
        // hand off to the pure, unit-tested `synthesize_spine_edges`.
        let spine_atoms: Vec<SpineAtom> = envelopes
            .iter()
            .map(|env| {
                let id = env.id().clone();
                let (atom_type, salience) =
                    meta.get(&id).copied().unwrap_or((AtomType::Entity, None));
                let mut chunks = Vec::new();
                if let Ok(v) = serde_json::to_value(env) {
                    collect_chunk_ids(&v, &mut chunks);
                }
                chunks.sort();
                chunks.dedup();
                let (role, authoritative) = match env {
                    AtomEnvelope::Question(q) => {
                        let mut auth: Vec<AtomId> = q.addressed_by.clone();
                        match &q.resolution_status {
                            ResolutionStatus::Resolved { claim_id } => auth.push(claim_id.clone()),
                            ResolutionStatus::Contested { claim_ids } => {
                                auth.extend(claim_ids.iter().cloned())
                            }
                            _ => {}
                        }
                        (SpineRole::Question, auth)
                    }
                    AtomEnvelope::ArgumentReconstruction(a) => {
                        (SpineRole::Argument, a.proponent.iter().cloned().collect())
                    }
                    _ => (SpineRole::Other, Vec::new()),
                };
                SpineAtom {
                    id,
                    atom_type,
                    salience,
                    chunks,
                    authoritative,
                    role,
                }
            })
            .collect();

        edges_in.extend(synthesize_spine_edges(&spine_atoms));

        Ok(build_subgraph(&nodes_in, &edges_in, max_nodes))
    }
}

/// How a [`SpineAtom`] participates in spine-edge synthesis — set by the
/// envelope→`SpineAtom` extraction in [`FileAtlasReader::subgraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineRole {
    /// A Question — links to its answering Claims, or (fallback) co-occurrence.
    Question,
    /// An ArgumentReconstruction — links to its proponent, or co-occurrence.
    Argument,
    /// Any other atom: a candidate co-occurrence *target*, never a source.
    Other,
}

/// Light per-atom input for spine-edge synthesis — decoupled from the full
/// atom structs (mirrors [`NodeIn`]) so [`synthesize_spine_edges`] is pure and
/// unit-testable. The messy envelope→`SpineAtom` extraction (typed field
/// access + chunk-id walk) lives in [`FileAtlasReader::subgraph`].
#[derive(Debug, Clone)]
pub struct SpineAtom {
    pub id: AtomId,
    pub atom_type: AtomType,
    pub salience: Option<f32>,
    /// Evidence chunk ids this atom cites (the co-occurrence fallback signal).
    pub chunks: Vec<String>,
    /// Authoritative outbound links, when the extractor populated them:
    /// Question → answering Claims (`addressed_by` ∪ `resolution_status`),
    /// Argument → `proponent`. Empty → fall back to co-occurrence.
    pub authoritative: Vec<AtomId>,
    pub role: SpineRole,
}

/// Max co-occurrence neighbours linked per floating Question/Argument — keeps
/// the synthesized spine sparse (a backbone, not a hairball).
const COOCCUR_K: usize = 4;

/// Co-occurrence neighbour ranking: Claims/Positions first (the actual
/// answers being debated), then argument structure, then events/states, then
/// background entities. Within a tier we order by descending salience.
fn type_rank(t: AtomType) -> u8 {
    match t {
        AtomType::Claim | AtomType::Position => 0,
        AtomType::ArgumentReconstruction | AtomType::Opposition => 1,
        AtomType::Event | AtomType::State | AtomType::Relation => 2,
        _ => 3,
    }
}

/// Synthesize "spine" edges so Questions and Arguments aren't isolated nodes.
///
/// Materialized `edges.json` rows only ever target Claims / Entities / Events
/// / Positions — never a Question or ArgumentReconstruction; those atoms carry
/// their relationships in their own fields. For each Question/Argument we emit,
/// authoritative-first:
///
///   1. Authoritative links when present — Question → answering Claims,
///      Argument → `proponent` (deduped, deterministic order).
///   2. Co-occurrence FALLBACK when (1) is empty — which, today, is nearly
///      always: across the installed corpora the extractor populates the
///      authoritative links on a literal handful of atoms, so this fallback
///      is the de-facto mechanism. Link to the top-[`COOCCUR_K`] most-salient
///      atoms sharing an evidence `chunk_id`, preferring answer-bearing types
///      via [`type_rank`]. Corpus-agnostic: every atom cites chunks, so this
///      connects floating Questions/Arguments on any atlas, not just SEP.
///
/// Edges reuse quiet, non-Tension variants (`Grounds` for a question's
/// authoritative answers, `Involves` otherwise) so they read as connective
/// tissue, not fault lines. `Other` atoms are co-occurrence *targets* only,
/// never sources.
pub fn synthesize_spine_edges(atoms: &[SpineAtom]) -> Vec<EdgeIn> {
    // (type, salience) lookup + chunk → atoms inverted index.
    let meta: HashMap<AtomId, (AtomType, Option<f32>)> = atoms
        .iter()
        .map(|a| (a.id.clone(), (a.atom_type, a.salience)))
        .collect();
    let mut chunk_atoms: HashMap<&str, Vec<&AtomId>> = HashMap::new();
    for a in atoms {
        for c in &a.chunks {
            chunk_atoms.entry(c.as_str()).or_default().push(&a.id);
        }
    }

    // Top-K salient co-occurrence neighbours sharing any of `a`'s chunks.
    let cooccur = |a: &SpineAtom| -> Vec<AtomId> {
        let mut seen: HashSet<&AtomId> = HashSet::new();
        let mut cands: Vec<&AtomId> = Vec::new();
        for c in &a.chunks {
            if let Some(list) = chunk_atoms.get(c.as_str()) {
                for other in list {
                    if **other != a.id && seen.insert(*other) {
                        cands.push(*other);
                    }
                }
            }
        }
        cands.sort_by(|x, y| {
            let (tx, sx) = meta.get(*x).copied().unwrap_or((AtomType::Entity, None));
            let (ty, sy) = meta.get(*y).copied().unwrap_or((AtomType::Entity, None));
            type_rank(tx).cmp(&type_rank(ty)).then(
                sy.unwrap_or(0.0)
                    .partial_cmp(&sx.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        cands.truncate(COOCCUR_K);
        cands.into_iter().cloned().collect()
    };

    let mut out: Vec<EdgeIn> = Vec::new();
    for a in atoms {
        let auth_edge = match a.role {
            SpineRole::Question => EdgeType::Grounds,
            SpineRole::Argument => EdgeType::Involves,
            SpineRole::Other => continue,
        };
        if a.authoritative.is_empty() {
            for t in cooccur(a) {
                out.push(EdgeIn {
                    source: a.id.clone(),
                    target: t,
                    edge_type: EdgeType::Involves,
                    crux: None,
                });
            }
        } else {
            // addressed_by ∪ resolution_status can name the same claim twice.
            let mut seen: HashSet<&AtomId> = HashSet::new();
            for t in &a.authoritative {
                if seen.insert(t) {
                    out.push(EdgeIn {
                        source: a.id.clone(),
                        target: t.clone(),
                        edge_type: auth_edge.clone(),
                        crux: None,
                    });
                }
            }
        }
    }
    out
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

    // ── spine-edge synthesis ─────────────────────────────────────────
    // Ids are all `entity(i)` (just unique keys); `atom_type`/`role` carry
    // the meaning the shaping reads.
    fn spine(
        i: usize,
        t: AtomType,
        sal: f32,
        chunks: &[&str],
        auth: &[usize],
        role: SpineRole,
    ) -> SpineAtom {
        SpineAtom {
            id: AtomId::entity(i),
            atom_type: t,
            salience: Some(sal),
            chunks: chunks.iter().map(|s| s.to_string()).collect(),
            authoritative: auth.iter().map(|&j| AtomId::entity(j)).collect(),
            role,
        }
    }

    #[test]
    fn spine_question_falls_back_to_cooccurrence_ranked_and_capped() {
        // An `Open` question (no authoritative links) + 5 claims and 1 entity
        // sharing its chunk. Claims must outrank the (higher-salience) entity
        // on type, and only COOCCUR_K survive the cap.
        let atoms = vec![
            spine(
                0,
                AtomType::Question,
                0.5,
                &["c1"],
                &[],
                SpineRole::Question,
            ),
            spine(1, AtomType::Claim, 0.9, &["c1"], &[], SpineRole::Other),
            spine(2, AtomType::Claim, 0.8, &["c1"], &[], SpineRole::Other),
            spine(3, AtomType::Claim, 0.7, &["c1"], &[], SpineRole::Other),
            spine(4, AtomType::Claim, 0.6, &["c1"], &[], SpineRole::Other),
            spine(5, AtomType::Claim, 0.5, &["c1"], &[], SpineRole::Other),
            spine(6, AtomType::Entity, 1.0, &["c1"], &[], SpineRole::Other),
        ];
        let edges = synthesize_spine_edges(&atoms);
        assert_eq!(edges.len(), COOCCUR_K, "capped at COOCCUR_K");
        assert!(edges.iter().all(|e| e.source == AtomId::entity(0)));
        assert!(edges
            .iter()
            .all(|e| matches!(e.edge_type, EdgeType::Involves)));
        let targets: HashSet<AtomId> = edges.iter().map(|e| e.target.clone()).collect();
        // Top-4 claims by salience win; the salience-1.0 entity loses on type.
        assert!(targets.contains(&AtomId::entity(1)));
        assert!(targets.contains(&AtomId::entity(4)));
        assert!(
            !targets.contains(&AtomId::entity(5)),
            "5th claim dropped by cap"
        );
        assert!(
            !targets.contains(&AtomId::entity(6)),
            "entity excluded by type rank"
        );
    }

    #[test]
    fn spine_question_authoritative_links_win_over_cooccurrence() {
        // A resolved question links to its answering claim (not even chunk-
        // shared) via Grounds, and emits NO co-occurrence edges.
        let atoms = vec![
            spine(
                0,
                AtomType::Question,
                0.5,
                &["c1"],
                &[9],
                SpineRole::Question,
            ),
            spine(1, AtomType::Claim, 0.9, &["c1"], &[], SpineRole::Other), // shares chunk; ignored
            spine(9, AtomType::Claim, 0.1, &["c2"], &[], SpineRole::Other), // the authoritative answer
        ];
        let edges = synthesize_spine_edges(&atoms);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, AtomId::entity(0));
        assert_eq!(edges[0].target, AtomId::entity(9));
        assert!(matches!(edges[0].edge_type, EdgeType::Grounds));
    }

    #[test]
    fn spine_argument_uses_proponent_then_cooccurrence() {
        // Named proponent → single Involves edge to it.
        let with_prop = vec![
            spine(
                0,
                AtomType::ArgumentReconstruction,
                0.5,
                &["c1"],
                &[7],
                SpineRole::Argument,
            ),
            spine(7, AtomType::Entity, 0.9, &["c2"], &[], SpineRole::Other),
        ];
        let e = synthesize_spine_edges(&with_prop);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].target, AtomId::entity(7));
        assert!(matches!(e[0].edge_type, EdgeType::Involves));

        // Anonymous argument → co-occurrence fallback (also Involves).
        let anon = vec![
            spine(
                0,
                AtomType::ArgumentReconstruction,
                0.5,
                &["c1"],
                &[],
                SpineRole::Argument,
            ),
            spine(1, AtomType::Claim, 0.9, &["c1"], &[], SpineRole::Other),
        ];
        let e2 = synthesize_spine_edges(&anon);
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].target, AtomId::entity(1));
        assert!(matches!(e2[0].edge_type, EdgeType::Involves));
    }

    #[test]
    fn spine_other_atoms_never_source_and_isolated_yields_nothing() {
        // A question with no chunks and no authoritative links is genuinely
        // isolated; a plain Claim is never a synthesis source.
        let atoms = vec![
            spine(0, AtomType::Question, 0.5, &[], &[], SpineRole::Question),
            spine(1, AtomType::Claim, 0.9, &["c1"], &[], SpineRole::Other),
        ];
        assert!(synthesize_spine_edges(&atoms).is_empty());
    }

    #[test]
    fn spine_authoritative_targets_are_deduped() {
        // addressed_by ∪ resolution_status can name the same claim repeatedly.
        let atoms = vec![
            spine(
                0,
                AtomType::Question,
                0.5,
                &[],
                &[9, 9, 9],
                SpineRole::Question,
            ),
            spine(9, AtomType::Claim, 0.1, &[], &[], SpineRole::Other),
        ];
        let edges = synthesize_spine_edges(&atoms);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, AtomId::entity(9));
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
