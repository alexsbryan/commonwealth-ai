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
//! panicking on a `None` runtime, and that a mid-turn reply which resolves
//! nothing is SAID so rather than accepted and dropped (§18.3) — with the
//! socket's claim (`?approvals=true`) and the unclaimed default named apart,
//! because they are different facts a client can act on differently.
//!
//! The last group covers SESSION CONTINUATION — `Resume` and `Redirect`,
//! added by sv-surface rung 6 C2-b — and its centre is a rule rather than a
//! route: a socket cannot name a session belonging to another conversation.
//! Redirect and resume are deliberately NOT symmetric about a session the
//! daemon no longer holds, and both halves of that are pinned, because the
//! asymmetry is the whole reason an attached surface answers a stale
//! clarification card the way an in-process one does.

// The topic parts live in sibling files: together they put this one
// over the §3.2 size ceiling. Explicit `#[path]` is load-bearing —
// for a `#[path]`-loaded module a child `mod` resolves against the
// CONTAINING directory, not a file-stem directory.
#[path = "turn_surface/corpus_scoping.rs"]
mod corpus_scoping;
#[path = "turn_surface/parked_turn.rs"]
mod parked_turn;
#[path = "turn_surface/serve_basics.rs"]
mod serve_basics;
#[path = "turn_surface/session_continuation.rs"]
mod session_continuation;
#[path = "turn_surface/socket_lifecycle.rs"]
mod socket_lifecycle;
#[path = "turn_surface/websocket_answers.rs"]
mod websocket_answers;

use crate::common::{desktop_services_with_store, TestProvider};

use std::sync::Arc;

use futures::SinkExt;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_contracts::traits::StateStore;
use sovereign_contracts::types::TurnRequest;
use sovereign_daemon::EmbeddedDaemon;

pub(crate) fn engine(dir: &std::path::Path) -> Arc<corpus_engine::CorpusEngine> {
    Arc::new(corpus_engine::CorpusEngine::new(
        dir.join("recipes"),
        dir.join("indexes"),
        Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0_f32; 4]) })),
    ))
}

/// A serving daemon over a store the test also holds, so an assertion can read
/// the rows the turn wrote.
pub(crate) fn serving_daemon(
    provider: TestProvider,
) -> (tempfile::TempDir, Arc<EmbeddedDaemon>, Arc<dyn StateStore>) {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let services =
        desktop_services_with_store(engine(tmp.path()), Arc::clone(&store), Arc::new(provider));
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );
    (tmp, daemon, store)
}

pub(crate) async fn create_conversation(base: &str) -> String {
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

/// Open the turn stream for one conversation.
pub(crate) async fn open_stream(
    addr: std::net::SocketAddr,
    conv: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    tokio_tungstenite::connect_async(format!("ws://{addr}/v1/conversations/{conv}/stream"))
        .await
        .expect("the daemon accepts a WebSocket upgrade on the turn route")
        .0
}

pub(crate) async fn send_request(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    req: TurnRequest,
) {
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&req).unwrap().into(),
    ))
    .await
    .unwrap();
}
