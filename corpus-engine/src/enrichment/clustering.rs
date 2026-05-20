//! HDBSCAN clustering over existing chunk embeddings.
//!
//! **No inference calls in this phase.** Pure linear algebra over the
//! embedding vectors already stored in `chunks.lance/`.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::index::CorpusIndex;

use super::domain::ClusteringConfig;

/// Cluster all chunk embeddings from the index using HDBSCAN.
/// Writes `cluster_id` column back to `chunks.lance/`.
/// Returns cluster metadata for downstream phases.
pub async fn cluster_embeddings(
    index: &CorpusIndex,
    config: &ClusteringConfig,
    progress: &(dyn Fn(EnrichmentProgress) + Send + Sync),
) -> Result<ClusterResult> {
    progress(EnrichmentProgress::Phase {
        phase: 2,
        name: "Clustering embeddings",
        note: "No inference calls — pure vector math",
    });

    // ── Stream embeddings from LanceDB ────────────────────────────────
    let t0 = std::time::Instant::now();
    tracing::info!("Loading embeddings from index...");
    let (chunk_ids, embeddings) = index.stream_embedding_column().await?;

    let total = chunk_ids.len();
    let dims = embeddings.first().map(|e| e.len()).unwrap_or(0);
    let load_secs = t0.elapsed().as_secs();
    tracing::info!(
        chunks = total,
        dims = dims,
        elapsed_secs = load_secs,
        "Embeddings loaded"
    );
    progress(EnrichmentProgress::ClusteringStarted {
        total_chunks: total,
    });
    progress(EnrichmentProgress::ClusteringStep {
        step: "Loaded embeddings",
        detail: format!("{total} chunks x {dims} dims in {load_secs}s"),
    });

    // ── Guard: need enough points for clustering ────────────────────
    if total < config.min_cluster_size {
        tracing::warn!(
            "Only {total} chunks — fewer than min_cluster_size ({}). Skipping clustering.",
            config.min_cluster_size
        );
        progress(EnrichmentProgress::ClusteringComplete {
            cluster_count: 0,
            noise_chunks: total,
        });
        let assignments: HashMap<u64, i32> = chunk_ids.iter().map(|&id| (id, -1i32)).collect();
        return Ok(ClusterResult {
            assignments,
            clusters: Vec::new(),
            noise_count: total,
        });
    }

    // ── Optional: random projection for dimensionality reduction ────
    let work_embeddings = if config.reduced_dims > 0 && dims > config.reduced_dims {
        let target = config.reduced_dims;
        tracing::info!(from = dims, to = target, "Applying random projection");
        progress(EnrichmentProgress::ClusteringStep {
            step: "Random projection",
            detail: format!("{dims}d → {target}d"),
        });
        let t_rp = std::time::Instant::now();
        let projected = random_projection(&embeddings, target);
        tracing::info!(elapsed_secs = t_rp.elapsed().as_secs(), "Projection done");
        projected
    } else {
        embeddings.clone()
    };

    let work_dims = work_embeddings.first().map(|e| e.len()).unwrap_or(0);

    // ── Optional: sample-then-assign for large corpora ────────────────
    let use_sampling = config.max_cluster_points > 0 && total > config.max_cluster_points;
    let (sample_indices, sample_embeddings_f64) = if use_sampling {
        let sample_size = config.max_cluster_points;
        tracing::info!(
            sample = sample_size,
            total = total,
            "Sampling {sample_size} of {total} points for HDBSCAN"
        );
        progress(EnrichmentProgress::ClusteringStep {
            step: "Sampling",
            detail: format!("{sample_size} of {total} points"),
        });
        let indices = deterministic_sample(total, sample_size);
        let sampled: Vec<Vec<f64>> = indices
            .iter()
            .map(|&i| work_embeddings[i].iter().map(|&x| x as f64).collect())
            .collect();
        (Some(indices), sampled)
    } else {
        let all: Vec<Vec<f64>> = work_embeddings
            .iter()
            .map(|v| v.iter().map(|&x| x as f64).collect())
            .collect();
        (None, all)
    };

    // ── Run HDBSCAN ───────────────────────────────────────────────────
    let min_samples = (config.min_cluster_size / 5).max(2);
    let cluster_point_count = sample_embeddings_f64.len();
    tracing::info!(
        points = cluster_point_count,
        dims = work_dims,
        min_cluster_size = config.min_cluster_size,
        min_samples = min_samples,
        epsilon = config.epsilon,
        "Starting HDBSCAN"
    );
    progress(EnrichmentProgress::ClusteringStep {
        step: "Running HDBSCAN",
        detail: format!(
            "{cluster_point_count} points x {work_dims}d, min_cluster={}, eps={}",
            config.min_cluster_size, config.epsilon
        ),
    });
    let t2 = std::time::Instant::now();

    let hyper_params = hdbscan::HdbscanHyperParams::builder()
        .min_cluster_size(config.min_cluster_size)
        .min_samples(min_samples)
        .epsilon(config.epsilon as f64)
        .build();

    let clusterer = hdbscan::Hdbscan::new(&sample_embeddings_f64, hyper_params);
    let sample_labels: Vec<i32> = clusterer
        .cluster()
        .map_err(|e| Error::Extraction(format!("HDBSCAN clustering failed: {e:?}")))?;

    let hdbscan_secs = t2.elapsed().as_secs();
    tracing::info!(elapsed_secs = hdbscan_secs, "HDBSCAN complete");
    progress(EnrichmentProgress::ClusteringStep {
        step: "HDBSCAN complete",
        detail: format!("Finished in {hdbscan_secs}s"),
    });

    // ── Build cluster centroids from HDBSCAN results ──────────────────
    let mut cluster_map: HashMap<i32, Vec<usize>> = HashMap::new();
    for (i, &label) in sample_labels.iter().enumerate() {
        if label >= 0 {
            // Map back to original indices if sampling was used.
            let original_idx = sample_indices.as_ref().map_or(i, |si| si[i]);
            cluster_map.entry(label).or_default().push(original_idx);
        }
    }

    // Compute centroids using original (full-dim) embeddings for quality.
    let centroids: HashMap<i32, Vec<f32>> = cluster_map
        .iter()
        .map(|(&id, indices)| (id, mean_embedding(&embeddings, indices)))
        .collect();

    // ── Assign non-sampled points to nearest centroid ─────────────────
    let labels: Vec<i32> = if use_sampling {
        let sampled_set: std::collections::HashSet<usize> = sample_indices
            .as_ref()
            .unwrap()
            .iter()
            .copied()
            .collect();
        let mut full_labels = vec![-1i32; total];

        // Copy sample labels.
        for (i, &label) in sample_labels.iter().enumerate() {
            let original_idx = sample_indices.as_ref().unwrap()[i];
            full_labels[original_idx] = label;
        }

        // Assign remaining points by nearest centroid.
        let mut assigned = 0_usize;
        for i in 0..total {
            if sampled_set.contains(&i) {
                continue;
            }
            let (best_id, _) = nearest_centroid(&embeddings[i], &centroids);
            full_labels[i] = best_id;
            assigned += 1;
        }

        tracing::info!(assigned = assigned, "Assigned non-sampled points to nearest centroid");
        progress(EnrichmentProgress::ClusteringStep {
            step: "Assignment complete",
            detail: format!("Assigned {assigned} remaining points to nearest cluster"),
        });

        // Rebuild cluster_map with all points.
        cluster_map.clear();
        for (i, &label) in full_labels.iter().enumerate() {
            if label >= 0 {
                cluster_map.entry(label).or_default().push(i);
            }
        }

        full_labels
    } else {
        sample_labels
    };

    // ── Compute final cluster statistics ──────────────────────────────
    let clusters: Vec<ClusterInfo> = cluster_map
        .iter()
        .map(|(&id, indices)| {
            let centroid = centroids.get(&id).cloned().unwrap_or_default();
            let central_chunks =
                indices_nearest_to_centroid(&embeddings, indices, &chunk_ids, config.label_sample_size);
            ClusterInfo {
                id,
                size: indices.len(),
                centroid,
                central_chunks,
                label: None, // filled in Phase 2b
            }
        })
        .collect();

    let noise_count = labels.iter().filter(|&&l| l == -1).count();
    let total_secs = t0.elapsed().as_secs();
    tracing::info!(
        clusters = clusters.len(),
        noise = noise_count,
        total_elapsed_secs = total_secs,
        "Clustering statistics computed"
    );

    progress(EnrichmentProgress::ClusteringComplete {
        cluster_count: clusters.len(),
        noise_chunks: noise_count,
    });

    // ── Write cluster_id column to chunks.lance ────────────────────────
    let assignments: HashMap<u64, i32> = chunk_ids
        .iter()
        .zip(labels.iter())
        .map(|(&id, &label)| (id, label))
        .collect();

    index.bulk_update_i32_column("cluster_id", &assignments).await?;

    Ok(ClusterResult {
        assignments,
        clusters,
        noise_count,
    })
}

