//! Pre-reconciliation floor vs tuned delta — Phase 5 measurement
//! discipline on a synthetic clustering.
//!
//! Verifies the scorer behaves correctly on the two boundary cases
//! Phase 5 establishes:
//!   - **Pre-reconciliation floor**: every surface form its own
//!     cluster. Precision 1.0 (singletons trivially pure), recall
//!     dominated by `1 / mean_cluster_size`.
//!   - **Perfectly tuned**: identical to gold.
//!
//! The Phase 5 demoable check ("tuning measurably moved the number")
//! is the *delta* between the two — this test pins the delta sign so
//! a regression that silently flips reconciliation back into a
//! pre-floor state surfaces in CI.

use std::collections::BTreeMap;

use sovereign_eval::entity_resolution_score::{score, Clustering};

#[test]
fn pre_reconciliation_floor_vs_tuned_delta_is_positive() {
    let surface_forms = [
        ("Ken Lay", "person-ken-lay"),
        ("Kenneth Lay", "person-ken-lay"),
        ("Kenneth L. Lay", "person-ken-lay"),
        ("Jeff Skilling", "person-jeff-skilling"),
    ];
    let gold: Clustering = surface_forms
        .iter()
        .map(|(s, c)| (s.to_string(), c.to_string()))
        .collect();
    let pre: Clustering = surface_forms
        .iter()
        .enumerate()
        .map(|(i, (s, _))| (s.to_string(), format!("pre-cluster-{i}")))
        .collect();
    let tuned: Clustering = gold.clone();

    let pre_outcome = score(&pre, &gold);
    let tuned_outcome = score(&tuned, &gold);

    assert!((pre_outcome.b_cubed.precision - 1.0).abs() < 1e-9);
    assert!(pre_outcome.b_cubed.recall < 0.6);
    assert!((tuned_outcome.b_cubed.f1 - 1.0).abs() < 1e-9);
    let delta = tuned_outcome.b_cubed.f1 - pre_outcome.b_cubed.f1;
    assert!(
        delta > 0.0,
        "tuned F1 must exceed pre-reconciliation floor; delta = {delta}"
    );
}

#[test]
fn over_merging_drops_precision_and_surfaces_in_pairwise() {
    // Reconciler accidentally collapses Lay + Skilling into one
    // cluster — precision must drop, pairwise must show FP.
    let mut over: Clustering = BTreeMap::new();
    over.insert("Ken Lay".into(), "cluster-1".into());
    over.insert("Jeff Skilling".into(), "cluster-1".into());

    let mut gold: Clustering = BTreeMap::new();
    gold.insert("Ken Lay".into(), "person-ken-lay".into());
    gold.insert("Jeff Skilling".into(), "person-jeff-skilling".into());

    let outcome = score(&over, &gold);
    // Per-mention B³ precision: each member of cluster-1 has
    // gold-intersect 1 / predicted size 2 → 0.5.
    assert!((outcome.b_cubed.precision - 0.5).abs() < 1e-9);
    assert_eq!(outcome.pairwise.precision, 0.0);
}
