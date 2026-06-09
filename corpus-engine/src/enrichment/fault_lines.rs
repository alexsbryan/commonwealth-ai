// SPDX-License-Identifier: AGPL-3.0-or-later
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
    progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
) -> Result<Vec<FaultLine>> {
    progress(EnrichmentProgress::Phase {
        phase: 5,
        name: "Detecting fault lines",
        note: "",
    });

    let mut fault_lines = Vec::new();

    // Find pairs of clusters assigned to different positions
    // whose centroids are semantically adjacent.
    let candidate_pairs = find_adjacent_cross_position_clusters(
        clusters,
        &alignment.aligned,
        config.proximity_threshold,
    );

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
        let response = match (inference)(&prompt, None).await {
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
) -> Vec<(
    &'a super::clustering::ClusterInfo,
    &'a super::clustering::ClusterInfo,
)> {
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
    if denom < 1e-12 {
        0.0
    } else {
        (dot / denom) as f32
    }
}

/// Extract a JSON block from model output. Handles three shapes the
/// pipeline regularly sees in the wild:
///
///   1. ` ```json … ``` ` — explicit JSON fence. What thinking models
///      (Qwen3, Qwopus) emit by default even when `response_format` is
///      set, because the chat template steers them toward markdown.
///   2. ` ``` … ``` ` — bare triple-backtick fence. Less common but
///      observed when models drop the language hint.
///   3. **Bare JSON** — `{ … }` or `[ … ]` with no fence. What
///      well-behaved models (Darwin-9B, schema-faithful runs) emit
///      when `response_format: json_schema, strict: true` actually
///      sticks. Previously dropped on the floor with "no recognisable
///      JSON object" — silently rejecting every model that obeyed
///      the schema cleanly. The bench surfaced this when Darwin's
///      response started with a perfectly-formed `{"section_id": …}`
///      and we still errored out.
///
/// Returns the JSON substring trimmed of surrounding whitespace.
pub(crate) fn extract_json_block(text: &str) -> Option<&str> {
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
    // No fence — fall back to bare JSON. Trim leading/trailing
    // whitespace and accept anything that opens with `{` or `[`.
    // We don't try to slice out a sub-region; if the model wrapped
    // its JSON in prose, that's a parse failure further down the
    // pipeline (where serde_json gives a precise byte offset),
    // not a "no JSON" failure here. The full body is preserved
    // for the downstream parser to chew on.
    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some(trimmed);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::clustering::ClusterInfo;

    #[test]
    fn extract_json_block_from_json_fence() {
        let text = "Here:\n```json\n{\"crux\": \"test\"}\n```\nDone.";
        assert_eq!(extract_json_block(text), Some("{\"crux\": \"test\"}"));
    }

    #[test]
    fn extract_json_block_from_plain_fence() {
        let text = "```\n{\"a\": 1}\n```";
        assert_eq!(extract_json_block(text), Some("{\"a\": 1}"));
    }

    #[test]
    fn extract_json_block_non_json_content() {
        let text = "```\nsome text that is not json\n```";
        assert!(extract_json_block(text).is_none());
    }

    #[test]
    fn extract_json_block_no_fences() {
        let text = "just plain text";
        assert!(extract_json_block(text).is_none());
    }

    #[test]
    fn extract_json_block_bare_object() {
        // Regression: schema-faithful models (response_format honoured)
        // emit bare JSON with no fence. Used to fail with "no
        // recognisable JSON object" until bare-JSON fallback landed.
        let text = r#"{"section_id": "sec_0001", "claims": []}"#;
        assert_eq!(
            extract_json_block(text),
            Some(r#"{"section_id": "sec_0001", "claims": []}"#)
        );
    }

    #[test]
    fn extract_json_block_bare_object_with_whitespace() {
        let text = "\n\n  {\"x\": 1}\n";
        assert_eq!(extract_json_block(text), Some(r#"{"x": 1}"#));
    }

    #[test]
    fn extract_json_block_bare_array() {
        let text = r#"[{"x": 1}]"#;
        assert_eq!(extract_json_block(text), Some(r#"[{"x": 1}]"#));
    }

    #[test]
    fn extract_json_block_fence_still_wins_over_bare() {
        // When BOTH a fence and a leading `{` appear (rare — a model
        // wrote both prose and fenced JSON), the fence wins because
        // fence content is the explicitly-marked answer.
        let text = "{\n  \"draft\": true\n}\n\n```json\n{\"final\": true}\n```";
        assert_eq!(extract_json_block(text), Some(r#"{"final": true}"#));
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-5);
    }

    #[test]
    fn find_adjacent_cross_position_clusters_basic() {
        // Two clusters assigned to different positions with similar centroids.
        let clusters = ClusterResult {
            assignments: [(1u64, 0i32), (2, 1)].into_iter().collect(),
            clusters: vec![
                ClusterInfo {
                    id: 0,
                    size: 10,
                    centroid: vec![1.0, 0.0, 0.0],
                    central_chunks: vec![1],
                    label: None,
                },
                ClusterInfo {
                    id: 1,
                    size: 10,
                    centroid: vec![0.9, 0.1, 0.0], // very similar to cluster 0
                    central_chunks: vec![2],
                    label: None,
                },
            ],
            noise_count: 0,
        };
        let aligned: HashMap<i32, String> = [
            (0, "p_compat".to_string()),
            (1, "p_hard_incompat".to_string()),
        ]
        .into_iter()
        .collect();

        let pairs = find_adjacent_cross_position_clusters(&clusters, &aligned, 0.5);
        assert_eq!(pairs.len(), 1, "should find one cross-position pair");
    }

    #[test]
    fn find_adjacent_skips_same_position() {
        let clusters = ClusterResult {
            assignments: [(1u64, 0i32), (2, 1)].into_iter().collect(),
            clusters: vec![
                ClusterInfo {
                    id: 0,
                    size: 10,
                    centroid: vec![1.0, 0.0],
                    central_chunks: vec![1],
                    label: None,
                },
                ClusterInfo {
                    id: 1,
                    size: 10,
                    centroid: vec![0.99, 0.01],
                    central_chunks: vec![2],
                    label: None,
                },
            ],
            noise_count: 0,
        };
        // Both clusters assigned to the SAME position.
        let aligned: HashMap<i32, String> =
            [(0, "p_compat".to_string()), (1, "p_compat".to_string())]
                .into_iter()
                .collect();

        let pairs = find_adjacent_cross_position_clusters(&clusters, &aligned, 0.5);
        assert!(pairs.is_empty(), "same-position pairs should be excluded");
    }

    #[test]
    fn find_adjacent_respects_threshold() {
        let clusters = ClusterResult {
            assignments: [(1u64, 0i32), (2, 1)].into_iter().collect(),
            clusters: vec![
                ClusterInfo {
                    id: 0,
                    size: 10,
                    centroid: vec![1.0, 0.0, 0.0],
                    central_chunks: vec![1],
                    label: None,
                },
                ClusterInfo {
                    id: 1,
                    size: 10,
                    centroid: vec![0.0, 1.0, 0.0], // orthogonal — similarity ~0
                    central_chunks: vec![2],
                    label: None,
                },
            ],
            noise_count: 0,
        };
        let aligned: HashMap<i32, String> = [(0, "p_a".to_string()), (1, "p_b".to_string())]
            .into_iter()
            .collect();

        let pairs = find_adjacent_cross_position_clusters(&clusters, &aligned, 0.5);
        assert!(
            pairs.is_empty(),
            "orthogonal clusters should not meet the 0.5 threshold"
        );
    }

    #[test]
    fn fault_line_serde_round_trip() {
        let fl = FaultLine {
            id: "fl_0".into(),
            question_id: "q_1".into(),
            domain_id: "philosophy".into(),
            position_a_id: "p_compat".into(),
            position_b_id: "p_hard".into(),
            crux: "Whether alternative possibilities are required".into(),
            confidence: 0.91,
            key_chunk_ids: vec![100, 200],
            source: "detected".into(),
            resolution_condition: Some("Frankfurt case resolution".into()),
        };
        let json = serde_json::to_string(&fl).unwrap();
        let parsed: FaultLine = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.crux, fl.crux);
        assert_eq!(parsed.confidence, fl.confidence);
        assert_eq!(parsed.resolution_condition, fl.resolution_condition);
    }
}
