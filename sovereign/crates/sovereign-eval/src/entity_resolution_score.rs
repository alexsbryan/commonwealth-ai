// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entity-resolution scoring primitives (Phase 3 of the
//! architecture-over-Enron push).
//!
//! Two complementary metrics, both *generic over any clustering of
//! mention-ids* so every future vertical (Firm Inbox, sales-intel,
//! project-memory) can reuse them unchanged.
//!
//! **B³ (Bagga & Baldwin, 1998)** — per-mention precision / recall /
//! F1 averaged over the corpus. The canonical metric for
//! coreference / entity resolution. Definitions:
//!
//! - `B³_precision(m) = |predicted_cluster(m) ∩ gold_cluster(m)| / |predicted_cluster(m)|`
//! - `B³_recall(m)    = |predicted_cluster(m) ∩ gold_cluster(m)| / |gold_cluster(m)|`
//! - System-level: arithmetic mean across mentions.
//! - `F1 = 2PR / (P + R)`; defined as 0 when both P and R are 0.
//!
//! **Pairwise-F1** — every pair of mentions either same-cluster (1)
//! or different (0); the diagonal `(m, m)` is excluded. Precision /
//! recall computed on the agreement matrix. Sanity check that the
//! B³ number isn't being inflated by singleton-clusters dominating
//! the per-mention mean.
//!
//! Both metrics take **partition vectors keyed by mention id**:
//! `BTreeMap<MentionId, ClusterId>`. The id type is `String` so the
//! caller can use whatever surface-form key it has (canonical name,
//! atom id, raw email-address). The cluster id is also `String` —
//! arbitrary, opaque, used only for equality.
//!
//! Inputs are *aligned* — only mentions that appear in BOTH
//! `predicted` and `gold` are scored. Mentions in one but not the
//! other are flagged in [`B3Outcome::unmatched_predicted`] /
//! [`B3Outcome::unmatched_gold`] so the operator can see the
//! coverage gap without it silently zeroing the recall.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Clustering as a flat partition keyed by mention id.
pub type Clustering = BTreeMap<String, String>;

/// B³ outcome — per-cluster + system totals + alignment diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct B3Outcome {
    /// Arithmetic mean of per-mention precision across the aligned
    /// mention set. `0.0` when no mentions align.
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    /// Number of mentions that contributed to the means.
    pub n_aligned: usize,
    /// Mentions present in `predicted` but not in `gold`. Surfaces
    /// the "we hallucinated an entity" failure mode.
    pub unmatched_predicted: Vec<String>,
    /// Mentions present in `gold` but not in `predicted`. Surfaces
    /// the "we missed an entity" failure mode.
    pub unmatched_gold: Vec<String>,
}

impl B3Outcome {
    pub fn empty() -> Self {
        Self {
            precision: 0.0,
            recall: 0.0,
            f1: 0.0,
            n_aligned: 0,
            unmatched_predicted: Vec::new(),
            unmatched_gold: Vec::new(),
        }
    }
}

