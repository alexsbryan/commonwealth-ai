// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validating the INSTRUMENT and the BOUND (ARCH_PRINCIPLES §18.4).
//! These exist because the depth gauge sizes MESH_N4_TOPOLOGY's M5
//! bound, and a counter that drifts would send that decision
//! confidently wrong.
use super::{acquire_with_queue_gauge, SlotQueue};
use sovereign_contracts::Error;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// A queue with the shed bound pinned, so no test depends on ambient env.
fn queue(seed_ms: u64, max_wait_ms: u64) -> Arc<SlotQueue> {
    Arc::new(SlotQueue::new("slot-a", seed_ms).with_max_wait_ms(max_wait_ms))
}

/// Acquire, expecting a SHED — bounded so a regression FAILS instead of
/// hanging.
///
/// If the bound stops working, the caller parks behind a permit these
/// tests deliberately never release, and an unbounded `.await` would hang
/// forever. Verified 2026-08-06 by disabling the shed check: the test
/// suite wedged with no output rather than reporting. A gate that hangs
/// on the failure it exists to catch is worse than no gate.
async fn expect_shed(q: &Arc<SlotQueue>, phase: &'static str) -> Error {
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        acquire_with_queue_gauge(q, phase, None),
    )
    .await
    {
        Ok(Ok(_permit)) => panic!("{phase}: expected a shed, got a permit"),
        Ok(Err(e)) => e,
        Err(_) => panic!(
            "{phase}: expected an IMMEDIATE shed but the caller parked — \
             the predicted-wait bound is not firing"
        ),
    }
}

/// Spin (yielding) until `cond` holds, or FAIL after a deadline.
///
/// Bounded deliberately: with the drop guard sabotaged, an unbounded
/// `while gauge != 0 { yield }` hangs forever instead of asserting —
/// verified 2026-08-06. A gate that hangs on the failure it was written
/// to catch is a worse gate than none: CI reports a timeout with no
/// message rather than the leak.
async fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn uncontended_acquire_never_touches_the_gauge() {
    let q = queue(1_000, 30_000);
    let permit = acquire_with_queue_gauge(&q, "test", None)
        .await
        .expect("free permit must be granted");
    // The fast path is the common case on an idle host: it must not
    // register as a queued caller, or every idle request would inflate
    // the depth M5 is sized against.
    assert_eq!(q.depth(), 0);
    drop(permit);
    assert_eq!(q.depth(), 0);
}

/// `load_reading` is the sibling pool's ONE load decider (order
/// mesh-scale-t1-streampool): it must count the permit HOLDER as well
/// as the parked waiters, and return to zero when both clear. A
/// reading that missed the holder would make an occupied sibling look
/// idle and defeat least-loaded picking entirely.
#[tokio::test]
async fn load_reading_counts_holder_plus_waiters_and_returns_to_zero() {
    let q = queue(1_000, 0); // bound disabled — waiters park
    assert_eq!(q.load_reading(), 0, "idle queue must read zero");

    let holder = acquire_with_queue_gauge(&q, "test/holder", None)
        .await
        .expect("free permit must be granted");
    assert_eq!(q.load_reading(), 1, "one holder, no waiters");

    let q2 = Arc::clone(&q);
    let waiter =
        tokio::spawn(async move { acquire_with_queue_gauge(&q2, "test/waiter", None).await });
    wait_until(|| q.depth() == 1, "the waiter to register in the gauge").await;
    assert_eq!(q.load_reading(), 2, "holder + one parked waiter");

    drop(holder);
    let promoted = waiter
        .await
        .expect("waiter task must not panic")
        .expect("waiter must be promoted to holder");
    assert_eq!(q.load_reading(), 1, "waiter promoted to holder");
    drop(promoted);
    assert_eq!(q.load_reading(), 0, "all clear reads zero again");
}

