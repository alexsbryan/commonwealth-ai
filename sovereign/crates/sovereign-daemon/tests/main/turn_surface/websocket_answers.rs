// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! Mid-turn answers over the wire: a claimed and an unclaimed socket
//! are told different facts about a reply that resolves nothing, and
//! the receive loop keeps reading while a turn runs.

use crate::common::{spawn_router, TestProvider};

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use sovereign_contracts::types::{
    ResolveOutcome, TurnAnswer, TurnFrame, TurnMode, TurnNotice, TurnRequest,
};
use sovereign_daemon::turn_http::turn_router;

use super::{create_conversation, serving_daemon};

/// Send an `Answer` and read the daemon's reply to it.
/// Answer a question nothing is parked under and hand back what the daemon
/// said about it.
///
/// A `ResolveAck` and not a `StreamError` since sv-surface RB4: a reply that
/// resolved nothing is a recoverable fact about ONE question, and riding the
/// terminal frame meant a client that double-clicked approve threw away a
/// turn that was running perfectly well.
async fn answer_and_read_ack(
    addr: std::net::SocketAddr,
    conv: &str,
    claim: bool,
) -> ResolveOutcome {
    let query = if claim { "?approvals=true" } else { "" };
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream{query}"
    ))
    .await
    .expect("the daemon accepts the upgrade");
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: "step:0".to_string(),
            answer: TurnAnswer::Approved(true),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    let frame = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                return serde_json::from_str::<TurnFrame>(&t).ok();
            }
        }
        None
    })
    .await
    .expect("an answer arrives rather than silence")
    .expect("the answer is a well-formed TurnFrame");

    match frame {
        TurnFrame::Notice {
            notice: TurnNotice::ResolveAck { outcome, .. },
        } => outcome,
        TurnFrame::StreamError { message, .. } => panic!(
            "a refused answer must not ride the TERMINAL frame — both clients end the turn \
             on it (sv-surface RB4); got {message:?}"
        ),
        other => panic!("expected a ResolveAck notice, got {other:?}"),
    }
}

/// A socket that claimed nothing is told SO, by name — not told "nothing is
/// pending", which is a different fact, and not told nothing at all.
///
/// Accepting the reply and doing nothing is the failure this pins: a client
/// that submitted an approval and received no frame cannot tell "granted" from
/// "never arrived" (§18.3). Before rung 6 C1 every reply landed here; now this
/// is the branch for a client that did not ask to answer.
#[tokio::test]
async fn an_unclaimed_socket_is_told_its_reply_resolved_nothing() {
    let (_tmp, daemon, _store) = serving_daemon(TestProvider::new());
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    assert_eq!(
        answer_and_read_ack(addr, &conv, false).await,
        ResolveOutcome::Unclaimed,
        "the outcome names the CLAIM the client is missing — not `NoSuchPending`, \
         which is a socket that could have answered and had nothing parked"
    );
}

/// The claim is ACCEPTED and its resolve path runs: a claimed socket whose
/// turn has nothing parked is told that, in different words from the socket
/// that never claimed. Both branches exist, and neither is silence.
#[tokio::test]
async fn a_claimed_socket_is_told_when_nothing_is_pending() {
    let (_tmp, daemon, _store) = serving_daemon(TestProvider::new());
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    assert_eq!(
        answer_and_read_ack(addr, &conv, true).await,
        ResolveOutcome::NoSuchPending,
        "a claimed socket is answered about its QUESTIONS, not about its claim"
    );
}

/// **The receive loop keeps running while a turn does.**
///
/// The turn is spawned rather than awaited inline, and this is the property
/// that buys: a socket mid-turn still reads. It is the same property that
/// makes the connection answer PINGS, which is how the defect was found —
/// the first real turn against a deployed daemon died at 20s with
/// `keepalive ping timeout` while the daemon's own log showed that turn's
/// retrieval completing normally. A grounded turn over a real corpus runs
/// minutes (235s, measured); every standards-compliant client with keepalive
/// would have dropped before the answer arrived.
///
/// It is asserted through the second-turn refusal rather than through a ping,
/// because a ping assertion is a race and this one is not: with the turn
/// awaited inline the second message CANNOT be read until the first turn ends,
/// so it would start a second turn and stream tokens. Here it is read
/// immediately and refused by name.
#[tokio::test]
async fn a_socket_still_reads_while_its_turn_is_running() {
    let provider = TestProvider::new()
        .with_stream_chunks(vec!["slow".to_string(), " answer".to_string()])
        // Two chunks at 400ms leaves ~800ms of turn to be inside of.
        .with_stream_delay(std::time::Duration::from_millis(400));
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/v1/conversations/{conv}/stream"))
            .await
            .unwrap();
    let msg = |text: &str| {
        tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&TurnRequest::Message {
                content: text.to_string(),
                mode: TurnMode::Grounded,
                intent: None,
            })
            .unwrap()
            .into(),
        )
    };
    ws.send(msg("first")).await.unwrap();
    ws.send(msg("second while the first runs")).await.unwrap();

    let refusal = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while let Some(Ok(m)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = m else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).unwrap();
            match frame {
                TurnFrame::StreamError { message, .. } => return Some(message),
                // The first turn finished before the second was even read —
                // which is exactly the inline behaviour this test forbids.
                TurnFrame::Complete { .. } => return None,
                _ => continue,
            }
        }
        None
    })
    .await
    .expect("the socket answered within 30s");

    let message = refusal.expect(
        "the second turn was not refused — the receive loop is not reading \
         during a turn, so the socket also cannot answer a ping and every \
         keepalive client drops mid-answer",
    );
    assert!(
        message.contains("already in flight"),
        "the refusal names why; got {message:?}"
    );
}
