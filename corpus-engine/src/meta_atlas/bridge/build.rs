// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bridge build orchestrator — generic over any `(left, right)` corpus
//! pair. Nothing here names a corpus; SEP↔Wikipedia is one instantiation
//! the caller supplies.
//!
//! Per left (driver) topic: embed its concept text → ANN the right
//! (candidate) corpus index → dedupe candidates per article → score with
//! the graded signal stack → band → emit a `same` edge (AutoSame), hand
//! to the injected LLM adjudicator (Uncertain), or drop.
//!
//! **Resumable.** The build is a long offline job (per-topic cost is
//! dominated by the ANN over a large index + link-graph queries, not the
//! LLM). So it checkpoints after *every* left topic: the topic's edges are
//! appended to the reversible oplog, the (small) snapshot is rewritten,
//! and the completed topic's key is recorded in `bridge_progress.json`. A
//! re-run loads that done-set and skips finished topics — a kill or crash
//! loses at most one topic's work, never the whole run. `--fresh` ignores
//! the checkpoint and rebuilds from scratch.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atlas_canonical::lookup_key;
use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::types::EmbedFn;
use crate::wikipedia_graph::WikipediaGraph;

use super::adjudicate::{AdjudicateFn, AdjudicationRequest};
use super::edges::{
    default_bridge_edges_path, write_bridge_edges, BridgeAct, BridgeEdge, BridgeEdgesFile,
    BridgeRelation, EdgeSource, TopicRef,
};
use super::signals::{AlignmentBand, SignalContext, SignalStack};
use super::topic_node::{topic_from_atlas, topic_from_chunk, BridgeTopic};

/// One driver-side topic to align FROM (its per-article atlas on disk).
#[derive(Debug, Clone)]
pub struct DriverTopic {
    pub corpus_id: String,
    pub topic_id: String,
    pub atlas_dir: PathBuf,
}

/// Build inputs. The left side is enumerated (`left_topics`); the right
/// side is a single searchable corpus we generate candidates against.
pub struct BridgeBuildConfig {
    pub indexes_dir: PathBuf,
    pub left_topics: Vec<DriverTopic>,
    /// Candidate corpus id — its `<indexes_dir>/<id>` LanceDB index is
    /// ANN-searched. (e.g. `wikipedia`.)
    pub right_corpus_id: String,
    /// Whether the right corpus has a link graph
    /// (`<indexes_dir>/<id>/<id>_graph.db`) for the co-neighbour signal.
    pub right_has_link_graph: bool,
    /// ANN candidates fetched per left topic before per-article dedupe.
    pub k_candidates: usize,
    /// When true, compute edges but persist nothing (and never resume).
    pub dry_run: bool,
    /// Ignore any prior checkpoint and rebuild from scratch.
    pub fresh: bool,
    /// Override the edges snapshot path (default: `default_bridge_edges_path`).
    pub edges_out: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub struct BridgeBuildStats {
    pub left_topics: usize,
    pub candidates: usize,
    pub auto_same: usize,
    pub adjudicated: usize,
    pub dropped: usize,
    pub errors: usize,
    /// Topics skipped because a prior checkpoint already completed them.
    pub skipped_done: usize,
}

pub struct BridgeBuildReport {
    pub edges: Vec<BridgeEdge>,
    pub topics_seen: Vec<TopicRef>,
    pub stats: BridgeBuildStats,
}

/// Checkpoint sidecar — records which driver topics are done so a re-run
/// can skip them. Pinned to `(k, right_corpus)` so a resume can't mix
/// edges built under incompatible params.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BridgeProgress {
    schema_version: String,
    k_candidates: usize,
    right_corpus_id: String,
    done_topic_keys: Vec<String>,
}

impl BridgeProgress {
    const SCHEMA_VERSION: &'static str = "1.0";
}

fn read_progress(path: &Path) -> Option<BridgeProgress> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_progress(path: &Path, p: &BridgeProgress) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(p).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

fn topic_ref(t: &BridgeTopic) -> TopicRef {
    TopicRef::new(t.corpus_id.clone(), t.topic_id.clone(), t.title.clone())
}

