// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`heartbeat_verdict`](super::heartbeat_verdict)'s tests — moved verbatim
//! from `auto_ingest.rs`, nothing renamed.
use super::{
    heartbeat_verdict, AbortCause, HeartbeatOutcome, HeartbeatVerdict, MAX_HEARTBEAT_MISSES,
};

/// The failing input this gate exists for. `commonwealth-api`'s
/// heartbeat route answers 404 when the handoff is not in the
/// coordinator's store at all — a coordinator that restarted with
/// empty state, or a reaped handoff. Before this, the donor dropped
/// that status into a `debug!` catch-all and kept ingesting into a
/// lease nobody held until the unit finished.
#[test]
fn heartbeat_404_aborts_with_not_found() {
    assert_eq!(
        heartbeat_verdict(
            HeartbeatOutcome::Answered(reqwest::StatusCode::NOT_FOUND),
            0
        ),
        HeartbeatVerdict::Abort(AbortCause::NotFound)
    );
}

/// 410 is the arm that already worked; kept so the 404 fix cannot
/// be made by widening one match arm over both.
#[test]
fn heartbeat_410_aborts_with_gone() {
    assert_eq!(
        heartbeat_verdict(HeartbeatOutcome::Answered(reqwest::StatusCode::GONE), 0),
        HeartbeatVerdict::Abort(AbortCause::Gone)
    );
}

/// A coordinator that is simply unreachable never sends a 410, so
/// silence is the only signal. Driven through the same counter the
/// spawner keeps, so this proves the composed loop and not just the
/// arithmetic inside one call.
#[test]
fn heartbeat_three_misses_abort_with_silence() {
    let mut misses = 0u32;
    let mut verdicts = Vec::new();
    for _ in 0..MAX_HEARTBEAT_MISSES {
        let v = heartbeat_verdict(HeartbeatOutcome::NoAnswer, misses);
        misses = v.next_misses(misses);
        verdicts.push(v);
    }
    assert_eq!(
        verdicts,
        vec![
            HeartbeatVerdict::Miss,
            HeartbeatVerdict::Miss,
            HeartbeatVerdict::Abort(AbortCause::Silence),
        ],
        "the third beat in a row that does not land is the abort"
    );
}

/// What stops `heartbeat_three_misses_abort_with_silence` passing on
/// a counter that only ever increments: two misses, one renewal,
/// then two more misses must NOT abort — the run of three was
/// broken.
#[test]
fn heartbeat_success_resets_the_miss_counter() {
    let mut misses = 0u32;
    let mut step = |outcome| {
        let v = heartbeat_verdict(outcome, misses);
        misses = v.next_misses(misses);
        v
    };
    assert_eq!(step(HeartbeatOutcome::NoAnswer), HeartbeatVerdict::Miss);
    assert_eq!(step(HeartbeatOutcome::NoAnswer), HeartbeatVerdict::Miss);
    assert_eq!(
        step(HeartbeatOutcome::Answered(reqwest::StatusCode::OK)),
        HeartbeatVerdict::Renewed
    );
    assert_eq!(step(HeartbeatOutcome::NoAnswer), HeartbeatVerdict::Miss);
    assert_eq!(
        step(HeartbeatOutcome::NoAnswer),
        HeartbeatVerdict::Miss,
        "the renewal broke the run — this is the fifth failed beat but only the second in a row"
    );
}

/// A 5xx is the coordinator answering while broken. It is a miss,
/// not an abort: the lease may still be ours.
#[test]
fn heartbeat_500_is_a_miss_not_an_abort() {
    assert_eq!(
        heartbeat_verdict(
            HeartbeatOutcome::Answered(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
            0
        ),
        HeartbeatVerdict::Miss
    );
}