/// Compute the mean embedding for a set of indices.
fn mean_embedding(embeddings: &[Vec<f32>], indices: &[usize]) -> Vec<f32> {
    if indices.is_empty() {
        return Vec::new();
    }
    let dims = embeddings[indices[0]].len();
    let mut sum = vec![0.0f64; dims];
    for &idx in indices {
        for (j, &val) in embeddings[idx].iter().enumerate() {
            sum[j] += val as f64;
        }
    }
    let n = indices.len() as f64;
    sum.iter().map(|&s| (s / n) as f32).collect()
}

/// Find the N chunk IDs closest to the centroid within a cluster.
fn indices_nearest_to_centroid(
    embeddings: &[Vec<f32>],
    indices: &[usize],
    chunk_ids: &[u64],
    n: usize,
) -> Vec<u64> {
    let centroid = mean_embedding(embeddings, indices);
    let mut scored: Vec<(usize, f32)> = indices
        .iter()
        .map(|&idx| {
            let dist = cosine_distance(&embeddings[idx], &centroid);
            (idx, dist)
        })
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .iter()
        .take(n)
        .map(|(idx, _)| chunk_ids[*idx])
        .collect()
}

/// Random projection from `d` dimensions to `target_dims` using a sparse
/// random matrix (Achlioptas, 2003). Preserves pairwise distances by the
/// Johnson-Lindenstrauss lemma with high probability.
///
/// Uses a deterministic seed so results are reproducible.
fn random_projection(embeddings: &[Vec<f32>], target_dims: usize) -> Vec<Vec<f32>> {
    let d = embeddings[0].len();
    // Generate a d × target_dims projection matrix using a simple LCG PRNG.
    // Each entry is ±1/sqrt(target_dims) with equal probability.
    let scale = 1.0 / (target_dims as f32).sqrt();
    let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_1234; // fixed seed
    let mut projection = vec![vec![0.0f32; target_dims]; d];
    for row in projection.iter_mut() {
        for val in row.iter_mut() {
            // LCG: x_{n+1} = (a*x_n + c) mod m
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *val = if (rng_state >> 33) & 1 == 0 { scale } else { -scale };
        }
    }

    // Matrix multiply: result[i][j] = sum_k(embedding[i][k] * projection[k][j])
    embeddings
        .iter()
        .map(|emb| {
            let mut projected = vec![0.0f32; target_dims];
            for (k, &val) in emb.iter().enumerate() {
                if val.abs() < 1e-10 {
                    continue; // skip near-zero for speed
                }
                for (j, p) in projection[k].iter().enumerate() {
                    projected[j] += val * p;
                }
            }
            projected
        })
        .collect()
}

