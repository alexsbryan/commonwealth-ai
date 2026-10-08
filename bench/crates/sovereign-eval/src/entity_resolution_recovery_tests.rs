// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{score, Clustering, EntityResolutionReport};
use serde_json::Value;

fn clustering(pairs: &[(&str, &str)]) -> Clustering {
    pairs
        .iter()
        .map(|(m, c)| (m.to_string(), c.to_string()))
        .collect()
}

fn report(predicted: &[(&str, &str)], gold: &[(&str, &str)]) -> Value {
    serde_json::to_value(score(&clustering(predicted), &clustering(gold))).unwrap()
}

fn check(value: &Value, precision: f64, recall: f64, f1: f64) {
    let recovery = value
        .get("recovery_b_cubed")
        .expect("report must expose recovery, separately from conditional clustering");
    for (key, expected) in [("precision", precision), ("recall", recall), ("f1", f1)] {
        let actual = recovery[key].as_f64().expect("metric must be a number");
        assert!(
            (actual - expected).abs() < 1e-9,
            "{key}: {actual} != {expected}"
        );
    }
}

#[test]
fn partial_recovery_keeps_full_gold_denominator() {
    let value = report(
        &[("a", "p1"), ("c", "p2")],
        &[("a", "g1"), ("b", "g1"), ("c", "g2"), ("d", "g2")],
    );
    assert_eq!(value["b_cubed"]["f1"], 1.0);
    check(&value, 1.0, 0.25, 0.4);
}

#[test]
fn omitted_singleton_receives_no_recovery_credit() {
    check(
        &report(&[("a", "p")], &[("a", "g1"), ("b", "g2")]),
        1.0,
        0.5,
        2.0 / 3.0,
    );
}

#[test]
fn empty_prediction_recovers_nothing() {
    check(&report(&[], &[("a", "g")]), 0.0, 0.0, 0.0);
    check(&report(&[], &[]), 0.0, 0.0, 0.0);
    check(&report(&[("noise", "p")], &[]), 0.0, 0.0, 0.0);
}

#[test]
fn perfect_recovery_includes_singletons() {
    check(
        &report(
            &[("a", "p1"), ("b", "p1"), ("c", "p2")],
            &[("a", "g1"), ("b", "g1"), ("c", "g2")],
        ),
        1.0,
        1.0,
        1.0,
    );
}

#[test]
fn noise_penalizes_separate_and_contaminated_records() {
    let gold = [("a", "g"), ("b", "g")];
    let separate = report(&[("a", "p"), ("b", "p"), ("noise", "other")], &gold);
    check(&separate, 2.0 / 3.0, 1.0, 0.8);
    let mixed = report(&[("a", "p"), ("b", "p"), ("noise", "p")], &gold);
    assert_eq!(mixed["b_cubed"]["f1"], 1.0);
    check(&mixed, 4.0 / 9.0, 1.0, 8.0 / 13.0);
    assert_eq!(
        mixed["recovery_b_cubed"]["unmatched_predicted"],
        serde_json::json!(["noise"])
    );
}

#[test]
fn recovery_still_penalizes_splits_and_merges() {
    check(
        &report(&[("a", "p1"), ("b", "p2")], &[("a", "g"), ("b", "g")]),
        1.0,
        0.5,
        2.0 / 3.0,
    );
    check(
        &report(&[("a", "p"), ("b", "p")], &[("a", "g1"), ("b", "g2")]),
        0.5,
        1.0,
        2.0 / 3.0,
    );
}

#[test]
fn recovery_is_invariant_to_cluster_labels_and_input_order() {
    let first = report(
        &[("a", "p"), ("b", "p"), ("noise", "n")],
        &[("a", "g"), ("b", "g")],
    );
    let renamed = report(
        &[("noise", "z"), ("b", "x"), ("a", "x")],
        &[("b", "y"), ("a", "y")],
    );
    assert_eq!(first, renamed);
}

#[test]
fn reports_written_before_recovery_remain_readable() {
    let mut old = report(&[("a", "p")], &[("a", "g")]);
    old.as_object_mut().unwrap().remove("recovery_b_cubed");
    let parsed: EntityResolutionReport = serde_json::from_value(old).unwrap();
    assert_eq!(parsed.b_cubed.f1, 1.0);
    assert!(
        parsed.recovery_b_cubed.is_none(),
        "historical absence is not a measured zero"
    );
}
