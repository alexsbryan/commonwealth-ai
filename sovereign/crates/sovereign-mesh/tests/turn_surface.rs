// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "treesitter")]
//! **The daemon serves a turn** — the falsifier for `quality/TOPOLOGY.md` §10
//! phase 5c.
//!
//! # The named failing input (ARCH §18.1)
//!
//! Delete the `turn_http` mount from `EmbeddedDaemon::start_daemon`, or the
//! `runtime` field from `ServingCore`, and every test below fails: the routes
//! 404 and no frame arrives. That is precisely the state the workspace was in
//! until 2026-08-25 — a daemon holding every ingredient of an answer and
//! serving none — and nothing in the build reported it, which is why this file
//! exists rather than a doc paragraph (§7.2: an assertion belongs in a test).
//!
//! # What each test proves, and what it does not
//!
//! `a_daemon_streams_a_turn_to_a_websocket_client` is the whole claim end to
//! end: a real listener, a real WebSocket, a real `TurnRequest` in and real
//! `TurnFrame`s out, terminating in `Complete`. It uses the stub provider, so
//! it proves the WIRING — that the daemon can drive `serve_turn` and get the
//! frames back onto a socket. It says nothing about answer quality, which is
//! the bench's job and deliberately not gated here.
//!
//! The three narrower tests pin the edges the end-to-end one would pass
//! through silently: that the conversation is really SEEDED (not merely
//! assigned an id), that a `MeshAdmin` daemon refuses with a reason instead of
//! panicking on a `None` runtime, and that mid-turn approvals are refused
//! loudly rather than accepted and dropped (§18.3).

mod common;
use common::{desktop_services_with_store, mesh_admin_services, spawn_router, TestProvider};

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use sovereign_contracts::types::{TurnFrame, TurnRequest};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::StateStore;
use sovereign_mesh::{turn_http::turn_router, EmbeddedDaemon};

fn engine(dir: &std::path::Path) -> Arc<corpus_engine::CorpusEngine> {
    Arc::new(corpus_engine::CorpusEngine::new(
        dir.join("recipes"),
        dir.join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ))
}

/// A serving daemon over a store the test also holds, so an assertion can read
/// the rows the turn wrote.
fn serving_daemon(
    provider: TestProvider,
) -> (
    tempfile::TempDir,
    Arc<EmbeddedDaemon>,
    Arc<dyn StateStore>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> =
        Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let services = desktop_services_with_store(
        engine(tmp.path()),
        Arc::clone(&store),
        Arc::new(provider),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );
    (tmp, daemon, store)
}

async fn create_conversation(base: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/conversations"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("daemon reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "POST /v1/conversations on a serving daemon"
    );
    resp.json::<serde_json::Value>()
        .await
        .unwrap()
        .get("id")
        .and_then(|v| v.as_str())
        .expect("create response carries an id")
        .to_string()
}

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

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream"
    ))
    .await
    .expect("the daemon accepts a WebSocket upgrade on the turn route");

    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Message {
            content: "who answered?".to_string(),
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
            let frame: TurnFrame =
                serde_json::from_str(&t).expect("every frame the daemon sends parses as a TurnFrame");
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

/// Approvals are on the wire and have no daemon-side owner yet. Accepting one
/// and doing nothing is the failure this pins: a client that submitted an
/// approval and received no frame cannot tell "granted" from "never arrived".
#[tokio::test]
async fn a_mid_turn_approval_is_refused_loudly() {
    let (_tmp, daemon, _store) = serving_daemon(TestProvider::new());
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream"
    ))
    .await
    .unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Approve {
            task_id: "t".to_string(),
            step_id: 0,
            approved: true,
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
    .expect("a refusal arrives rather than silence")
    .expect("the refusal is a well-formed TurnFrame");

    match frame {
        TurnFrame::StreamError { message, .. } => assert!(
            message.contains("approval"),
            "the refusal says what it refused; got {message:?}"
        ),
        other => panic!("expected a StreamError refusal, got {other:?}"),
    }
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

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream"
    ))
    .await
    .unwrap();
    let msg = |text: &str| {
        tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&TurnRequest::Message {
                content: text.to_string(),
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