/// Compute B³ precision / recall / F1 for `predicted` against `gold`.
pub fn b_cubed(predicted: &Clustering, gold: &Clustering) -> B3Outcome {
    // Alignment + diagnostic sets.
    let predicted_keys: BTreeSet<&String> = predicted.keys().collect();
    let gold_keys: BTreeSet<&String> = gold.keys().collect();
    let aligned: Vec<&String> = predicted_keys.intersection(&gold_keys).copied().collect();
    let unmatched_predicted: Vec<String> = predicted_keys
        .difference(&gold_keys)
        .map(|s| (*s).clone())
        .collect();
    let unmatched_gold: Vec<String> = gold_keys
        .difference(&predicted_keys)
        .map(|s| (*s).clone())
        .collect();
    if aligned.is_empty() {
        return B3Outcome {
            precision: 0.0,
            recall: 0.0,
            f1: 0.0,
            n_aligned: 0,
            unmatched_predicted,
            unmatched_gold,
        };
    }

    // Group aligned mentions by their cluster on each side.
    let mut predicted_cluster_members: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
    let mut gold_cluster_members: BTreeMap<&String, BTreeSet<&String>> = BTreeMap::new();
    for &m in &aligned {
        let pc = predicted.get(m).expect("aligned key in predicted");
        let gc = gold.get(m).expect("aligned key in gold");
        predicted_cluster_members.entry(pc).or_default().insert(m);
        gold_cluster_members.entry(gc).or_default().insert(m);
    }

    let mut p_sum = 0.0;
    let mut r_sum = 0.0;
    for &m in &aligned {
        let pc = predicted.get(m).expect("aligned key");
        let gc = gold.get(m).expect("aligned key");
        let p_members = predicted_cluster_members.get(pc).expect("cluster present");
        let g_members = gold_cluster_members.get(gc).expect("cluster present");
        let intersect = p_members.intersection(g_members).count();
        p_sum += intersect as f64 / p_members.len() as f64;
        r_sum += intersect as f64 / g_members.len() as f64;
    }
    let n = aligned.len() as f64;
    let precision = p_sum / n;
    let recall = r_sum / n;
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };

    B3Outcome {
        precision,
        recall,
        f1,
        n_aligned: aligned.len(),
        unmatched_predicted,
        unmatched_gold,
    }
}

/// Pairwise outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairwiseOutcome {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub n_aligned_pairs: usize,
}

/// Compute pairwise precision / recall / F1 over the aligned
/// mention pairs. Excludes the diagonal `(m, m)`. Quadratic in the
/// number of mentions — fine for benches under a few thousand
/// mentions; the operator should bucket by chunk for larger sets.
pub fn pairwise(predicted: &Clustering, gold: &Clustering) -> PairwiseOutcome {
    let aligned: Vec<&String> = predicted.keys().filter(|k| gold.contains_key(*k)).collect();
    let n = aligned.len();
    if n < 2 {
        return PairwiseOutcome {
            precision: 0.0,
            recall: 0.0,
            f1: 0.0,
            n_aligned_pairs: 0,
        };
    }
    let mut tp = 0usize;
    let mut fp = 0usize;
    let mut fn_ = 0usize;
    let mut total_pairs = 0usize;
    for i in 0..n {
        for j in (i + 1)..n {
            let mi = aligned[i];
            let mj = aligned[j];
            let same_predicted = predicted[mi] == predicted[mj];
            let same_gold = gold[mi] == gold[mj];
            match (same_predicted, same_gold) {
                (true, true) => tp += 1,
                (true, false) => fp += 1,
                (false, true) => fn_ += 1,
                (false, false) => {}
            }
            total_pairs += 1;
        }
    }
    let precision = if tp + fp == 0 {
        0.0
    } else {
        tp as f64 / (tp + fp) as f64
    };
    let recall = if tp + fn_ == 0 {
        0.0
    } else {
        tp as f64 / (tp + fn_) as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    PairwiseOutcome {
        precision,
        recall,
        f1,
        n_aligned_pairs: total_pairs,
    }
}

/// Convenience wrapper running both metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityResolutionReport {
    pub b_cubed: B3Outcome,
    pub pairwise: PairwiseOutcome,
}

