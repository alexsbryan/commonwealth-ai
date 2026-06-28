// SPDX-License-Identifier: AGPL-3.0-or-later
//! ATLAS_STORAGE_V2 Increment A — the isolated ANN-seeding gate (eval-only).
//!
//! Proves that seeding `atlas_navigate` from a co-located vector ANN (the v2
//! mechanism — nearest returns atom-ids directly, no `resolve_atom_id_from_entry`
//! reverse-scan) yields the same retrieval as v1's exact-cosine-over-the-bag
//! seeding. The Lance **storage** lives in `corpus_engine::…::ann_store`
//! (`AnnSeedTable`) — this module only *orchestrates*: the Path-E join, the
//! re-scoring, and the navigate port. Selected per-question via `--atlas-seed
//! ann`; the default (`cosine`) runs the v1 path and none of this code, so the
//! production `atlas_navigate` (sovereign-core) and the daemon stay untouched.
//!
//! `atlas_navigate_ann` is a faithful port of `sovereign_core::atlas_context::
//! atlas_navigate` with exactly two substitutions: the cosine-seed step is an
//! `AnnSeedTable::nearest` query, and the per-seed `resolve` is gone (ANN hits
//! are already atom-ids). Name-match seeding, BFS, and ChunkRequest emission are
//! copied verbatim and reuse the canonical (now-`pub`) sovereign-core helpers so
//! build-time and query-time logic can never drift. ANN hits are re-scored with
//! the canonical `cosine()` so the BFS sees bit-identical seed scores — the only
//! thing under test is the ANN *ranking*, which at SEP/flat scale equals exact
//! cosine.

use std::collections::{HashMap, HashSet};

use corpus_engine::enrichment::atlas::ann_store::AnnSeedTable;
use corpus_engine::enrichment::atlas::EdgeType;
use sovereign_core::atlas_context::{
    atom_verbatim_excerpt, contains_whole_word, cosine, edge_weight, resolve_atom_id_from_entry,
    AtlasContext, AtlasGraph, ChunkRequest,
};

/// Which seed source `run_question` uses for `atlas_navigate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedMode {
    /// v1: exact cosine over the in-memory embedding bag + `resolve`.
    Cosine,
    /// v2: ANN over a co-located vector column (this experiment).
    Ann,
}

/// Which on-disk store backs the `AtlasGraph` the eval loads — the
/// ATLAS_STORAGE_V2 Increment-C reader axis (orthogonal to [`SeedMode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtlasBackend {
    /// v1: the rkyv archive (`atoms.rkyv`), or convert-on-load from `atoms.json`.
    Rkyv,
    /// v2: the columnar store (`atoms.lance` + `edges.csr`), reconstructed to an
    /// owned archive via `corpus_engine::…::store::reconstruct_archive_bytes`.
    Lance,
}

/// The atlas-seed ANN table (corpus-engine `AnnSeedTable`) + an in-memory
/// `key -> embedding` map for canonical re-scoring, built once per eval run.
pub struct AnnTable {
    _dir: tempfile::TempDir, // owns the Lance files for the table's lifetime
    table: AnnSeedTable,
    emb: HashMap<String, Vec<f32>>,
    /// Diagnostics: how many bag entries resolved to an atom-id (the Path-E
    /// join). The cosine path drops the same unresolvable entries, so a join
    /// below 100% is what the go/no-go criterion watches.
    pub resolved: usize,
    pub total: usize,
}

/// `(corpus_id, atom_id)` as one opaque Lance key — content-hash ids are unique
/// per corpus but could collide across corpora; the unit separator never appears
/// in a corpus id or atom id.
fn seed_key(corpus_id: &str, atom_id: &str) -> String {
    format!("{corpus_id}\u{1f}{atom_id}")
}
fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once('\u{1f}')
}