#[tokio::test]
async fn contended_acquire_reports_depth_then_returns_to_zero() {
    // Bound disabled so this test measures the GAUGE, not the shed.
    let q = queue(1_000, 0);
    let held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("first caller takes the permit");

    let mut waiters = Vec::new();
    for _ in 0..2 {
        let q = Arc::clone(&q);
        waiters.push(tokio::spawn(async move {
            acquire_with_queue_gauge(&q, "waiter", None)
                .await
                .map(|p| drop(p))
        }));
    }

    wait_until(|| q.depth() >= 2, "both waiters to park").await;
    assert_eq!(
        q.depth(),
        2,
        "both parked callers must be visible as queue depth — this is \
         the number that was invisible before the accessor existed"
    );

    drop(held);
    for w in waiters {
        w.await.expect("waiter task").expect("waiter got a permit");
    }
    assert_eq!(
        q.depth(),
        0,
        "gauge must return to zero once the queue drains"
    );
}

#[tokio::test]
async fn cancelled_waiter_does_not_leak_the_gauge() {
    // THE REGRESSION FENCE for `QueuedGuard`. A client that hangs up
    // mid-wait (dropped SSE connection, cancelled chat turn) drops the
    // acquiring future between the increment and the await's completion.
    // Without the drop guard the counter ratchets up and never comes
    // down, so a host that had merely seen some disconnects would report
    // a permanently deep queue — and would then SHED on the strength of
    // a fiction, refusing work it could serve.
    let q = queue(1_000, 0);
    let held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("first caller takes the permit");

    let qc = Arc::clone(&q);
    let waiter = tokio::spawn(async move {
        let _ = acquire_with_queue_gauge(&qc, "doomed", None).await;
    });
    wait_until(|| q.depth() >= 1, "the waiter to park").await;

    waiter.abort();
    let _ = waiter.await;

    wait_until(
        || q.depth() == 0,
        "the cancelled waiter to release its gauge slot",
    )
    .await;
    assert_eq!(
        q.depth(),
        0,
        "a cancelled waiter must release its gauge slot"
    );
    drop(held);
}

#[tokio::test]
async fn predicted_wait_over_the_bound_sheds_without_parking() {
    // Seed 10 s/turn against a 5 s bound: the first waiter's predicted
    // wait is 10 s, so it must be refused rather than parked.
    let q = queue(10_000, 5_000);
    let _held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("first caller takes the permit");

    let err = expect_shed(&q, "complete_stream_with_finish/lazy").await;

    match err {
        Error::QueueShed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        } => {
            assert_eq!(position, 1, "shed caller was next in line");
            assert_eq!(predicted_wait_ms, 10_000);
            assert_eq!(retry_after_secs, 10, "Retry-After hints the real wait");
        }
        other => panic!("expected a structured QueueShed, got: {other:?}"),
    }
    // Shedding must happen BEFORE parking — a caller that pays the wait
    // AND gets refused is the worst of both worlds.
    assert_eq!(q.depth(), 0, "a shed caller must never have parked");
}

/// **TWO ADMITTED TURNS THROUGH A ONE-SLOT GAUGE** — the case the whole
/// admission change exists for, and the one to sabotage.
///
/// Turn A holds the only permit. Turn B is the continuation of a turn
/// this host already admitted (a judge call, or its draft) and its
/// predicted wait is double the bound. Before this change it was refused
/// with `QueueShed` and a 30 s retry hint, which on the live host meant
/// `judge_failed_open` with zero calls answered, or a turn that died
/// whole at the draft. It must PARK, and it must get the permit when A
/// finishes.
///
/// SABOTAGE, run before trusting the green (ARCH §18.1): drop the
/// `Some(turn)` arm from the pre-park gate in
/// [`acquire_with_queue_gauge`] — i.e. ignore the lease — and this test
/// fails at `expect("turn B must be parked, not shed")` with the
/// `QueueShed` that the contended run reports as `queue_shed`.
#[tokio::test]
async fn an_admitted_continuation_parks_where_a_fresh_request_sheds() {
    // Seed 10 s/turn against a 5 s bound: predicted wait 10 s, double
    // the bound — identical conditions for both callers.
    let q = queue(10_000, 5_000);
    let turn_a = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("turn A takes the permit");

    // THE CONTROL, in the same test rather than a separate one: the
    // fresh-request path must be byte-identical. A test that only
    // watched the admitted side would pass just as well if the bound
    // had been deleted outright.
    let shed = expect_shed(&q, "fresh").await;
    assert!(
        matches!(shed, Error::QueueShed { .. }),
        "an UNADMITTED caller must still be shed: {shed:?}"
    );
    assert_eq!(q.depth(), 0, "the shed caller never parked");

    let admitted = sovereign_contracts::types::TurnAdmission::new("turn-b");
    let qc = Arc::clone(&q);
    let waiter = tokio::spawn(async move {
        acquire_with_queue_gauge(&qc, "complete/lazy", Some(&admitted)).await
    });
    wait_until(
        || q.depth() >= 1,
        "the admitted continuation to PARK rather than be shed",
    )
    .await;

    drop(turn_a);
    let permit = tokio::time::timeout(std::time::Duration::from_secs(3), waiter)
        .await
        .expect("the parked continuation must be served once the slot frees")
        .expect("waiter task")
        .expect("turn B must be parked, not shed");
    drop(permit);
    assert_eq!(q.depth(), 0, "the gauge drains after the continuation runs");
}

