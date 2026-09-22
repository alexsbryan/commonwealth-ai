// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! The wiring core: a real turn over a real WebSocket ends in
//! `Complete`, the create route really seeds its row, and a
//! `MeshAdmin` daemon refuses with a reason instead of panicking.

use crate::common::{mesh_admin_services, spawn_router, TestProvider};

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::{TurnFrame, TurnMode, TurnRequest};
use sovereign_daemon::{turn_http::turn_router, EmbeddedDaemon};

use super::{create_conversation, serving_daemon};

/// THE test. A turn goes in over a WebSocket and frames come back.
#[tokio::test]
async fn a_daemon_streams_a_turn_to_a_websocket_client() {
    let provider = TestProvider::new().with_stream_chunks(vec![
        "The ".to_string(),
        "daemon ".to_string(),
        "answered.".to_string(),
    ]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let base = format!("http://{addr}");
    let conv = create_conversation(&base).await;

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/v1/conversations/{conv}/stream"))
            .await
            .expect("the daemon accepts a WebSocket upgrade on the turn route");

    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Message {
            content: "who answered?".to_string(),
            mode: TurnMode::Grounded,
            intent: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Collect until the terminal frame. A bounded wait, because a hang here is
    // a real failure mode (the turn never completing) and a test that hangs
    // reports nothing.
    let frames = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut frames: Vec<TurnFrame> = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t)
                .expect("every frame the daemon sends parses as a TurnFrame");
            let terminal = matches!(
                frame,
                TurnFrame::Complete { .. } | TurnFrame::StreamError { .. }
            );
            frames.push(frame);
            if terminal {
                break;
            }
        }
        frames
    })
    .await
    .expect("the turn produced a terminal frame within 60s");

    assert!(
        !frames.is_empty(),
        "the daemon accepted the turn and sent nothing back"
    );
    match frames.last().expect("checked non-empty") {
        TurnFrame::Complete { message_id, .. } => {
            assert!(
                !message_id.is_empty(),
                "a `Complete` names the message the turn produced"
            );
        }
        other => panic!(
            "the turn ended on {other:?} rather than `Complete` — the daemon \
             reached the turn service but could not finish a turn"
        ),
    }
}

/// The create route SEEDS the row. Handing back an id without writing one
/// would pass a status-code assertion and fail the first turn.
#[tokio::test]
async fn create_conversation_seeds_the_row_it_names() {
    let (_tmp, daemon, store) = serving_daemon(TestProvider::new());
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;
    store
        .get_conversation(&conv)
        .await
        .expect("the conversation the create route named exists in the daemon's own store");
}

/// A `MeshAdmin` one-shot has no serving role and therefore no `Runtime`. It
/// must say so, not panic and not 404 — the difference between "this daemon
/// does not serve turns" and "this build has no turn surface" is one a client
/// has to be able to tell (§18.3).
#[tokio::test]
async fn a_mesh_admin_daemon_refuses_with_a_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    let addr = spawn_router(turn_router(daemon)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("server reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "a mesh-admin daemon must refuse the turn surface, not serve it"
    );
    let body = resp.json::<serde_json::Value>().await.unwrap();
    assert!(
        body.get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|e| e.contains("mesh-admin")),
        "the refusal names which shape refused; got {body}"
    );
}
