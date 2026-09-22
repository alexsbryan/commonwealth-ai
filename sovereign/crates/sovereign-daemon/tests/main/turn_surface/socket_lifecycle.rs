// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! The socket's own life (sv-surface RB1, RB2): a settled turn is
//! said and then the socket closes, a settled socket serves a second
//! turn, and a Cancel unparks the question its turn is stopped on.

use crate::common::{spawn_router, TestProvider};

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use sovereign_contracts::types::{
    ResolveOutcome, TurnAnswer, TurnFrame, TurnMode, TurnNotice, TurnRequest,
};
use sovereign_daemon::turn_http::{turn_router_with, SocketTimers};

use super::parked_turn::{turn_parked_on_a_question, ParkedTurn};
use super::{create_conversation, open_stream, send_request, serving_daemon};

// ─── The socket's own life: settling, closing, reuse (sv-surface RB1) ─────
//
// `Complete` is the terminal frame of the TURN, and until RB1 nothing was
// the terminal frame of the SOCKET: `handle_ws` looped forever, the client's
// drain ended only on a host close that never came, and every turn leaked a
// WebSocket, a writer task and an approval channel on both ends.
//
// The windows come from `SocketTimers` rather than the production constants
// because the property is only observable by living through one — see
// `turn_router_with`.

/// Milliseconds, so a test can watch a real socket close.
fn quick_timers() -> SocketTimers {
    SocketTimers {
        idle_after_settled: std::time::Duration::from_millis(300),
        post_complete_settle_max: std::time::Duration::from_secs(5),
    }
}

/// Read frames until one satisfies `done`, and hand back everything read.
async fn read_until(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    what: &str,
    done: impl Fn(&TurnFrame) -> bool,
) -> Vec<TurnFrame> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut seen = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            let stop = done(&frame);
            seen.push(frame);
            if stop {
                return seen;
            }
        }
        panic!("the socket ended before {what}; read {seen:?}");
    })
    .await
    .unwrap_or_else(|_| panic!("waited 30s for {what} and the daemon said nothing"))
}

/// **The turn settles, the daemon says so, and then it closes the socket.**
///
/// Three claims, and each one failed before RB1: no `TurnSettled` was ever
/// emitted (there was no such frame), the receive loop had no exit, and the
/// writer task ran until the process did.
///
/// The settled notice carries the turn's message id — stamped by the writer,
/// which is the only thing that has SEEN the `Complete` go out.
#[tokio::test]
async fn a_settled_turn_is_said_and_the_daemon_closes_the_socket_nobody_reused() {
    let provider = TestProvider::new().with_stream_chunks(vec!["done".to_string()]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router_with(Arc::clone(&daemon), quick_timers())).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Message {
            content: "one question".to_string(),
            mode: TurnMode::Grounded,
            intent: None,
        },
    )
    .await;

    let frames = read_until(&mut ws, "the settled notice", |f| {
        matches!(
            f,
            TurnFrame::Notice {
                notice: TurnNotice::TurnSettled { .. }
            }
        )
    })
    .await;

    let completed = frames
        .iter()
        .find_map(|f| match f {
            TurnFrame::Complete { message_id, .. } => Some(message_id.clone()),
            _ => None,
        })
        .expect("the turn completed before it settled");
    let TurnFrame::Notice {
        notice: TurnNotice::TurnSettled { message_id },
    } = frames.last().expect("read_until stopped on it")
    else {
        unreachable!("read_until stopped on the settled notice")
    };
    assert_eq!(
        message_id, &completed,
        "the settled notice names the turn it closes out"
    );

    // And the socket ENDS. Bounded by well over the idle window, because the
    // failure being pinned is "never" — a test that waited a moment and gave
    // up would pass against the leak it exists to catch.
    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let tokio_tungstenite::tungstenite::Message::Close(_) = msg {
                return true;
            }
        }
        // The stream ending is the same fact.
        true
    })
    .await;
    assert!(
        closed.is_ok(),
        "the daemon never closed a socket nobody was using — one leaked WebSocket, writer \
         task and approval channel per turn (sv-surface RB1)"
    );
}

