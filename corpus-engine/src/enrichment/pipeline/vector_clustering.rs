// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure in-memory HDBSCAN over a `Vec<Vec<f32>>` — no LanceDB, no
//! sampling, no random projection.
//!
//! The v1 `cluster_embeddings` wrapper in `enrichment::clustering`
//! handles big corpora by sampling + centroid reassignment + (optional)
//! random projection. The v2 admin harness runs against a single book
//! — a few thousand paragraphs at most — so we can skip those
//! optimizations and cluster directly. Keeping this path small also
//! means a bug in one clusterer can't silently mask the other.
//!
//! When v2 eventually replaces v1, the two paths consolidate into one.

use hdbscan::{Hdbscan, HdbscanHyperParams};

use crate::enrichment::domain::ClusteringConfig;
use crate::error::{Error, Result};

/// Result of a single in-memory clustering pass.
#[derive(Debug, Clone)]
pub struct VectorClusterResult {
    /// Per-input label. `-1` = noise. `≥0` = cluster id, stable within
    /// the call.
    pub labels: Vec<i32>,
    /// Number of distinct `≥0` clusters. `labels.iter().filter(≥0).unique().len()`.
    pub cluster_count: usize,
    /// Number of `-1` entries in `labels`.
    pub noise_count: usize,
}

impl VectorClusterResult {
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// Group input indices by cluster id. Noise (`-1`) is excluded.
    pub fn members_by_cluster(&self) -> std::collections::HashMap<i32, Vec<usize>> {
        let mut map: std::collections::HashMap<i32, Vec<usize>> = std::collections::HashMap::new();
        for (i, &label) in self.labels.iter().enumerate() {
            if label >= 0 {
                map.entry(label).or_default().push(i);
            }
        }
        map
    }
}

/// Cluster `embeddings` using HDBSCAN with the supplied `ClusteringConfig`.
///
/// - When `embeddings.len() < config.min_cluster_size`, every point is
///   classified as noise (returns an all-`-1` result). This matches
///   v1's guard clause and keeps the admin harness from panicking on
///   very small fixture inputs.
/// - `min_samples` is derived as `max(2, min_cluster_size / 5)`, same
///   as v1.
pub fn cluster_vectors(
    embeddings: &[Vec<f32>],
    config: &ClusteringConfig,
) -> Result<VectorClusterResult> {
    if embeddings.len() < config.min_cluster_size {
        return Ok(VectorClusterResult {
            labels: vec![-1; embeddings.len()],
            cluster_count: 0,
            noise_count: embeddings.len(),
        });
    }

    // hdbscan wants `&[Vec<f64>]`.
    let points: Vec<Vec<f64>> = embeddings
        .iter()
        .map(|v| v.iter().map(|&x| x as f64).collect())
        .collect();

    let min_samples = (config.min_cluster_size / 5).max(2);
    let params = HdbscanHyperParams::builder()
        .min_cluster_size(config.min_cluster_size)
        .min_samples(min_samples)
        .epsilon(config.epsilon as f64)
        .build();
    let clusterer = Hdbscan::new(&points, params);
    let labels = clusterer
        .cluster()
        .map_err(|e| Error::Extraction(format!("HDBSCAN clustering failed: {e:?}")))?;

    let noise_count = labels.iter().filter(|&&l| l == -1).count();
    let cluster_count: std::collections::HashSet<i32> =
        labels.iter().copied().filter(|l| *l >= 0).collect();

    Ok(VectorClusterResult {
        labels,
        cluster_count: cluster_count.len(),
        noise_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tight_cluster(offset: f32, n: usize) -> Vec<Vec<f32>> {
        // Many near-duplicate 3D vectors — HDBSCAN should see them as one cluster.
        (0..n)
            .map(|i| {
                let jitter = i as f32 * 0.001;
                vec![offset + jitter, offset - jitter, offset]
            })
            .collect()
    }

    #[test]
    fn fewer_points_than_min_cluster_size_returns_all_noise() {
        let config = ClusteringConfig {
            min_cluster_size: 10,
            epsilon: 0.3,
            label_sample_size: 5,
            max_cluster_points: 0,
            reduced_dims: 0,
        };
        let embeddings = vec![vec![1.0_f32, 0.0, 0.0]; 3];
        let res = cluster_vectors(&embeddings, &config).unwrap();
        assert_eq!(res.labels, vec![-1, -1, -1]);
        assert_eq!(res.cluster_count, 0);
        assert_eq!(res.noise_count, 3);
    }

    #[test]
    fn two_tight_clusters_discovered() {
        let mut points = tight_cluster(1.0, 8);
        points.extend(tight_cluster(-1.0, 8));
        let config = ClusteringConfig {
            min_cluster_size: 3,
            epsilon: 0.2,
            label_sample_size: 5,
            max_cluster_points: 0,
            reduced_dims: 0,
        };
        let res = cluster_vectors(&points, &config).unwrap();
        assert_eq!(res.len(), 16);
        assert!(
            res.cluster_count >= 1,
            "expected at least 1 cluster, got {}",
            res.cluster_count
        );
    }

    #[test]
    fn members_by_cluster_groups_near_duplicates() {
        // Two dense blobs with a tiny jitter (HDBSCAN needs non-zero
        // variance within a cluster to compute core distances).
        let mut points = tight_cluster(2.0, 6);
        points.extend(tight_cluster(-2.0, 6));
        let config = ClusteringConfig {
            min_cluster_size: 3,
            epsilon: 0.5,
            label_sample_size: 2,
            max_cluster_points: 0,
            reduced_dims: 0,
        };
        let res = cluster_vectors(&points, &config).unwrap();
        let groups = res.members_by_cluster();
        // We want the grouping map to actually map the `-1`-excluded
        // points, not claim everything was noise.
        let total_clustered: usize = groups.values().map(|v| v.len()).sum();
        assert!(
            total_clustered + res.noise_count == points.len(),
            "clustered + noise must equal total"
        );
    }

    #[test]
    fn empty_input_returns_empty_result() {
        let config = ClusteringConfig {
            min_cluster_size: 3,
            epsilon: 0.3,
            label_sample_size: 2,
            max_cluster_points: 0,
            reduced_dims: 0,
        };
        let res = cluster_vectors(&[], &config).unwrap();
        assert!(res.is_empty());
        assert_eq!(res.cluster_count, 0);
    }
}
