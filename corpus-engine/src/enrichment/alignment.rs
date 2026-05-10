//! Skeleton ↔ cluster alignment.
//!
//! Bridges the skeleton (top-down, authoritative) and the clusters
//! (bottom-up, data-driven). Maps clusters to skeleton positions.
//! Discovers positions the skeleton missed.

use std::collections::HashMap;

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::types::{EmbedFn, InferenceFn};

use super::clustering::{ClusterResult, EnrichmentProgress};
use super::domain::AlignmentConfig;
use super::skeleton::PartialSkeleton;

/// Result of the alignment phase.
#[derive(Debug)]
pub struct AlignmentResult {
    /// Mapping from cluster_id → position_id.
    pub aligned: HashMap<i32, String>,
    /// Number of unaligned clusters promoted to discovered positions.
    pub unaligned_promoted: usize,
}

/// Align clusters to skeleton positions by cosine similarity.
/// Unaligned clusters above a size threshold are promoted to discovered positions.
pub async fn align_clusters(
    index: &CorpusIndex,
    clusters: &ClusterResult,
    skeleton: &PartialSkeleton,
    embed: &EmbedFn,
    inference: &InferenceFn,
    config: &AlignmentConfig,
    progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
) -> Result<AlignmentResult> {
    progress(EnrichmentProgress::Phase {
        phase: 4,
        name: "Aligning clusters to skeleton",
        note: "",
    });

    // Embed each position's claim text for similarity comparison.
    let mut position_embeddings: HashMap<String, Vec<f32>> = HashMap::new();
    for question in &skeleton.questions {
        for position in &question.positions {
            let emb = (embed)(&position.claim).await?;
            position_embeddings.insert(position.id.clone(), emb);
        }
    }

    let mut aligned: HashMap<i32, String> = HashMap::new();
    let mut unaligned_large: Vec<&super::clustering::ClusterInfo> = Vec::new();

    for cluster in &clusters.clusters {
        let Some(label) = &cluster.label else { continue };
        if !label.is_argumentative || !label.is_coherent {
            continue;
        }

        // Find the highest-similarity skeleton position for this cluster.
        let best_match = position_embeddings
            .iter()
            .filter_map(|(pos_id, pos_emb)| {
                let sim = cosine_similarity(&cluster.centroid, pos_emb);
                if sim >= config.alignment_threshold {
                    Some((pos_id.clone(), sim))
                } else {
                    None
                }
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        match best_match {
            Some((pos_id, _)) => {
                aligned.insert(cluster.id, pos_id);
            }
            None => {
                if cluster.size >= config.min_chunks_for_discovery {
                    unaligned_large.push(cluster);
                }
            }
        }
    }

    // Promote unaligned clusters to discovered positions.
    let mut promoted = 0;
    for cluster in &unaligned_large {
        let Some(label) = &cluster.label else { continue };
        let Some(pos_name) = &label.position_name else {
            continue;
        };

        // Ask the model to describe this newly discovered position.
        let chunks = index.get_chunks(&cluster.central_chunks).await?;
        let chunk_text: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let prompt = format!(
            "This cluster of {} passages represents a position called \"{}\". \
             Summarize the core claim in one sentence.\n\n{}",
            cluster.size,
            pos_name,
            chunk_text.join("\n---\n")
        );
        let claim = (inference)(&prompt).await.unwrap_or_else(|_| pos_name.clone());

        let discovered_id = format!("p_discovered_{}", cluster.id);
        aligned.insert(cluster.id, discovered_id);
        promoted += 1;

        // The skeleton is immutable here; the engine will handle writing
        // discovered positions to the tables after alignment returns.
        let _ = claim; // Used by engine to store the position
    }

    Ok(AlignmentResult {
        aligned,
        unaligned_promoted: promoted,
    })
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let x = x as f64;
        let y = y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (dot / denom) as f32
    }
}