/// The settle does not end the CONVERSATION: a second turn on the same
/// socket, sent inside the idle window, runs normally.
///
/// This is the constraint the close had to respect — `handle_ws` serves
/// sequential turns, and a socket that hung up at `Complete` would have
/// broken every client that asks a follow-up.
#[tokio::test]
async fn a_second_turn_on_a_settled_socket_runs_normally() {
    let provider = TestProvider::new().with_stream_chunks(vec!["again".to_string()]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router_with(
        Arc::clone(&daemon),
        SocketTimers {
            // Long enough to send a follow-up by hand, short enough that the
            // close still happens in this test's lifetime.
            idle_after_settled: std::time::Duration::from_secs(5),
            post_complete_settle_max: std::time::Duration::from_secs(5),
        },
    ))
    .await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let mut ws = open_stream(addr, &conv).await;
    for turn in ["first", "second"] {
        send_request(
            &mut ws,
            TurnRequest::Message {
                content: turn.to_string(),
                mode: TurnMode::Grounded,
                intent: None,
            },
        )
        .await;
        let frames = read_until(&mut ws, "the settled notice", |f| {
            matches!(
                f,
                TurnFrame::Notice {
                    notice: TurnNotice::TurnSettled { .. }
                }
            )
        })
        .await;
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, TurnFrame::Complete { .. })),
            "turn {turn} completed on the reused socket; got {frames:?}"
        );
    }
}

/// **Stop, on an open consent card.** sv-surface RB2.
///
/// `Cancel` tripped the turn's cancellation tokens and nothing else, and a
/// turn parked in `Parked::answered` is blocked on a `oneshot` that no token
/// reaches: the executor never returned, so NO terminal frame was ever
/// emitted and the user's stop button did nothing at all. The desk has to be
/// resolved too, by the per-kind hangup policy (E2).
///
/// The assertion that went red before the fix is the terminal read below —
/// it waits 30s and panics with "the daemon said nothing", which is exactly
/// what the client saw.
#[tokio::test]
async fn a_cancel_unparks_the_question_its_turn_is_stopped_on() {
    let ParkedTurn {
        _tmp,
        _daemon,
        mut ws,
        prompt_id,
        ..
    } = turn_parked_on_a_question(quick_timers()).await;
    assert!(prompt_id.ends_with(":input"), "parked on the ask_user step");

    send_request(&mut ws, TurnRequest::Cancel {}).await;

    let frames = read_until(&mut ws, "the cancelled turn's terminal frame", |f| {
        matches!(
            f,
            TurnFrame::Complete { .. } | TurnFrame::StreamError { .. }
        )
    })
    .await;
    match frames.last().expect("read_until stopped on it") {
        // KNOWN GAP, measured here rather than assumed: the terminal frame
        // arrives (which is the whole of RB2) but on the AGENTIC path it
        // carries `provenance.finish_reason: None` — watched, 2026-09-10.
        // The `"cancelled"` stamp is the STREAMING path's
        // (handlers/generative.rs), and the non-streaming turn persists no
        // finish reason at all, so a client cannot yet tell a stopped
        // agentic turn from a finished one. Closing that is a
        // sovereign-core change (the non-streaming turn's provenance), not
        // one this socket may make: rewriting the projection here would
        // make the frame disagree with the row it was projected from
        // (§18.3).
        TurnFrame::Complete { .. } => {}
        TurnFrame::StreamError { message, .. } => panic!(
            "a stop is not a failure — the turn ends the way a cancelled turn always ended, \
             with its terminal Complete; got {message:?}"
        ),
        other => unreachable!("read_until stopped on {other:?}"),
    }

    // And the question is gone with it: an answer arriving after the stop
    // resolves nothing rather than restarting what the user stopped.
    send_request(
        &mut ws,
        TurnRequest::Answer {
            id: prompt_id,
            answer: TurnAnswer::Text("too late".into()),
        },
    )
    .await;
    let acked = read_until(&mut ws, "the ack for the late answer", |f| {
        matches!(
            f,
            TurnFrame::Notice {
                notice: TurnNotice::ResolveAck { .. }
            }
        )
    })
    .await;
    let TurnFrame::Notice {
        notice: TurnNotice::ResolveAck { outcome, .. },
    } = acked.last().unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        *outcome,
        ResolveOutcome::NoSuchPending,
        "the cancelled question is not still parked"
    );
}
