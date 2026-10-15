//! The half-open SSE client: reads nothing, drops nothing.
//!
//! A `blocking_send` into a full channel parks forever against such
//! a consumer, and the generation loop's deadline check — which sits
//! at the TOP of the loop — never gets to run. The slot's context
//! mutex stays held indefinitely, and on a daemon serving roughly
//! one concurrent turn that is the whole node.
//!
//! RETARGETED by noun-convergence rung nc-17. These tests used to
//! watch `send_stream_piece`, the legacy `Result<String>` sender —
//! which by then nothing streamed on, because `complete_stream` had
//! become a copy of the typed routing path and the typed path used a
//! bare `blocking_send`. The hardening was guarding the dead half.
//! They now watch [`StreamSink`], which every streaming completion
//! goes through.
use super::{StreamFrame, StreamSend, StreamSink};
use std::time::{Duration, Instant};

/// RED-FIRST (order mesh-scale-t0, item 5). Runs the send on its own
/// thread and refuses to wait forever for it: on the pre-fix
/// `blocking_send` the thread never reports back, `recv_timeout`
/// expires, and the assertion fails naming the pin. (The test itself
/// still terminates — a hanging test proves nothing and blocks the
/// suite, which is why this is structured as a watched thread rather
/// than a direct call.)
#[test]
fn a_consumer_that_stops_reading_frees_the_slot_within_the_deadline() {
    // Capacity 1, filled and never read — and the receiver is HELD,
    // so the channel is full rather than closed. A closed channel
    // would return `ReceiverGone` and pass for the wrong reason.
    let (tx, _rx_held_open) = tokio::sync::mpsc::channel::<StreamFrame>(1);
    tx.try_send(StreamFrame::Token("first".into()))
        .expect("prime the buffer");

    let budget = Duration::from_millis(300);
    let deadline = Instant::now() + budget;

    let (report_tx, report_rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let sink = StreamSink::with_deadline(&tx, deadline);
        let outcome = sink.send_piece("second");
        let _ = report_tx.send((outcome, started.elapsed()));
    });

    // Generous slack: we are asserting BOUNDED, not fast.
    let (outcome, elapsed) = report_rx
        .recv_timeout(budget + Duration::from_secs(3))
        .expect(
            "the send never returned — a half-open consumer is pinning the slot \
             indefinitely, and the wall-clock deadline cannot fire because the \
             generation loop is parked inside the send",
        );

    assert_eq!(
        outcome,
        StreamSend::DeadlineExceeded,
        "a full channel at the deadline must report the consumer, not the receiver"
    );
    assert!(
        elapsed >= budget,
        "must not give up EARLY either — a healthy consumer that is briefly \
         slow has the whole budget to catch up (elapsed {elapsed:?})"
    );
    assert!(
        elapsed < budget + Duration::from_secs(1),
        "released far too late: {elapsed:?} against a {budget:?} budget"
    );
}

/// A consumer that IS reading must be unaffected — the poll loop
/// must not turn a healthy stream into a stuttering one.
#[test]
fn a_reading_consumer_is_delivered_immediately() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamFrame>(4);
    let sink = StreamSink::with_deadline(&tx, Instant::now() + Duration::from_secs(30));
    let started = Instant::now();
    assert_eq!(sink.send_piece("tok"), StreamSend::Sent);
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "an unblocked send must not pay the poll interval"
    );
    assert!(matches!(rx.try_recv(), Ok(StreamFrame::Token(s)) if s == "tok"));
}

/// A dropped receiver stays a clean cancel, distinct from a stalled
/// consumer — collapsing the two would make every cancelled stream
/// look like a client fault.
#[test]
fn a_dropped_receiver_is_still_reported_as_cancellation() {
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamFrame>(1);
    drop(rx);
    let sink = StreamSink::with_deadline(&tx, Instant::now() + Duration::from_secs(30));
    assert_eq!(sink.send_piece("tok"), StreamSend::ReceiverGone);
}

/// The terminal frame must never be sent after a non-`Sent` piece —
/// it would block on the same full channel the piece just gave up
/// on. Structural in both loops; asserted here so a future edit that
/// drops the guard has something to fail against.
#[test]
fn a_stalled_send_leaves_room_for_no_terminal_frame() {
    let (tx, _rx_held_open) = tokio::sync::mpsc::channel::<StreamFrame>(1);
    tx.try_send(StreamFrame::Token("first".into()))
        .expect("prime the buffer");
    let sink = StreamSink::with_deadline(&tx, Instant::now() + Duration::from_millis(50));
    assert_eq!(sink.send_piece("second"), StreamSend::DeadlineExceeded);
    assert!(
        tx.try_send(StreamFrame::Finish {
            reason: super::FinishReason::Stop,
            usage: None,
        })
        .is_err(),
        "the channel is still full — a Finish frame here would park the \
         decode loop exactly as the token send just did"
    );
}
