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
//! The standard metrics use *aligned* inputs — only mentions that appear in BOTH
//! `predicted` and `gold` are scored. Mentions in one but not the
//! other are flagged in [`B3Outcome::unmatched_predicted`] /
//! [`B3Outcome::unmatched_gold`] so the operator can see the
//! coverage gap without it silently zeroing the recall.
//! [`recovery_b_cubed`] is a separate, nonstandard recovery measure: missing
//! gold members earn zero recall and extra predictions earn zero precision.
//! Its cluster sizes use the full input sets. Scope predictions to the
//! evaluation universe first; known no-case placements belong in that set.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[path = "entity_resolution_recovery.rs"]
mod recovery;
pub use recovery::recovery_b_cubed;

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

/// Precision / recall / F1 of an entity-level metric (CEAF-e, LEA).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityOutcome {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

impl EntityOutcome {
    fn of(precision: f64, recall: f64) -> Self {
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        Self {
            precision,
            recall,
            f1,
        }
    }
}

/// Each side's clusters over the mentions both sides hold, as `b_cubed`
/// scores them.
fn aligned_clusters(
    predicted: &Clustering,
    gold: &Clustering,
) -> (Vec<BTreeSet<String>>, Vec<BTreeSet<String>>) {
    let mut p: BTreeMap<&String, BTreeSet<String>> = BTreeMap::new();
    let mut g: BTreeMap<&String, BTreeSet<String>> = BTreeMap::new();
    for (m, pc) in predicted {
        if let Some(gc) = gold.get(m) {
            p.entry(pc).or_default().insert(m.clone());
            g.entry(gc).or_default().insert(m.clone());
        }
    }
    (p.into_values().collect(), g.into_values().collect())
}

/// Entity-level CEAF (Luo 2005, φ4): the one-to-one alignment of predicted
/// to gold entities maximising the summed Dice of their mention sets, so a
/// split and a merge both cost. Precision divides by the predicted entity
/// count, recall by the gold. The alignment is exact (Kuhn–Munkres), run per
/// connected component of entities that share a mention — entities sharing
/// none contribute nothing — so a greedy pick never decides a tie.
pub fn ceaf_e(predicted: &Clustering, gold: &Clustering) -> EntityOutcome {
    let (p, g) = aligned_clusters(predicted, gold);
    if p.is_empty() {
        return EntityOutcome::default();
    }
    let dice = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
        2.0 * a.intersection(b).count() as f64 / (a.len() + b.len()) as f64
    };
    let mut gold_of: BTreeMap<&String, usize> = BTreeMap::new();
    for (j, c) in g.iter().enumerate() {
        for m in c {
            gold_of.insert(m, j);
        }
    }
    // Components over the bipartite overlap graph: predicted i joins gold j
    // when they share a mention.
    let mut parent: Vec<usize> = (0..p.len() + g.len()).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for (i, c) in p.iter().enumerate() {
        for m in c {
            let (a, b) = (
                root(&mut parent, i),
                root(&mut parent, p.len() + gold_of[m]),
            );
            parent[a] = b;
        }
    }
    let mut components: BTreeMap<usize, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for i in 0..p.len() {
        let r = root(&mut parent, i);
        components.entry(r).or_default().0.push(i);
    }
    for j in 0..g.len() {
        let r = root(&mut parent, p.len() + j);
        components.entry(r).or_default().1.push(j);
    }
    let total: f64 = components
        .values()
        .filter(|(pi, gj)| !pi.is_empty() && !gj.is_empty())
        .map(|(pi, gj)| {
            let sims: Vec<Vec<f64>> = pi
                .iter()
                .map(|&i| gj.iter().map(|&j| dice(&p[i], &g[j])).collect())
                .collect();
            max_assignment(&sims)
        })
        .sum();
    EntityOutcome::of(total / p.len() as f64, total / g.len() as f64)
}

