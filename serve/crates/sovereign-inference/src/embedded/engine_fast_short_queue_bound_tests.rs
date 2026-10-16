// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A coalescer whose receiver is held and never read — the only way
/// to genuinely FILL the queue without a resident model. Returned
/// alongside the receiver so the channel stays open (dropping it
/// would turn every `try_send` into `Closed`, which is a different
/// error and would let this test pass for the wrong reason).
fn full_coalescer(
    cap: usize,
    n_seq_max: usize,
) -> (
    FastShortCoalescer,
    tokio::sync::mpsc::Receiver<CoalescerJob>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<CoalescerJob>(cap);
    (FastShortCoalescer::with_sender(tx, cap, n_seq_max, 120), rx)
}

/// RED-FIRST (order mesh-scale-t0, item 4): "fill the coalescer,
/// assert a shed". On the pre-fix `unbounded_channel` there was no
/// cap to reach and `send` never refused, so no shed could EVER
/// fire on this path — the assertion at the end had nothing that
/// could produce it.
#[test]
fn a_full_coalescer_sheds_instead_of_growing() {
    let cap = 4;
    let (coalescer, _rx_held_open) = full_coalescer(cap, 2);

    // Fill it exactly to the bound. Every one of these is admitted;
    // `enqueue_job` hands back a receiver we deliberately keep so
    // the jobs stay queued.
    let mut admitted = Vec::new();
    for i in 0..cap {
        admitted.push(
            coalescer
                .enqueue_job(CompletionRequest::default())
                .unwrap_or_else(|e| panic!("request {i} should be admitted under cap: {e}")),
        );
    }
    assert_eq!(admitted.len(), cap);

    // One more. This is the request that used to disappear into an
    // unbounded queue.
    let err = coalescer
        .enqueue_job(CompletionRequest::default())
        .expect_err("a queue at its bound must shed, not grow");

    match err {
        Error::QueueShed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        } => {
            assert_eq!(
                position,
                cap as u32 + 1,
                "the shed must report the caller's real place in line"
            );
            // 5 in line at n_seq_max=2 → 3 batches ahead × 120ms.
            assert_eq!(predicted_wait_ms, 360);
            assert!(
                retry_after_secs >= 1,
                "Retry-After: 0 invites a hot loop against the host that just refused"
            );
        }
        other => panic!("expected the existing QueueShed shape, got: {other:?}"),
    }
}

/// Draining one job must make room again — a shed that latched would
/// be worse than the leak it replaced.
#[test]
fn the_queue_reopens_once_the_drainer_takes_one() {
    let cap = 2;
    let (coalescer, mut rx) = full_coalescer(cap, 8);
    let _a = coalescer.enqueue_job(CompletionRequest::default()).unwrap();
    let _b = coalescer.enqueue_job(CompletionRequest::default()).unwrap();
    assert!(coalescer.enqueue_job(CompletionRequest::default()).is_err());

    let taken = rx.try_recv().expect("drainer takes one");
    drop(taken);
    assert!(
        coalescer.enqueue_job(CompletionRequest::default()).is_ok(),
        "the bound must be a moving window, not a latch"
    );
}

/// The cap is an operator dial, and a nonsense value must not
/// silently produce a zero-capacity channel (which would shed
/// every request forever).
#[test]
fn a_bad_cap_env_falls_back_to_the_default() {
    // Not touching the process env — the parse/filter policy is the
    // thing under test and it is expressed here exactly as
    // `fast_short_queue_cap` expresses it.
    let parse = |s: &str| {
        s.parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(FAST_SHORT_QUEUE_CAP_DEFAULT)
    };
    assert_eq!(parse("0"), FAST_SHORT_QUEUE_CAP_DEFAULT);
    assert_eq!(parse("banana"), FAST_SHORT_QUEUE_CAP_DEFAULT);
    assert_eq!(parse("128"), 128);
}
