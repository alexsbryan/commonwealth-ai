// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn percentiles_are_nearest_rank() {
    let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
    assert_eq!(percentile(&xs, 50.0), 5.0);
    assert_eq!(percentile(&xs, 95.0), 10.0);
    assert_eq!(percentile(&xs, 100.0), 10.0);
    assert_eq!(percentile(&[], 50.0), 0.0);
}

#[test]
fn jain_is_one_when_equal_and_falls_with_concentration() {
    assert!((jains_index(&[5.0, 5.0, 5.0]) - 1.0).abs() < 1e-9);
    let concentrated = jains_index(&[15.0, 0.0, 0.0]);
    assert!((concentrated - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn cov_is_zero_for_a_flat_distribution() {
    assert!(coefficient_of_variation(&[3.0, 3.0, 3.0]).abs() < 1e-9);
    assert!(coefficient_of_variation(&[0.0, 0.0]).abs() < 1e-9);
    assert!(coefficient_of_variation(&[10.0, 0.0]) > 0.9);
}

#[test]
fn origin_is_recovered_from_a_sim_request_id_and_falls_back_otherwise() {
    let mut d = sovereign_scheduler::decision_log::DecisionBuilder::new(
        "d-sim-7-1234",
        "sim-7-1234",
        sovereign_scheduler::decision_log::DecisionPath::RankedOicp,
        sovereign_scheduler::decision_log::RequestFacts {
            capability_hint: "general".into(),
            latency_class: "Extended".into(),
            sharding: "MeshAllowed".into(),
            context_tokens: None,
            max_output_tokens: None,
            preferred_speed: "Slow".into(),
            explicit_model_id: None,
        },
    )
    .finish_at(Verdict::StayLocal, &[], 0);
    assert_eq!(origin_of(&d), "7");
    d.oicp_request_id = "wl-knowledge-42".into();
    assert_eq!(origin_of(&d), "wl-knowledge-42");
}