/// The admitted park is WIDER, not unbounded. The liveness guarantee the
/// post-park timeout exists for (a wl-judge call parked 62 minutes
/// behind a stuck slot, with no outcome event) still holds for a
/// continuation — it just waits `ADMITTED_PARK_MULTIPLE` times as long
/// before calling the slot stuck.
///
/// SABOTAGE: make `bound_ms` unconditional (`queue.max_wait_ms`) and the
/// admitted caller sheds at 50 ms instead of 200, failing the elapsed
/// assertion; remove the timeout entirely and the test hangs at its own
/// 3 s ceiling rather than reporting.
#[tokio::test]
async fn an_admitted_park_is_widened_but_still_bounded() {
    // 50 ms bound, and a holder that never releases: the only way out is
    // the post-park timeout.
    let q = queue(10_000, 50);
    let _stuck = acquire_with_queue_gauge(&q, "stuck-holder", None)
        .await
        .expect("the stuck holder takes the permit");

    let admitted = sovereign_contracts::types::TurnAdmission::new("turn-b");
    let started = std::time::Instant::now();
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        acquire_with_queue_gauge(&q, "complete/lazy", Some(&admitted)),
    )
    .await
    .expect("the admitted park must still END — an unbounded park is the defect this bounds");
    let waited = started.elapsed();

    match out {
        Err(Error::QueueShed { .. }) | Ok(_) => {}
        Err(ref other) => panic!("expected a bounded QueueShed, got: {other:?}"),
    }
    assert!(
        out.is_err(),
        "a permit that never frees must eventually shed even an admitted caller"
    );
    assert!(
        waited
            >= std::time::Duration::from_millis(50 * u64::from(super::ADMITTED_PARK_MULTIPLE)),
        "the admitted caller waited {waited:?}, less than {}x the {}ms bound — \
         it was cut off at the FRESH bound",
        super::ADMITTED_PARK_MULTIPLE,
        50
    );
}

#[tokio::test]
async fn zero_bound_never_sheds() {
    // `0` is the escape hatch back to the pre-M5 behaviour every
    // deployment ran before the bound existed. A wildly over-budget
    // predicted wait must still park.
    let q = queue(3_600_000, 0);
    let held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("first caller takes the permit");

    let qc = Arc::clone(&q);
    let waiter =
        tokio::spawn(async move { acquire_with_queue_gauge(&qc, "waiter", None).await });
    wait_until(
        || q.depth() >= 1,
        "the waiter to park despite a 1 h estimate",
    )
    .await;
    drop(held);
    assert!(
        waiter.await.expect("task").is_ok(),
        "must be served, not shed"
    );
}

