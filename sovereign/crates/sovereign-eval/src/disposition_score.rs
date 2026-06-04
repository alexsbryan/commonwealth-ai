//! Disposition-classification scorer — the classification analog of
//! `entity_resolution_score`. Computes accuracy, macro-F1, per-category
//! precision/recall/F1, and a confusion matrix for a per-case
//! categorical prediction against frozen gold.
//!
//! Discipline mirrors `entity_resolution_score`: pure functions over
//! `BTreeMap`, all types `Serialize`/`Deserialize`, and every
//! 0-denominator metric resolves to `0.0` (never NaN). Only case ids
//! present in BOTH maps contribute to accuracy/F1; the rest surface in
//! `unmatched_*` so a coverage gap can't silently zero a metric.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Per-case category assignment: `case_id` → category token.
pub type Labeling = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerCategory {
    pub category: String,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    /// Number of gold cases in this category (the macro-F1 is unweighted
    /// across categories, so support is reported for interpretation).
    pub support: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfusionMatrix {
    /// Axis labels in stable order. `matrix[g][p]` counts cases whose
    /// gold == categories[g] and predicted == categories[p].
    pub categories: Vec<String>,
    pub matrix: Vec<Vec<usize>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DispositionReport {
    pub accuracy: f64,
    /// Unweighted mean of per-category F1 over the axis categories.
    pub macro_f1: f64,
    /// Cases scored (present in both `predicted` and `gold`).
    pub n_aligned: usize,
    pub confusion_matrix: ConfusionMatrix,
    pub per_category: Vec<PerCategory>,
    /// Case ids in `predicted` but not `gold` (hallucinated case ids).
    pub unmatched_predicted: Vec<String>,
    /// Case ids in `gold` but not `predicted` (the model never answered).
    pub unmatched_gold: Vec<String>,
}

/// Score against gold using the canonical axis derived from the data
/// (every category seen in gold or predicted, taxonomy order is the
/// caller's concern — here we sort the observed set for determinism).
pub fn score(predicted: &Labeling, gold: &Labeling) -> DispositionReport {
    let mut axis: BTreeSet<String> = BTreeSet::new();
    for v in predicted.values().chain(gold.values()) {
        axis.insert(v.clone());
    }
    let axis: Vec<String> = axis.into_iter().collect();
    score_with_axis(predicted, gold, &axis)
}

/// Score with an explicit category axis — used by the bench so the
/// confusion matrix is read against the era-masked label set. Categories
/// present in the data but absent from `axis` are appended to the axis
/// tail so they're never silently dropped.
pub fn score_with_axis(predicted: &Labeling, gold: &Labeling, axis: &[String]) -> DispositionReport {
    // Build the full axis: the supplied order first, then any
    // out-of-axis categories observed in the data, appended.
    let mut categories: Vec<String> = axis.to_vec();
    let known: BTreeSet<&String> = axis.iter().collect();
    let mut extra: BTreeSet<String> = BTreeSet::new();
    for v in predicted.values().chain(gold.values()) {
        if !known.contains(v) {
            extra.insert(v.clone());
        }
    }
    categories.extend(extra);

    let index: BTreeMap<&str, usize> = categories
        .iter()
        .enumerate()
        .map(|(i, c)| (c.as_str(), i))
        .collect();
    let n = categories.len();
    let mut matrix = vec![vec![0usize; n]; n];

    // Aligned set: case ids present in both maps.
    let mut unmatched_predicted: Vec<String> = Vec::new();
    let mut unmatched_gold: Vec<String> = gold
        .keys()
        .filter(|k| !predicted.contains_key(*k))
        .cloned()
        .collect();
    unmatched_gold.sort();

    let mut correct = 0usize;
    let mut n_aligned = 0usize;
    for (case_id, pred) in predicted {
        let Some(g) = gold.get(case_id) else {
            unmatched_predicted.push(case_id.clone());
            continue;
        };
        n_aligned += 1;
        if pred == g {
            correct += 1;
        }
        // Both pred and gold are guaranteed in the axis (axis ⊇ observed).
        let (gi, pi) = (index[g.as_str()], index[pred.as_str()]);
        matrix[gi][pi] += 1;
    }
    unmatched_predicted.sort();

    let accuracy = if n_aligned == 0 {
        0.0
    } else {
        correct as f64 / n_aligned as f64
    };

    // Per-category precision/recall/F1 from the confusion matrix.
    let mut per_category = Vec::with_capacity(n);
    let mut f1_sum = 0.0;
    for (ci, cat) in categories.iter().enumerate() {
        let tp = matrix[ci][ci];
        // Predicted as ci across all golds = column sum.
        let pred_total: usize = (0..n).map(|g| matrix[g][ci]).sum();
        // Gold ci across all preds = row sum (also the support).
        let gold_total: usize = matrix[ci].iter().sum();
        let precision = if pred_total == 0 {
            0.0
        } else {
            tp as f64 / pred_total as f64
        };
        let recall = if gold_total == 0 {
            0.0
        } else {
            tp as f64 / gold_total as f64
        };
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        f1_sum += f1;
        per_category.push(PerCategory {
            category: cat.clone(),
            precision,
            recall,
            f1,
            support: gold_total,
        });
    }
    let macro_f1 = if n == 0 { 0.0 } else { f1_sum / n as f64 };

    DispositionReport {
        accuracy,
        macro_f1,
        n_aligned,
        confusion_matrix: ConfusionMatrix { categories, matrix },
        per_category,
        unmatched_predicted,
        unmatched_gold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(pairs: &[(&str, &str)]) -> Labeling {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn perfect_classification_scores_one() {
        let gold = lab(&[("c1", "ASTRONOMICAL"), ("c2", "AIRCRAFT")]);
        let pred = gold.clone();
        let r = score(&pred, &gold);
        assert_eq!(r.accuracy, 1.0);
        assert_eq!(r.macro_f1, 1.0);
        assert_eq!(r.n_aligned, 2);
        // Diagonal only.
        for (gi, row) in r.confusion_matrix.matrix.iter().enumerate() {
            for (pi, &cell) in row.iter().enumerate() {
                if gi != pi {
                    assert_eq!(cell, 0);
                }
            }
        }
    }

    #[test]
    fn single_confusion_drops_precision_recall() {
        let gold = lab(&[("c1", "ASTRONOMICAL"), ("c2", "AIRCRAFT")]);
        let pred = lab(&[("c1", "AIRCRAFT"), ("c2", "AIRCRAFT")]);
        let r = score(&pred, &gold);
        assert_eq!(r.accuracy, 0.5);
        let astro = r
            .per_category
            .iter()
            .find(|p| p.category == "ASTRONOMICAL")
            .unwrap();
        assert_eq!(astro.recall, 0.0); // the one ASTRONOMICAL gold was missed
        let air = r.per_category.iter().find(|p| p.category == "AIRCRAFT").unwrap();
        assert!(air.precision < 1.0); // one of two AIRCRAFT predictions is wrong
    }

    #[test]
    fn macro_f1_is_unweighted_vs_accuracy() {
        // Imbalanced: 3 AIRCRAFT (all right) + 1 BIRD (wrong) → high
        // accuracy but macro-F1 pulled down by BIRD's 0 recall.
        let gold = lab(&[
            ("c1", "AIRCRAFT"),
            ("c2", "AIRCRAFT"),
            ("c3", "AIRCRAFT"),
            ("c4", "BIRD"),
        ]);
        let pred = lab(&[
            ("c1", "AIRCRAFT"),
            ("c2", "AIRCRAFT"),
            ("c3", "AIRCRAFT"),
            ("c4", "AIRCRAFT"),
        ]);
        let r = score(&pred, &gold);
        assert_eq!(r.accuracy, 0.75);
        assert!(r.macro_f1 < r.accuracy, "macro_f1={} acc={}", r.macro_f1, r.accuracy);
    }

    #[test]
    fn unmatched_keys_surface_without_zeroing_aligned() {
        let gold = lab(&[("c1", "AIRCRAFT"), ("c2", "BIRD")]);
        let pred = lab(&[("c1", "AIRCRAFT"), ("c3", "HOAX")]);
        let r = score(&pred, &gold);
        assert_eq!(r.n_aligned, 1);
        assert_eq!(r.accuracy, 1.0); // the one aligned case is correct
        assert_eq!(r.unmatched_predicted, vec!["c3".to_string()]);
        assert_eq!(r.unmatched_gold, vec!["c2".to_string()]);
    }

    #[test]
    fn empty_input_is_zero_not_nan() {
        let r = score(&Labeling::new(), &Labeling::new());
        assert_eq!(r.accuracy, 0.0);
        assert_eq!(r.macro_f1, 0.0);
        assert_eq!(r.n_aligned, 0);
    }

    #[test]
    fn axis_override_masks_and_appends_strays() {
        // Era-masked axis without SATELLITE; a stray SATELLITE prediction
        // still gets scored (appended to the axis tail), not dropped.
        let gold = lab(&[("c1", "ASTRONOMICAL")]);
        let pred = lab(&[("c1", "SATELLITE")]);
        let axis = vec!["ASTRONOMICAL".to_string(), "AIRCRAFT".to_string()];
        let r = score_with_axis(&pred, &gold, &axis);
        assert_eq!(r.accuracy, 0.0);
        assert!(r.confusion_matrix.categories.contains(&"SATELLITE".to_string()));
        // The supplied axis order is preserved at the head.
        assert_eq!(r.confusion_matrix.categories[0], "ASTRONOMICAL");
    }
}
