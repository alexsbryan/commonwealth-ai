//! Fault line detection between opposing position clusters.
//!
//! Finds pairs of clusters assigned to different positions whose centroids
//! are semantically adjacent, then runs inference to identify the crux
//! of disagreement.

use std::collections::HashMap;

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::types::InferenceFn;

use super::alignment::AlignmentResult;
use super::clustering::{ClusterResult, EnrichmentProgress};
use super::domain::{Domain, FaultLineConfig};

/// A detected fault line between two positions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FaultLine {
    pub id: String,
    pub question_id: String,
    pub domain_id: String,
    pub position_a_id: String,
    pub position_b_id: String,
    pub crux: String,
    pub confidence: f32,
    pub key_chunk_ids: Vec<u64>,
    pub source: String, // "detected" | "skeleton"
    pub resolution_condition: Option<String>,
}

/// Detect fault lines between position clusters.
pub async fn detect_fault_lines(
    index: &CorpusIndex,
    clusters: &ClusterResult,
    alignment: &AlignmentResult,
    inference: &InferenceFn,
    config: &FaultLineConfig,
    domain: &dyn Domain,
    progress: &dyn Fn(EnrichmentProgress),
) -> Result<Vec<FaultLine>> {
    progress(EnrichmentProgress::Phase {
        phase: 5,
        name: "Detecting fault lines",
        note: "",
    });

    let mut fault_lines = Vec::new();

    // Find pairs of clusters assigned to different positions
    // whose centroids are semantically adjacent.
    let candidate_pairs =
        find_adjacent_cross_position_clusters(clusters, &alignment.aligned, config.proximity_threshold);

    for (cluster_a, cluster_b) in &candidate_pairs {
        let Some(pos_a_id) = alignment.aligned.get(&cluster_a.id) else {
            continue;
        };
        let Some(pos_b_id) = alignment.aligned.get(&cluster_b.id) else {
            continue;
        };

        let chunks_a = index.get_chunks(&cluster_a.central_chunks).await?;
        let chunks_b = index.get_chunks(&cluster_b.central_chunks).await?;
        let refs_a: Vec<&_> = chunks_a.iter().collect();
        let refs_b: Vec<&_> = chunks_b.iter().collect();

        let prompt = domain.fault_line_detection_prompt(&refs_a, &refs_b, pos_a_id, pos_b_id);
        let response = match (inference)(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Fault line detection failed for pair");
                continue;
            }
        };

        let parsed: serde_json::Value = match serde_json::from_str(&response) {
            Ok(v) => v,
            Err(_) => {
                // Try extracting JSON from markdown code fence
                if let Some(json_str) = extract_json_block(&response) {
                    match serde_json::from_str(json_str) {
                        Ok(v) => v,
                        Err(_) => continue,
                    }
                } else {
                    continue;
                }
            }
        };

        let confidence = parsed["confidence"].as_f64().unwrap_or(0.0) as f32;
        if confidence < config.min_confidence {
            continue;
        }

        fault_lines.push(FaultLine {
            id: format!("fl_{}", fault_lines.len()),
            question_id: String::new(), // filled by engine
            domain_id: domain.id().to_string(),
            position_a_id: pos_a_id.clone(),
            position_b_id: pos_b_id.clone(),
            crux: parsed["crux"].as_str().unwrap_or_default().to_string(),
            confidence,
            key_chunk_ids: cluster_a
                .central_chunks
                .iter()
                .chain(cluster_b.central_chunks.iter())
                .copied()
                .collect(),
            source: "detected".into(),
            resolution_condition: parsed["resolution_condition"]
                .as_str()
                .map(|s| s.to_string()),
        });
    }

    Ok(fault_lines)
}

/// Find pairs of clusters assigned to different positions whose centroids
/// have cosine similarity above the proximity threshold.
fn find_adjacent_cross_position_clusters<'a>(
    clusters: &'a ClusterResult,
    aligned: &HashMap<i32, String>,
    proximity_threshold: f32,
) -> Vec<(&'a super::clustering::ClusterInfo, &'a super::clustering::ClusterInfo)> {
    let mut pairs = Vec::new();
    let aligned_clusters: Vec<&super::clustering::ClusterInfo> = clusters
        .clusters
        .iter()
        .filter(|c| aligned.contains_key(&c.id))
        .collect();

    for i in 0..aligned_clusters.len() {
        for j in (i + 1)..aligned_clusters.len() {
            let a = aligned_clusters[i];
            let b = aligned_clusters[j];
            let pos_a = &aligned[&a.id];
            let pos_b = &aligned[&b.id];
            if pos_a == pos_b {
                continue;
            }
            let sim = cosine_similarity(&a.centroid, &b.centroid);
            if sim >= proximity_threshold {
                pairs.push((a, b));
            }
        }
    }
    pairs
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let (x, y) = (x as f64, y as f64);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-12 { 0.0 } else { (dot / denom) as f32 }
}

fn extract_json_block(text: &str) -> Option<&str> {
    if let Some(start) = text.find("```json") {
        let start = start + 7;
        if let Some(end) = text[start..].find("```") {
            return Some(text[start..start + end].trim());
        }
    }
    if let Some(start) = text.find("```") {
        let start = start + 3;
        if let Some(end) = text[start..].find("```") {
            let block = text[start..start + end].trim();
            if block.starts_with('{') || block.starts_with('[') {
                return Some(block);
            }
        }
    }
    None
}