/// Path E join: pool every `AtlasContext` entry, resolve it to an atom-id via the
/// canonical `resolve_atom_id_from_entry`, and stand up the corpus-engine
/// `AnnSeedTable` over `(key, embedding)`. Pooling mirrors v1's pooled cosine bag.
pub async fn build_ann_table(
    atlases: &[AtlasContext],
    graphs: &[AtlasGraph],
) -> Result<AnnTable, String> {
    let graph_by_id: HashMap<&str, &AtlasGraph> =
        graphs.iter().map(|g| (g.atlas_corpus_id.as_str(), g)).collect();

    let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
    let mut emb: HashMap<String, Vec<f32>> = HashMap::new();
    let mut total = 0usize;
    for ctx in atlases {
        let Some(graph) = graph_by_id.get(ctx.atlas_corpus_id.as_str()) else {
            continue;
        };
        for entry in &ctx.entries {
            total += 1;
            if entry.embedding.is_empty() {
                continue;
            }
            let Some(atom_id) =
                resolve_atom_id_from_entry(graph, &entry.canonical_name, &entry.embed_text)
            else {
                continue;
            };
            let key = seed_key(&ctx.atlas_corpus_id, &atom_id);
            // First-resolved wins (deterministic); duplicates are the same atom.
            if emb.contains_key(&key) {
                continue;
            }
            emb.insert(key.clone(), entry.embedding.clone());
            rows.push((key, entry.embedding.clone()));
        }
    }
    let resolved = rows.len();
    if resolved == 0 {
        return Err("ann_table: no atlas entries resolved to atom-ids".into());
    }
    let dir = tempfile::tempdir().map_err(|e| format!("ann_table tempdir: {e}"))?;
    let table = AnnSeedTable::build(dir.path(), &rows).await?;
    Ok(AnnTable {
        _dir: dir,
        table,
        emb,
        resolved,
        total,
    })
}