/// Deterministic sampling: select `n` indices from `total` using stride.
/// Produces a reproducible, evenly-spaced sample.
fn deterministic_sample(total: usize, n: usize) -> Vec<usize> {
    if n >= total {
        return (0..total).collect();
    }
    let step = total as f64 / n as f64;
    (0..n).map(|i| (i as f64 * step) as usize).collect()
}

/// Find the cluster ID of the nearest centroid to a point.
fn nearest_centroid(point: &[f32], centroids: &HashMap<i32, Vec<f32>>) -> (i32, f32) {
    centroids
        .iter()
        .map(|(&id, centroid)| (id, cosine_distance(point, centroid)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or((-1, f32::MAX))
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
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
        1.0
    } else {
        1.0 - (dot / denom) as f32
    }
}

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ClusterResult {
    pub assignments: HashMap<u64, i32>,
    pub clusters: Vec<ClusterInfo>,
    pub noise_count: usize,
}

#[derive(Debug, Clone)]
pub struct ClusterInfo {
    pub id: i32,
    pub size: usize,
    pub centroid: Vec<f32>,
    pub central_chunks: Vec<u64>,
    pub label: Option<super::domain::ClusterLabel>,
}

/// Progress events emitted during enrichment.
#[derive(Debug, Clone)]
pub enum EnrichmentProgress {
    Phase {
        phase: u32,
        name: &'static str,
        note: &'static str,
    },
    PhaseSkipped {
        phase: u32,
        name: &'static str,
    },
    Resuming {
        from_phase: String,
    },
    ClusteringStarted {
        total_chunks: usize,
    },
    /// Sub-phase within clustering (loading embeddings, running HDBSCAN, etc.)
    ClusteringStep {
        step: &'static str,
        detail: String,
    },
    ClusteringComplete {
        cluster_count: usize,
        noise_chunks: usize,
    },
    Phase1Progress {
        batches_done: usize,
        batches_total: usize,
    },
    /// Per-cluster progress during Phase 2b labeling. Emitted after
    /// each cluster's inference call (success or failure) so the
    /// daemon can surface "X of Y labelled, Z stuck" to the operator
    /// rather than waiting for `Phase2bComplete` at the end of the
    /// whole phase. `last_error` carries the most-recent inference
    /// error message when non-empty — UI surfaces the cluster that
    /// failed, the count of consecutive failures, and the underlying
    /// error so the operator can decide whether to keep waiting,
    /// restart the daemon (clears MTP quarantine), or abandon the
    /// run.
    Phase2bProgress {
        clusters_done: usize,
        clusters_total: usize,
        clusters_failed: usize,
        consecutive_failures: usize,
        last_error: Option<String>,
    },
    Phase2bComplete {
        labeled_count: usize,
    },
}

/// Summary statistics for a completed enrichment run.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FieldModelStats {
    pub total_chunks: u64,
    pub classified_chunks: u64,
    pub unclassified_chunks: u64,
    pub cluster_count: usize,
    pub questions_count: usize,
    pub positions_count: usize,
    pub fault_lines_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_embedding_basic() {
        let embeddings = vec![
            vec![1.0, 2.0, 3.0],
            vec![3.0, 4.0, 5.0],
            vec![5.0, 6.0, 7.0],
        ];
        let mean = mean_embedding(&embeddings, &[0, 1, 2]);
        assert!((mean[0] - 3.0).abs() < 1e-5);
        assert!((mean[1] - 4.0).abs() < 1e-5);
        assert!((mean[2] - 5.0).abs() < 1e-5);
    }

    #[test]
    fn mean_embedding_single() {
        let embeddings = vec![vec![1.0, 2.0]];
        let mean = mean_embedding(&embeddings, &[0]);
        assert!((mean[0] - 1.0).abs() < 1e-5);
        assert!((mean[1] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn mean_embedding_empty_indices() {
        let embeddings = vec![vec![1.0, 2.0]];
        let mean = mean_embedding(&embeddings, &[]);
        assert!(mean.is_empty());
    }

    #[test]
    fn cosine_distance_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let dist = cosine_distance(&a, &a);
        assert!(dist.abs() < 1e-5, "identical vectors should have distance ~0");
    }

    #[test]
    fn cosine_distance_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let dist = cosine_distance(&a, &b);
        assert!(
            (dist - 1.0).abs() < 1e-5,
            "orthogonal vectors should have distance ~1"
        );
    }

    #[test]
    fn cosine_distance_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let dist = cosine_distance(&a, &b);
        assert!(
            (dist - 2.0).abs() < 1e-5,
            "opposite vectors should have distance ~2"
        );
    }

    #[test]
    fn indices_nearest_to_centroid_returns_n_results() {
        let embeddings = vec![
            vec![1.0, 0.0],
            vec![0.9, 0.1],
            vec![0.8, 0.2],
            vec![0.0, 1.0],
        ];
        let chunk_ids: Vec<u64> = vec![100, 200, 300, 400];
        let nearest =
            indices_nearest_to_centroid(&embeddings, &[0, 1, 2, 3], &chunk_ids, 2);
        assert_eq!(nearest.len(), 2, "should return exactly n results");
    }

    #[test]
    fn indices_nearest_to_centroid_single_element() {
        let embeddings = vec![vec![1.0, 0.0]];
        let chunk_ids: Vec<u64> = vec![42];
        let nearest =
            indices_nearest_to_centroid(&embeddings, &[0], &chunk_ids, 5);
        assert_eq!(nearest.len(), 1, "can't return more than available");
        assert_eq!(nearest[0], 42);
    }

    #[test]
    fn field_model_stats_default() {
        let stats = FieldModelStats::default();
        assert_eq!(stats.total_chunks, 0);
        assert_eq!(stats.cluster_count, 0);
        assert_eq!(stats.questions_count, 0);
    }

    #[test]
    fn field_model_stats_json_round_trip() {
        let stats = FieldModelStats {
            total_chunks: 187967,
            classified_chunks: 141823,
            unclassified_chunks: 46144,
            cluster_count: 347,
            questions_count: 18,
            positions_count: 67,
            fault_lines_count: 43,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let parsed: FieldModelStats = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.total_chunks, 187967);
        assert_eq!(parsed.cluster_count, 347);
    }
}