/// THE daemon-empty-candidates wedge, as a unit shape (order
/// daemon-empty-candidates-error, 2026-08-18). The audit's wl-judge
/// call parked behind a primary-slot generation that did not complete
/// for ~25 minutes: the EWMA only folds COMPLETED turns, so the
/// predicted wait stayed under the 30 s bound, the pre-park shed
/// never fired, and the ACTUAL wait ran to the hour — no outcome
/// event, no error, the client hung forever.
///
/// Here the holder NEVER releases, which is exactly the wedge shape.
/// The seed (100 ms) sits under the bound (200 ms), so the pre-park
/// check lets the caller park — and the PARK itself must still shed
/// with the same named `Error::QueueShed`, within the queue's own
/// bound, not the test's outer timeout.
#[tokio::test]
async fn a_stuck_slot_sheds_with_a_named_error_instead_of_parking_forever() {
    let q = queue(100, 200); // seed under the bound -> parks; bound = 200 ms
    let _held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("first caller takes the permit");

    let started = std::time::Instant::now();
    // Bounded by the QUEUE's bound, not the test's: if the
    // park-timeout regresses, the caller parks forever and this
    // 3 s outer timeout fails the test instead of hanging the suite
    // (a gate that hangs on the failure it exists to catch is worse
    // than none — the same rule `expect_shed` lives by).
    let err = match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        acquire_with_queue_gauge(&q, "complete/lazy", None),
    )
    .await
    {
        Ok(Ok(_permit)) => panic!("expected a shed, got a permit"),
        Ok(Err(e)) => e,
        Err(_) => panic!(
            "the caller parked for 3 s behind a stuck holder — the park \
             itself is unbounded; the queue's wait bound must shed it"
        ),
    };
    let waited_ms = started.elapsed().as_millis() as u64;

    match err {
        Error::QueueShed {
            position,
            predicted_wait_ms,
            retry_after_secs,
        } => {
            assert_eq!(position, 1, "the shed caller was next in line");
            assert_eq!(
                predicted_wait_ms, 200,
                "the reported wait is the bound actually hit"
            );
            assert_eq!(retry_after_secs, 1, "Retry-After never drops below 1");
        }
        other => panic!("expected a structured QueueShed, got: {other:?}"),
    }
    assert!(
        waited_ms < 1_500,
        "the wait must be bounded by the queue's own 200 ms bound, not the \
         test's outer timeout (waited {waited_ms} ms)"
    );
    assert_eq!(q.depth(), 0, "a shed caller must not stay parked");
}

#[tokio::test]
async fn the_bound_adapts_to_observed_turn_duration() {
    // THE LOAD-BEARING TEST. The whole reason M5 bounds on predicted
    // WAIT rather than on depth is that the same depth cost 6.2 s in one
    // measured shape and 90.7 s in another. That only works if the
    // estimate tracks reality, so: identical depth, identical bound —
    // and the verdict must flip purely on observed turn duration.
    let q = queue(100, 5_000);

    // Depth 1 at a 100 ms estimate: comfortably admitted.
    let held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("holder");
    let qc = Arc::clone(&q);
    let waiter =
        tokio::spawn(async move { acquire_with_queue_gauge(&qc, "fast-turn", None).await });
    wait_until(|| q.depth() >= 1, "the cheap waiter to park").await;
    drop(held);
    let first = waiter.await.expect("task").expect("cheap turn is admitted");
    drop(first);

    // Now teach the queue that turns are expensive. Same call, same
    // depth, same bound — only the observed duration differs.
    for _ in 0..8 {
        q.record_turn(60_000);
    }

    let _held2 = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("holder");
    let err = expect_shed(&q, "slow-turn").await;
    assert!(
        matches!(err, Error::QueueShed { .. }),
        "expected QueueShed, got: {err:?}"
    );
}

#[tokio::test]
async fn closed_semaphore_names_the_phase_in_the_error() {
    // A torn-down slot must produce an error that says WHICH entry
    // point died — a bare "permit closed" left the caller guessing
    // across thirteen call sites.
    let q = queue(1_000, 0);
    let _held = acquire_with_queue_gauge(&q, "holder", None)
        .await
        .expect("first caller takes the permit");

    let qc = Arc::clone(&q);
    let waiter = tokio::spawn(async move {
        acquire_with_queue_gauge(&qc, "complete_stream/fast", None).await
    });
    wait_until(|| q.depth() >= 1, "the waiter to park").await;
    q.close_for_test();

    let err = waiter
        .await
        .expect("waiter task")
        .expect_err("closed semaphore must surface as an error");
    let msg = err.to_string();
    assert!(
        msg.contains("complete_stream/fast"),
        "error must name the phase, got: {msg}"
    );
    assert_eq!(q.depth(), 0);
}