/// ANN-seeded port of `atlas_navigate`. Identical to v1 except steps 1a/resolve.
pub async fn atlas_navigate_ann(
    query_text: &str,
    query_embedding: &[f32],
    atlases: &[&AtlasContext],
    graphs: &[&AtlasGraph],
    ann: &AnnTable,
    max_seeds: usize,
    max_hops: usize,
) -> Vec<ChunkRequest> {
    if query_embedding.is_empty() || atlases.is_empty() {
        return Vec::new();
    }
    let graph_by_id: HashMap<&str, &AtlasGraph> =
        graphs.iter().map(|g| (g.atlas_corpus_id.as_str(), *g)).collect();

    // 1a (SUBSTITUTION). ANN over the co-located vector store -> atom-ids
    // directly. Re-score each hit with the canonical cosine() so the BFS sees
    // the same seed scores v1 would; the ANN supplies only the top-K ranking.
    let mut ann_seeds: Vec<(String, String, f32)> = Vec::new();
    match ann.table.nearest(query_embedding, max_seeds).await {
        Ok(keys) => {
            for key in keys {
                if let Some((cid, aid)) = split_key(&key) {
                    let score = ann
                        .emb
                        .get(&key)
                        .map(|e| cosine(query_embedding, e))
                        .unwrap_or(0.0);
                    ann_seeds.push((cid.to_string(), aid.to_string(), score));
                }
            }
        }
        Err(e) => eprintln!("atlas_navigate_ann: ANN nearest failed: {e}"),
    }

    // 1b. Name-match seeds — VERBATIM from atlas_navigate, then resolve to an
    // atom-id (name-match is text-based and survives v2 unchanged; only the
    // cosine-bag resolve is killed).
    let q_lower = query_text.to_lowercase();
    let mut name_seeds: Vec<(String, String, f32)> = Vec::new();
    for ctx in atlases {
        let Some(graph) = graph_by_id.get(ctx.atlas_corpus_id.as_str()) else {
            continue;
        };
        for entry in &ctx.entries {
            let name = entry.canonical_name.trim();
            if name.len() < 4 {
                continue;
            }
            let name_lower = name.to_lowercase();
            let mut hit = contains_whole_word(&q_lower, &name_lower);
            if !hit {
                if let Some(last) = name_lower.split_whitespace().last() {
                    if last.len() >= 4 && last != name_lower {
                        hit = contains_whole_word(&q_lower, last);
                    }
                }
            }
            if !hit {
                if let Some(rest) = entry.embed_text.strip_prefix("[Argument: ") {
                    if let Some(end) = rest.find(']') {
                        let arg_name = rest[..end].trim().to_lowercase();
                        if arg_name.len() >= 4 {
                            let toks: Vec<&str> = arg_name.split_whitespace().collect();
                            for w in toks.windows(2) {
                                let phrase = format!("{} {}", w[0], w[1]);
                                if phrase.len() >= 6 && q_lower.contains(&phrase) {
                                    hit = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            if !hit {
                continue;
            }
            let s = cosine(query_embedding, &entry.embedding).max(0.6);
            if let Some(atom_id) =
                resolve_atom_id_from_entry(graph, &entry.canonical_name, &entry.embed_text)
            {
                name_seeds.push((ctx.atlas_corpus_id.clone(), atom_id, s));
            }
        }
    }

    // Merge ANN + name seeds, dedup by (corpus_id, atom_id), keep the max score
    // (v1 merges by (atlas_id, embed_text) then resolves — bijective for
    // resolvable entries, so the groups + scores match).
    let mut merged: HashMap<(String, String), f32> = HashMap::new();
    for (cid, aid, s) in ann_seeds.into_iter().chain(name_seeds.into_iter()) {
        merged
            .entry((cid, aid))
            .and_modify(|e| {
                if s > *e {
                    *e = s;
                }
            })
            .or_insert(s);
    }
    let seeds: Vec<(String, String, f32, &AtlasGraph)> = merged
        .into_iter()
        .filter_map(|((cid, aid), s)| graph_by_id.get(cid.as_str()).map(|g| (cid, aid, s, *g)))
        .collect();

    // 2. BFS expand — VERBATIM from atlas_navigate.
    let mut neighborhood: HashMap<(String, String), f32> = HashMap::new();
    for (atlas_id, atom_id, seed_score, graph) in &seeds {
        let key = (atlas_id.clone(), atom_id.clone());
        let entry = neighborhood.entry(key).or_insert(0.0);
        *entry = entry.max(*seed_score);

        let mut frontier: Vec<(String, f32)> = vec![(atom_id.clone(), *seed_score)];
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(atom_id.clone());
        let decay = 0.6_f32;

        for hop in 1..=max_hops {
            let hop_decay = decay.powi(hop as i32);
            let mut next_frontier: Vec<(String, f32)> = Vec::new();
            for (current_id, current_score) in &frontier {
                let mut consider = |neighbor_id: &str, edge_type: EdgeType, conf: f32| {
                    if visited.contains(neighbor_id) {
                        return;
                    }
                    let w = edge_weight(edge_type);
                    if w <= 0.0 {
                        return;
                    }
                    let neighbor_score = current_score * w * conf * hop_decay;
                    if neighbor_score < 0.05 {
                        return;
                    }
                    let key = (atlas_id.clone(), neighbor_id.to_string());
                    let entry = neighborhood.entry(key).or_insert(0.0);
                    if neighbor_score > *entry {
                        *entry = neighbor_score;
                    }
                    visited.insert(neighbor_id.to_string());
                    next_frontier.push((neighbor_id.to_string(), neighbor_score));
                };
                for edge in graph.edges_from(current_id) {
                    consider(edge.target, edge.edge_type, edge.confidence);
                }
                for edge in graph.edges_to(current_id) {
                    consider(edge.source, edge.edge_type, edge.confidence);
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }
    }

    // 3. Emit ChunkRequests — VERBATIM from atlas_navigate.
    let mut chunk_scores: HashMap<(String, String), (f32, String, Vec<String>, Vec<String>, String)> =
        HashMap::new();
    for ((atlas_id, atom_id), atom_weight) in &neighborhood {
        let Some(graph) = graph_by_id.get(atlas_id.as_str()) else {
            continue;
        };
        let evidence = graph.atom_evidence(atom_id);
        let verbatim = atom_verbatim_excerpt(graph, atom_id);
        for ev in evidence {
            let chunk_id = ev.chunk_id().trim();
            if chunk_id.is_empty() {
                continue;
            }
            let preview = ev.passage_preview().trim();
            let key = (graph.article_slug.clone(), chunk_id.to_string());
            let entry = chunk_scores.entry(key).or_insert((
                0.0,
                preview.to_string(),
                Vec::new(),
                Vec::new(),
                graph.atlas_corpus_id.clone(),
            ));
            entry.0 += atom_weight;
            if preview.len() > entry.1.len() {
                entry.1 = preview.to_string();
            }
            entry.2.push(atom_id.clone());
            if let Some(line) = verbatim.as_ref() {
                if !entry.3.iter().any(|existing| existing == line) {
                    entry.3.push(line.clone());
                }
            }
        }
    }

    let mut requests: Vec<ChunkRequest> = chunk_scores
        .into_iter()
        .map(
            |((article_slug, chunk_id), (score, preview, motivating, verbatim, corpus_id))| {
                ChunkRequest {
                    corpus_id,
                    article_slug,
                    chunk_id,
                    passage_preview: preview,
                    score,
                    motivating_atoms: motivating,
                    verbatim_excerpts: verbatim,
                }
            },
        )
        .collect();
    requests.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    requests
}
