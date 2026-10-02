// SPDX-License-Identifier: AGPL-3.0-or-later
//! `admission_wire`'s tests: the one shed renderer and the Retry-After jitter,
//! moved with them from serving-host's admission tests (pb-svrn-serving-ports).

use super::*;

#[test]
fn shed_response_is_503_with_retry_after_and_the_openai_error_object() {
    let response = shed_response(AdmissionRejection::new(
        "busy",
        AdmissionReason::CeilingExceeded,
        7,
    ));
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "7");
}

#[test]
fn local_queue_shed_names_the_queue_position_and_predicted_wait() {
    let response = local_queue_shed_response(4, 30_000, 5);
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "5");
}
#[test]
fn jitter_stays_inside_the_base_plus_spread_window() {
    for base in [1u64, 2, 30] {
        for _ in 0..64 {
            let got = jittered_retry_after_secs(base);
            assert!(
                (base..base + RETRY_AFTER_JITTER_SPREAD_SECS).contains(&got),
                "base {base} produced {got}, outside [{base}, {})",
                base + RETRY_AFTER_JITTER_SPREAD_SECS
            );
        }
    }
}

#[test]
fn jitter_varies_across_calls() {
    let seen: std::collections::HashSet<u64> =
        (0..64).map(|_| jittered_retry_after_secs(2)).collect();
    assert!(seen.len() > 1, "a constant hint is the thundering herd");
}