pub fn score(predicted: &Clustering, gold: &Clustering) -> EntityResolutionReport {
    EntityResolutionReport {
        b_cubed: b_cubed(predicted, gold),
        pairwise: pairwise(predicted, gold),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clustering(pairs: &[(&str, &str)]) -> Clustering {
        pairs
            .iter()
            .map(|(m, c)| (m.to_string(), c.to_string()))
            .collect()
    }

    #[test]
    fn perfect_alignment_scores_1() {
        let predicted = clustering(&[
            ("Ken Lay", "C1"),
            ("Kenneth L. Lay", "C1"),
            ("klay@enron.com", "C1"),
            ("Jeff Skilling", "C2"),
        ]);
        let gold = clustering(&[
            ("Ken Lay", "G1"),
            ("Kenneth L. Lay", "G1"),
            ("klay@enron.com", "G1"),
            ("Jeff Skilling", "G2"),
        ]);
        let r = b_cubed(&predicted, &gold);
        assert!((r.precision - 1.0).abs() < 1e-9);
        assert!((r.recall - 1.0).abs() < 1e-9);
        assert!((r.f1 - 1.0).abs() < 1e-9);
        let p = pairwise(&predicted, &gold);
        assert!((p.f1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pre_reconciliation_floor_singletons_drop_recall() {
        // Every surface form its own cluster — the intentionally-bad
        // baseline Phase 3 establishes as the floor.
        let predicted = clustering(&[
            ("Ken Lay", "C1"),
            ("Kenneth L. Lay", "C2"),
            ("klay@enron.com", "C3"),
        ]);
        let gold = clustering(&[
            ("Ken Lay", "G1"),
            ("Kenneth L. Lay", "G1"),
            ("klay@enron.com", "G1"),
        ]);
        let r = b_cubed(&predicted, &gold);
        // Perfect precision (every singleton trivially "purely
        // contains" its one gold member). Recall floor at 1/3 (each
        // mention recovers only itself out of 3 gold-cluster members).
        assert!(
            (r.precision - 1.0).abs() < 1e-9,
            "precision {}",
            r.precision
        );
        assert!((r.recall - 1.0 / 3.0).abs() < 1e-9, "recall {}", r.recall);
        assert!(r.f1 < 0.6);
    }

    #[test]
    fn over_merged_cluster_drops_precision() {
        // Predicted merges Lay + Skilling into one cluster.
        let predicted = clustering(&[("Ken Lay", "C1"), ("Jeff Skilling", "C1")]);
        let gold = clustering(&[("Ken Lay", "G1"), ("Jeff Skilling", "G2")]);
        let r = b_cubed(&predicted, &gold);
        // Per-mention precision: both have cluster size 2, intersect
        // 1 → 0.5 each → mean 0.5.
        assert!((r.precision - 0.5).abs() < 1e-9);
        // Per-mention recall: each mention's gold cluster is size 1,
        // intersect 1 → recall 1.0 each → mean 1.0.
        assert!((r.recall - 1.0).abs() < 1e-9);
        let p = pairwise(&predicted, &gold);
        // One pair, predicted same, gold different → FP.
        assert_eq!(p.precision, 0.0);
    }

    #[test]
    fn unmatched_keys_surface_in_diagnostics() {
        let predicted = clustering(&[("a", "C1"), ("b", "C1"), ("c", "C2")]);
        let gold = clustering(&[("a", "G1"), ("b", "G1"), ("d", "G2")]);
        let r = b_cubed(&predicted, &gold);
        assert_eq!(r.unmatched_predicted, vec!["c".to_string()]);
        assert_eq!(r.unmatched_gold, vec!["d".to_string()]);
        // Only `a` + `b` align; they're correctly co-clustered.
        assert!((r.f1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn empty_input_returns_zero_without_panic() {
        let r = b_cubed(&Clustering::new(), &Clustering::new());
        assert_eq!(r.f1, 0.0);
        assert_eq!(r.n_aligned, 0);
    }

    #[test]
    fn pairwise_excludes_diagonal_and_counts_one_per_pair() {
        let predicted = clustering(&[("a", "C1"), ("b", "C1"), ("c", "C1")]);
        let gold = clustering(&[("a", "G1"), ("b", "G1"), ("c", "G1")]);
        let p = pairwise(&predicted, &gold);
        // 3 mentions → C(3,2) = 3 pairs.
        assert_eq!(p.n_aligned_pairs, 3);
        assert!((p.f1 - 1.0).abs() < 1e-9);
    }
}