/// Run the bridge build. `embed` and `adjudicate` are injected so this
/// crate stays inference-agnostic. Checkpoints after every topic.
pub async fn build_bridge(
    cfg: &BridgeBuildConfig,
    embed: EmbedFn,
    adjudicate: AdjudicateFn,
) -> Result<BridgeBuildReport> {
    // Resolve persistence paths up-front (needed for resume).
    let out_path = if cfg.dry_run {
        None
    } else {
        Some(
            cfg.edges_out
                .clone()
                .or_else(default_bridge_edges_path)
                .ok_or_else(|| {
                    Error::Extraction("no bridge edges output path (HOME unset)".into())
                })?,
        )
    };
    let dir = out_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf);
    let progress_path = dir.as_ref().map(|d| d.join("bridge_progress.json"));

    // ── Resume: load prior edges + done-set unless fresh/dry. ──
    let mut edges: Vec<BridgeEdge> = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    if !cfg.dry_run && !cfg.fresh {
        if let (Some(op), Some(pp)) = (out_path.as_ref(), progress_path.as_ref()) {
            if let (Ok(file), Some(prog)) = (super::edges::read_bridge_edges(op), read_progress(pp))
            {
                if prog.k_candidates == cfg.k_candidates
                    && prog.right_corpus_id == cfg.right_corpus_id
                {
                    edges = file.edges;
                    done = prog.done_topic_keys.into_iter().collect();
                    tracing::info!(
                        resumed_topics = done.len(),
                        resumed_edges = edges.len(),
                        "bridge: resuming prior build"
                    );
                } else {
                    tracing::warn!(
                        "bridge: checkpoint params differ (k / right_corpus) — starting fresh"
                    );
                }
            }
        }
    }

    let right_index = CorpusIndex::open(&cfg.indexes_dir.join(&cfg.right_corpus_id)).await?;
    let graph = if cfg.right_has_link_graph {
        let p = WikipediaGraph::default_db_path(&cfg.indexes_dir, &cfg.right_corpus_id);
        match WikipediaGraph::open(&p, &cfg.right_corpus_id) {
            Ok(g) => Some(g),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "bridge: link graph open failed; continuing without co-neighbour signal");
                None
            }
        }
    } else {
        None
    };

    let stack = SignalStack::default_stack();
    let oplog = dir.as_ref().map(crate::oplog::Oplog::<BridgeAct>::new);

    // Rebuild topics_seen / seen_keys from any resumed edges.
    let mut topics_seen: Vec<TopicRef> = Vec::new();
    let mut seen_keys: BTreeSet<String> = BTreeSet::new();
    for e in &edges {
        for t in [&e.left, &e.right] {
            if seen_keys.insert(t.key()) {
                topics_seen.push(t.clone());
            }
        }
    }

    let mut stats = BridgeBuildStats::default();

    for dt in &cfg.left_topics {
        let left_key = format!("{}::{}", dt.corpus_id, dt.topic_id);
        if done.contains(&left_key) {
            stats.skipped_done += 1;
            continue;
        }
        let left = match topic_from_atlas(&dt.corpus_id, &dt.topic_id, &dt.atlas_dir) {
            Ok(Some(t)) => t,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(corpus = %dt.corpus_id, error = %e, "bridge: read left atlas failed");
                stats.errors += 1;
                continue;
            }
        };
        stats.left_topics += 1;
        let lref = topic_ref(&left);
        if seen_keys.insert(lref.key()) {
            topics_seen.push(lref);
        }

        let qvec = match embed(&left.concept_text).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(topic = %left.topic_id, error = %e, "bridge: embed failed");
                stats.errors += 1;
                continue;
            }
        };
        let hits = match right_index
            .search(&qvec, &left.title, cfg.k_candidates)
            .await
        {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(topic = %left.topic_id, error = %e, "bridge: ANN search failed");
                stats.errors += 1;
                continue;
            }
        };

        // Dedupe candidates to one per right-article (nearest chunk wins).
        let mut best: HashMap<String, (f32, BridgeTopic)> = HashMap::new();
        for hit in &hits {
            let Some(wp) = topic_from_chunk(&cfg.right_corpus_id, hit) else {
                continue;
            };
            let dist = hit.vector_distance.unwrap_or(2.0);
            match best.get(&wp.topic_id) {
                Some((d, _)) if *d <= dist => {}
                _ => {
                    best.insert(wp.topic_id.clone(), (dist, wp));
                }
            }
        }

        // Score + band each candidate, collecting THIS topic's edges so we
        // can commit them durably before moving on.
        let lkeys: Vec<String> = left.entity_keys.iter().cloned().collect();
        let mut topic_edges: Vec<BridgeEdge> = Vec::new();
        for (_id, (dist, right)) in best {
            stats.candidates += 1;
            let rref = topic_ref(&right);
            if seen_keys.insert(rref.key()) {
                topics_seen.push(rref);
            }

            let mut ctx = SignalContext {
                embedding_similarity: Some((1.0 - dist).max(0.0)),
                ..Default::default()
            };
            if let Some(g) = &graph {
                ctx.co_neighbor_overlap =
                    co_neighbor_overlap(g, &right.title, &left.entity_keys).await;
            }

            let score = stack.evaluate(&left, &right, &ctx);
            match score.band {
                AlignmentBand::Drop => stats.dropped += 1,
                AlignmentBand::AutoSame => {
                    topic_edges.push(BridgeEdge {
                        left: topic_ref(&left),
                        right: topic_ref(&right),
                        relation: BridgeRelation::Same,
                        confidence: score.composite,
                        signals_fired: score.signals(),
                        source: EdgeSource::Deterministic,
                        rationale: None,
                        left_entity_keys: lkeys.clone(),
                    });
                    stats.auto_same += 1;
                }
                AlignmentBand::Uncertain => {
                    let req = AdjudicationRequest {
                        left_title: left.title.clone(),
                        left_gloss: left.concept_text.clone(),
                        left_arguments: left.argument_names.clone(),
                        right_title: right.title.clone(),
                        right_gloss: right.concept_text.clone(),
                    };
                    match adjudicate(req).await {
                        Ok(Some(v)) => {
                            topic_edges.push(BridgeEdge {
                                left: topic_ref(&left),
                                right: topic_ref(&right),
                                relation: v.relation,
                                confidence: v.confidence,
                                signals_fired: score.signals(),
                                source: EdgeSource::Adjudicated,
                                rationale: v.rationale,
                                left_entity_keys: lkeys.clone(),
                            });
                            stats.adjudicated += 1;
                        }
                        Ok(None) => stats.dropped += 1, // model said "different"
                        Err(e) => {
                            tracing::warn!(error = %e, "bridge: adjudication failed");
                            stats.errors += 1;
                        }
                    }
                }
            }
        }

        // ── Checkpoint: commit this topic durably before the next. ──
        edges.extend(topic_edges.iter().cloned());
        if let (Some(op), Some(log), Some(pp)) =
            (out_path.as_ref(), oplog.as_ref(), progress_path.as_ref())
        {
            for e in &topic_edges {
                if let Err(err) = log.append(&super::edges::BridgeOp::add(e)) {
                    tracing::warn!(error = %err, "bridge: oplog append failed");
                }
            }
            done.insert(left_key);
            let file = BridgeEdgesFile::new(edges.clone(), topics_seen.clone());
            if let Err(err) = write_bridge_edges(&file, op) {
                tracing::warn!(error = %err, "bridge: snapshot write failed");
            }
            if let Err(err) = write_progress(
                pp,
                &BridgeProgress {
                    schema_version: BridgeProgress::SCHEMA_VERSION.to_string(),
                    k_candidates: cfg.k_candidates,
                    right_corpus_id: cfg.right_corpus_id.clone(),
                    done_topic_keys: done.iter().cloned().collect(),
                },
            ) {
                tracing::warn!(error = %err, "bridge: progress write failed");
            }
        }
    }

    Ok(BridgeBuildReport {
        edges,
        topics_seen,
        stats,
    })
}

/// Fraction of `left_keys` that appear among the right candidate's
/// link-graph neighbours — the structural corroboration the
/// `LinkGraphCoNeighbor` signal reads. `0` when the candidate has no
/// neighbours or the left side names nothing.
async fn co_neighbor_overlap(
    g: &WikipediaGraph,
    right_title: &str,
    left_keys: &BTreeSet<String>,
) -> f32 {
    if left_keys.is_empty() {
        return 0.0;
    }
    let neighbors = g.neighbors(right_title, 60).await;
    if neighbors.is_empty() {
        return 0.0;
    }
    let nset: BTreeSet<String> = neighbors
        .iter()
        .map(|n| lookup_key(&n.title))
        .filter(|k| !k.is_empty())
        .collect();
    let hits = left_keys.iter().filter(|k| nset.contains(*k)).count();
    hits as f32 / left_keys.len() as f32
}