/// The largest summed weight of a one-to-one assignment over a rectangular
/// matrix of non-negative weights (Kuhn–Munkres with potentials, O(n³) on the
/// padded square).
fn max_assignment(w: &[Vec<f64>]) -> f64 {
    let rows = w.len();
    let cols = w.first().map_or(0, Vec::len);
    let n = rows.max(cols);
    if n == 0 {
        return 0.0;
    }
    let top = w.iter().flatten().copied().fold(0.0_f64, f64::max);
    // minimise cost = top - weight on the padded square (padding weighs 0)
    let cost = |i: usize, j: usize| top - if i < rows && j < cols { w[i][j] } else { 0.0 };
    let (mut u, mut v) = (vec![0.0; n + 1], vec![0.0; n + 1]);
    let (mut matched, mut way) = (vec![0usize; n + 1], vec![0usize; n + 1]);
    for i in 1..=n {
        matched[0] = i;
        let mut j0 = 0;
        let mut minv = vec![f64::INFINITY; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let (i0, mut delta, mut j1) = (matched[j0], f64::INFINITY, 0);
            for j in 1..=n {
                if !used[j] {
                    let cur = cost(i0 - 1, j - 1) - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[matched[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if matched[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            matched[j0] = matched[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    (1..=n)
        .filter(|&j| matched[j] >= 1 && matched[j] <= rows && j <= cols)
        .map(|j| w[matched[j] - 1][j - 1])
        .sum()
}

/// LEA (Moosavi & Strube 2016): each entity weighted by its size, scored by
/// the share of its coreference links the other side keeps. A singleton has
/// one self-link, kept when the other side also holds it alone (the
/// reference scorer's rule). Recall reads gold entities against predicted,
/// precision the reverse.
pub fn lea(predicted: &Clustering, gold: &Clustering) -> EntityOutcome {
    let (p, g) = aligned_clusters(predicted, gold);
    if p.is_empty() {
        return EntityOutcome::default();
    }
    fn side(entities: &[BTreeSet<String>], other: &[BTreeSet<String>]) -> f64 {
        let mut of: BTreeMap<&String, usize> = BTreeMap::new();
        for (k, c) in other.iter().enumerate() {
            for m in c {
                of.insert(m, k);
            }
        }
        let (mut num, mut den) = (0.0, 0.0);
        for e in entities {
            let members: Vec<&String> = e.iter().collect();
            let kept = if members.len() == 1 {
                f64::from(other[of[members[0]]].len() == 1)
            } else {
                let links = (members.len() * (members.len() - 1) / 2) as f64;
                let mut common = 0usize;
                for (i, a) in members.iter().enumerate() {
                    common += members[i + 1..]
                        .iter()
                        .filter(|b| of[*a] == of[**b])
                        .count();
                }
                common as f64 / links
            };
            num += members.len() as f64 * kept;
            den += members.len() as f64;
        }
        num / den
    }
    EntityOutcome::of(side(&p, &g), side(&g, &p))
}

/// MUC (Vilain et al. 1995): the links each side keeps. Recall counts, per
/// gold entity, its size less the number of predicted clusters it is split
/// across, over its size less one; precision the reverse.
pub fn muc(predicted: &Clustering, gold: &Clustering) -> EntityOutcome {
    let (p, g) = aligned_clusters(predicted, gold);
    if p.is_empty() {
        return EntityOutcome::default();
    }
    fn side(entities: &[BTreeSet<String>], other: &[BTreeSet<String>]) -> f64 {
        let mut of: BTreeMap<&String, usize> = BTreeMap::new();
        for (k, c) in other.iter().enumerate() {
            for m in c {
                of.insert(m, k);
            }
        }
        let (mut num, mut den) = (0usize, 0usize);
        for e in entities {
            let parts: BTreeSet<usize> = e.iter().map(|m| of[m]).collect();
            num += e.len() - parts.len();
            den += e.len() - 1;
        }
        if den == 0 {
            0.0
        } else {
            num as f64 / den as f64
        }
    }
    EntityOutcome::of(side(&p, &g), side(&g, &p))
}

/// Every metric this module computes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityResolutionReport {
    pub b_cubed: B3Outcome,
    /// Coverage-aware extension; absent in historical reports, not part of CoNLL F1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_b_cubed: Option<B3Outcome>,
    pub pairwise: PairwiseOutcome,
    #[serde(default)]
    pub ceaf_e: EntityOutcome,
    #[serde(default)]
    pub lea: EntityOutcome,
    #[serde(default)]
    pub muc: EntityOutcome,
    /// The CoNLL-2012 score the coreference literature reports: the mean F1 of
    /// MUC, B³ and CEAF-e.
    #[serde(default)]
    pub conll_f1: f64,
}

pub fn score(predicted: &Clustering, gold: &Clustering) -> EntityResolutionReport {
    EntityResolutionReport {
        b_cubed: b_cubed(predicted, gold),
        recovery_b_cubed: Some(recovery_b_cubed(predicted, gold)),
        pairwise: pairwise(predicted, gold),
        ceaf_e: ceaf_e(predicted, gold),
        lea: lea(predicted, gold),
        muc: muc(predicted, gold),
        conll_f1: 0.0,
    }
    .with_conll()
}

impl EntityResolutionReport {
    fn with_conll(mut self) -> Self {
        self.conll_f1 = (self.muc.f1 + self.b_cubed.f1 + self.ceaf_e.f1) / 3.0;
        self
    }
}

#[cfg(test)]
#[path = "entity_resolution_recovery_tests.rs"]
mod recovery_tests;

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

    /// Key {a,b,c} {d,e,f,g} against response {a,b} {c,d} {f,g} {e} — the
    /// LEA paper's shape with every mention aligned. Values computed apart
    /// from this code (brute-force alignment, the reference LEA rule).
    #[test]
    fn ceaf_e_and_lea_score_a_split_and_a_cross_link() {
        let gold = clustering(&[
            ("a", "K1"),
            ("b", "K1"),
            ("c", "K1"),
            ("d", "K2"),
            ("e", "K2"),
            ("f", "K2"),
            ("g", "K2"),
        ]);
        let predicted = clustering(&[
            ("a", "R1"),
            ("b", "R1"),
            ("c", "R2"),
            ("d", "R2"),
            ("f", "R3"),
            ("g", "R3"),
            ("e", "R4"),
        ]);
        let c = ceaf_e(&predicted, &gold);
        assert!(
            (c.precision - 1.4666666666666668 / 4.0).abs() < 1e-9,
            "{c:?}"
        );
        assert!((c.recall - 1.4666666666666668 / 2.0).abs() < 1e-9, "{c:?}");
        let l = lea(&predicted, &gold);
        assert!((l.precision - 4.0 / 7.0).abs() < 1e-9, "{l:?}");
        assert!((l.recall - (1.0 + 4.0 / 6.0) / 7.0).abs() < 1e-9, "{l:?}");
        let perfect = lea(&gold, &gold);
        assert!((perfect.f1 - 1.0).abs() < 1e-9 && (ceaf_e(&gold, &gold).f1 - 1.0).abs() < 1e-9);
    }

    /// Vilain's example (key {a,b,c,d}, response {a,b} {c,d}) and the LEA-paper
    /// case; values computed apart from this code. CoNLL is the mean of the
    /// three F1s.
    #[test]
    fn muc_matches_vilain_and_conll_is_the_mean_of_three() {
        let gold = clustering(&[("a", "K"), ("b", "K"), ("c", "K"), ("d", "K")]);
        let predicted = clustering(&[("a", "R1"), ("b", "R1"), ("c", "R2"), ("d", "R2")]);
        let m = muc(&predicted, &gold);
        assert!(
            (m.precision - 1.0).abs() < 1e-9 && (m.recall - 2.0 / 3.0).abs() < 1e-9,
            "{m:?}"
        );
        let gold = clustering(&[
            ("a", "K1"),
            ("b", "K1"),
            ("c", "K1"),
            ("d", "K2"),
            ("e", "K2"),
            ("f", "K2"),
            ("g", "K2"),
        ]);
        let predicted = clustering(&[
            ("a", "R1"),
            ("b", "R1"),
            ("c", "R2"),
            ("d", "R2"),
            ("f", "R3"),
            ("g", "R3"),
            ("e", "R4"),
        ]);
        let r = score(&predicted, &gold);
        assert!(
            (r.muc.precision - 2.0 / 3.0).abs() < 1e-9 && (r.muc.recall - 0.4).abs() < 1e-9,
            "{:?}",
            r.muc
        );
        // computed apart: MUC F1 .5, B³ F1 .592, CEAF-e F1 .489
        assert!(
            (r.conll_f1 - 0.5270322270322271).abs() < 1e-9,
            "{}",
            r.conll_f1
        );
    }

    /// The assignment is exact: greedy takes 0.6 then 0.0 here, the optimum
    /// is 0.55 + 0.5. And on small random matrices it equals brute force.
    #[test]
    fn max_assignment_is_the_optimum_not_the_greedy_pick() {
        assert!((max_assignment(&[vec![0.6, 0.55], vec![0.5, 0.0]]) - 1.05).abs() < 1e-9);
        fn brute(w: &[Vec<f64>], row: usize, used: &mut Vec<bool>) -> f64 {
            if row == w.len() {
                return 0.0;
            }
            let mut best = brute(w, row + 1, used); // row left unassigned
            for j in 0..w[row].len() {
                if !used[j] {
                    used[j] = true;
                    best = best.max(w[row][j] + brute(w, row + 1, used));
                    used[j] = false;
                }
            }
            best
        }
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 1000) as f64 / 1000.0
        };
        for case in 0..300 {
            let (rows, cols) = (1 + case % 5, 1 + (case / 5) % 5);
            let w: Vec<Vec<f64>> = (0..rows)
                .map(|_| {
                    (0..cols)
                        .map(|_| if next() < 0.3 { 0.0 } else { next() })
                        .collect()
                })
                .collect();
            let want = brute(&w, 0, &mut vec![false; cols]);
            let got = max_assignment(&w);
            assert!(
                (want - got).abs() < 1e-9,
                "case {case}: {w:?} brute {want} got {got}"
            );
        }
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
