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

use crate::common;
use crate::common::{desktop_services_with_store, mesh_admin_services, spawn_router, TestProvider};

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use sovereign_contracts::types::{
    ResolveOutcome, TurnAnswer, TurnFrame, TurnMode, TurnNotice, TurnRequest,
};
use sovereign_core::setup_config::SetupConfig;
use sovereign_core::traits::StateStore;
use sovereign_mesh::{
    turn_http::{turn_router, turn_router_with, SocketTimers},
    EmbeddedDaemon,
};

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

/// Install a corpus at `<indexes>/<id>` with one chunk, marked complete so
/// the engine's `installed_indexes()` reports it — the same fixture
/// `knowledge_served_e2e` uses.
async fn install_corpus(indexes_dir: &std::path::Path, id: &str) {
    use corpus_engine::index::{CorpusIndex, InsertChunk};
    let index = CorpusIndex::create(
        &indexes_dir.join(id),
        id,
        id,
        "qwen3-embedding-0.6b",
        4,
        /* mesh_sharing */ true,
        "CC-BY-NC",
    )
    .await
    .unwrap();
    index
        .insert_batch(&[(
            InsertChunk {
                content: "one chunk".into(),
                title: Some(id.into()),
                url: None,
                metadata: None,
                content_hash: None,
                source_doc_id: Some(id.into()),
                source_file: None,
                code: Default::default(),
                unit_id: None,
            },
            vec![0.0_f32; 4],
        )])
        .await
        .unwrap();
    index.mark_ingestion_complete().unwrap();
}

/// `enabled_corpora` on the create body — the wire form `svrn chat ask
/// --corpus` uses — lands on the row BEFORE the first turn, so retrieval's
/// allow-list filter sees it. Until 2026-09-01 the field had no wire form
/// at all and a daemon-served turn could not be scoped.
#[tokio::test]
async fn create_conversation_seeds_the_corpus_allow_list_it_was_given() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    install_corpus(&tmp.path().join("indexes"), "gutenberg").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body = resp.json::<serde_json::Value>().await.unwrap();
    let conv = body["id"].as_str().unwrap().to_string();
    assert_eq!(
        body["enabled_corpora"],
        serde_json::json!(["sep"]),
        "the create response echoes the seeded allow-list — the client's only \
         way to tell a daemon that scoped from one that dropped the key"
    );

    let row = store.get_conversation(&conv).await.unwrap();
    assert_eq!(
        row.enabled_corpora.as_deref(),
        Some(&["sep".to_string()][..]),
        "the allow-list must be on the row the first turn will read"
    );
}

/// The named failing input (§18.3): an id the daemon has not installed.
/// Retrieval would silently intersect it away and search NOTHING while the
/// answer read as "the corpus does not cover this". The route refuses with
/// a 400 that names the offender and lists what IS installed — the remedy,
/// not just the complaint — and seeds no row.
#[tokio::test]
async fn create_conversation_refuses_an_unknown_corpus_and_lists_the_installed_ones() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep", "nope"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "an unknown corpus id is the caller's mistake, not a daemon fault"
    );
    let body = resp.json::<serde_json::Value>().await.unwrap();
    let err = body["error"]
        .as_str()
        .expect("the refusal carries a reason");
    assert!(err.contains("unknown corpus id: nope"), "{err}");
    assert!(err.contains("installed: sep"), "{err}");
    assert!(
        store.list_conversations(10, 0).await.unwrap().is_empty(),
        "a refused create must not leave a half-seeded row behind"
    );
}

/// The allow-list has a SECOND writer — the desktop's corpus-chip strip,
/// which toggles it long after create — and until sv-surface it had no wire
/// form at all. The desktop wrote the column through its own store handle,
/// so in attach mode every chip the user touched landed on a row the turn
/// never reads: a filter that looked like it worked and did nothing.
///
/// PUT replaces the list wholesale, which is what a chip strip does.
#[tokio::test]
async fn enabled_corpora_put_writes_the_row_the_turn_reads() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    install_corpus(&tmp.path().join("indexes"), "gutenberg").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let body = http
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let conv = body["id"].as_str().unwrap().to_string();

    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": ["gutenberg"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        store
            .get_conversation(&conv)
            .await
            .unwrap()
            .enabled_corpora
            .as_deref(),
        Some(&["gutenberg".to_string()][..]),
        "the PUT must REPLACE the seeded list on the row the turn reads, \
         not merge with it"
    );

    // `null` clears, and clearing means "search every installed corpus" —
    // the column's contract, so the route must not confuse it with the
    // empty list below.
    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": serde_json::Value::Null }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        store.get_conversation(&conv).await.unwrap().enabled_corpora,
        None,
        "null clears the column — not 'search nothing'"
    );
}

/// The named failing inputs (§18.1), both of which the desktop's local write
/// accepted: an id nothing has installed, and the empty list. Retrieval
/// intersects the allow-list SILENTLY, so either one produced an empty
/// fan-out and an answer that read as "the corpus does not cover this".
/// The route refuses both, names the remedy, and leaves the row as it was.
#[tokio::test]
async fn enabled_corpora_put_refuses_what_would_search_nothing() {
    let (tmp, daemon, store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let http = reqwest::Client::new();

    let body = http
        .post(format!("http://{addr}/v1/conversations"))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let conv = body["id"].as_str().unwrap().to_string();

    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": ["nope"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err = resp.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .expect("the refusal carries a reason")
        .to_string();
    assert!(err.contains("unknown corpus id: nope"), "{err}");
    assert!(err.contains("installed: sep"), "{err}");

    let resp = http
        .put(format!(
            "http://{addr}/v1/conversations/{conv}/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let err = resp.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(err.contains("would search nothing"), "{err}");

    assert_eq!(
        store
            .get_conversation(&conv)
            .await
            .unwrap()
            .enabled_corpora
            .as_deref(),
        Some(&["sep".to_string()][..]),
        "a refused PUT must leave the row exactly as it found it"
    );
}

/// A conversation the daemon does not hold is a 404, not a silent success.
/// The store's write reports `NotFound` and the route must carry that
/// through rather than collapse it into 204 (§18.3) — otherwise an attached
/// surface pointed at the wrong daemon toggles chips forever and is told
/// each one landed.
#[tokio::test]
async fn enabled_corpora_put_on_a_missing_conversation_is_404() {
    let (tmp, daemon, _store) = serving_daemon(TestProvider::new());
    std::fs::create_dir_all(tmp.path().join("indexes")).unwrap();
    install_corpus(&tmp.path().join("indexes"), "sep").await;
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;

    let resp = reqwest::Client::new()
        .put(format!(
            "http://{addr}/v1/conversations/ghost/enabled-corpora"
        ))
        .json(&serde_json::json!({ "enabled_corpora": ["sep"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
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

// ─── Session continuation over the wire (sv-surface rung 6, C2-b) ─────────
//
// `redirect_turn` and `resume_session` were the two turn commands with no
// wire form at all, so an ATTACHED desktop could ask a question but could not
// answer its own clarification card — it had to keep a `Runtime` behind the
// card, which is the construction rung 6 deletes.
//
// The named failing input for the pair (§18.1): delete either arm from
// `handle_ws`'s match and the request parses, is dropped, and the socket goes
// quiet — the exact silence §18.3 forbids. Every test below reads a frame
// back, so silence fails them all.

/// Open the turn stream for one conversation.
async fn open_stream(
    addr: std::net::SocketAddr,
    conv: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    tokio_tungstenite::connect_async(format!("ws://{addr}/v1/conversations/{conv}/stream"))
        .await
        .expect("the daemon accepts a WebSocket upgrade on the turn route")
        .0
}

async fn send_request(
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

/// Read frames until the turn ends, and hand back the terminal one.
///
/// Bounded, because the failure this file is most exposed to is a socket that
/// answers NOTHING, and a test that hangs reports nothing either.
async fn read_until_terminal(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> TurnFrame {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame =
                serde_json::from_str(&t).expect("every frame the daemon sends parses");
            if matches!(
                frame,
                TurnFrame::Complete { .. } | TurnFrame::StreamError { .. }
            ) {
                return frame;
            }
        }
        panic!("the socket closed without a terminal frame");
    })
    .await
    .expect("the daemon answered within 60s rather than going quiet")
}

fn refusal(frame: TurnFrame) -> String {
    match frame {
        TurnFrame::StreamError { message, .. } => message,
        other => panic!("expected a named refusal, got {other:?}"),
    }
}

/// Run one turn to completion on `conv` and hand back the `QuerySession` id it
/// left behind.
///
/// A REAL session rather than a hand-assembled one: redirect resolves the
/// turn's message and its conversation off the session, so a fixture that
/// built a `QuerySession` by hand would assert against its own construction
/// instead of against what a turn actually leaves.
async fn turn_leaving_a_session(
    addr: std::net::SocketAddr,
    daemon: &EmbeddedDaemon,
    conv: &str,
) -> String {
    let mut ws = open_stream(addr, conv).await;
    send_request(
        &mut ws,
        TurnRequest::Message {
            content: "which one did you mean?".to_string(),
            mode: TurnMode::Grounded,
            intent: None,
        },
    )
    .await;
    match read_until_terminal(&mut ws).await {
        TurnFrame::Complete { .. } => {}
        other => panic!("the seeding turn did not complete: {other:?}"),
    }
    daemon
        .runtime()
        .expect("a serving daemon holds a runtime")
        .sessions
        .latest_for_conversation(conv)
        .expect(
            "a completed turn leaves its QuerySession live for ~30s — redirect and resume \
             are named against exactly that",
        )
        .id
}

/// A redirect names a session the daemon does not hold. Refused BY NAME —
/// not with silence, and not by re-answering something else it knows about,
/// which is the substitution §18.3 forbids.
///
/// `Gone` is ordinary here rather than exotic: `SESSION_RETENTION` is 30s and
/// a daemon restart drops every session, while the card that names one sits
/// in a transcript indefinitely.
#[tokio::test]
async fn a_redirect_naming_an_unknown_session_is_refused_by_name() {
    let (_tmp, daemon, _store) = serving_daemon(TestProvider::new());
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Redirect {
            session_id: "no-such-session".to_string(),
            intent_hint: "DeepQuery".to_string(),
        },
    )
    .await;

    let message = refusal(read_until_terminal(&mut ws).await);
    assert!(
        message.contains("no-such-session") && message.contains("no live session"),
        "the refusal names the session it could not find; got {message:?}"
    );
}

/// **One socket cannot redirect another conversation's session.**
///
/// The hazard is specific and it is not hypothetical: `redirect_turn_stream`
/// resolves the replacement turn's message AND its conversation off the
/// session, so without this guard a client holding conversation A's socket
/// could name conversation B's session and have the turn RUN in B while its
/// tokens streamed to A. Same shape as C1's rule that one socket cannot answer
/// another's approval.
#[tokio::test]
async fn a_redirect_cannot_reach_a_session_on_another_conversation() {
    let provider = TestProvider::new().with_stream_chunks(vec!["answered".to_string()]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let base = format!("http://{addr}");
    let theirs = create_conversation(&base).await;
    let mine = create_conversation(&base).await;

    let their_session = turn_leaving_a_session(addr, &daemon, &theirs).await;

    let mut ws = open_stream(addr, &mine).await;
    send_request(
        &mut ws,
        TurnRequest::Redirect {
            session_id: their_session.clone(),
            intent_hint: "DeepQuery".to_string(),
        },
    )
    .await;

    let message = refusal(read_until_terminal(&mut ws).await);
    assert!(
        message.contains(&their_session) && message.contains("another conversation"),
        "the refusal names the session and says why it is not this socket's; got {message:?}"
    );
    assert!(
        message.contains(&mine) && !message.contains(&theirs),
        "the refusal names THIS socket's conversation and does not hand back the owning \
         one — a refusal is not a lookup service; got {message:?}"
    );
}

/// Same rule for resume. It cannot run a turn elsewhere the way redirect can,
/// but it would write "this continues session X" into THIS conversation's
/// routing metadata, and the claim would be false.
#[tokio::test]
async fn a_resume_cannot_reach_a_session_on_another_conversation() {
    let provider = TestProvider::new().with_stream_chunks(vec!["answered".to_string()]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let base = format!("http://{addr}");
    let theirs = create_conversation(&base).await;
    let mine = create_conversation(&base).await;

    let their_session = turn_leaving_a_session(addr, &daemon, &theirs).await;

    let mut ws = open_stream(addr, &mine).await;
    send_request(
        &mut ws,
        TurnRequest::Resume {
            content: "the second one".to_string(),
            session_id: their_session.clone(),
            intent_hint: "DeepQuery".to_string(),
        },
    )
    .await;

    let message = refusal(read_until_terminal(&mut ws).await);
    assert!(
        message.contains(&their_session) && message.contains("another conversation"),
        "the refusal names the session it would have cited; got {message:?}"
    );
}

/// **A resume whose session has been forgotten still ANSWERS.**
///
/// The deliberate asymmetry with redirect, pinned so a later tidy-up does not
/// "fix" it into symmetry. Resume reads nothing out of the session — the id
/// only appears in the synthetic classification's rationale — while
/// `SESSION_RETENTION` is 30s and a daemon restart drops all of them. The
/// in-process command answers this click today; an attached surface that
/// refused it would have traded the divergence rung 6 is closing for a new
/// one, and the desktop's ClarificationCard path has no fallback to catch it.
#[tokio::test]
async fn a_resume_whose_session_expired_still_answers() {
    let provider =
        TestProvider::new().with_stream_chunks(vec!["still ".to_string(), "answered".to_string()]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Resume {
            content: "the second one".to_string(),
            // Never existed — indistinguishable from swept, which is the point.
            session_id: "long-since-swept".to_string(),
            intent_hint: "DeepQuery".to_string(),
        },
    )
    .await;

    match read_until_terminal(&mut ws).await {
        TurnFrame::Complete { message_id, .. } => assert!(
            !message_id.is_empty(),
            "a resumed turn completes with the message it produced"
        ),
        TurnFrame::StreamError { message, .. } => panic!(
            "an expired session must not fail the click — the in-process command answers it, \
             and refusing here is a NEW surface divergence; got {message:?}"
        ),
        other => panic!("unreachable terminal {other:?}"),
    }
}

/// The happy path, end to end: a turn leaves a session, the socket redirects
/// it, and a second answer comes back through the ONE drain.
///
/// Proves the wiring the two refusal tests cannot: that the arm reaches
/// `redirect_turn_stream`, that `drive_stream_handle` drains the handle it
/// returns, and that the turn terminates in `Complete` rather than in the
/// acquire's error.
#[tokio::test]
async fn a_socket_redirects_its_own_session_and_gets_a_second_answer() {
    let provider = TestProvider::new().with_stream_chunks(vec!["redirected".to_string()]);
    let (_tmp, daemon, _store) = serving_daemon(provider);
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let session = turn_leaving_a_session(addr, &daemon, &conv).await;

    let mut ws = open_stream(addr, &conv).await;
    send_request(
        &mut ws,
        TurnRequest::Redirect {
            session_id: session,
            intent_hint: "DeepQuery".to_string(),
        },
    )
    .await;

    match read_until_terminal(&mut ws).await {
        TurnFrame::Complete { message_id, .. } => assert!(
            !message_id.is_empty(),
            "the redirected turn names the message it produced"
        ),
        TurnFrame::StreamError { message, .. } => {
            panic!("the redirect did not reach a turn: {message:?}")
        }
        other => panic!("unreachable terminal {other:?}"),
    }
}

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

// ─── A turn that PARKS on a question (sv-surface R3; RB2's fixture too) ───
//
// Hoisted out of the R3 test by RB2, which needs the same parked turn to
// CANCEL: one planner and one setup, not two that drift (ARCH §10.6).

use sovereign_contracts::types::{ConversationContext, Plan, Step, StepKind, ToolDescriptor};

struct AskPlanner;
#[async_trait::async_trait]
impl sovereign_core::traits::Planner for AskPlanner {
    async fn plan(
        &self,
        goal: &str,
        _context: &ConversationContext,
        _tools: &[ToolDescriptor],
    ) -> sovereign_core::error::Result<Plan> {
        Ok(Plan {
            id: "plan-ask".into(),
            goal: goal.to_string(),
            steps: vec![Step {
                id: 0,
                description: "Ask the user which corpus".into(),
                kind: StepKind::UserInput {
                    question: "Which corpus should I search?".into(),
                },
                requires_approval: false,
                inputs: Vec::new(),
                sampling: None,
                evaluation: None,
            }],
            edges: Vec::new(),
        })
    }

    /// The plan cannot fail (one step, no tools), so a replan is the
    /// same plan again — the executor never asks on this fixture.
    async fn replan(
        &self,
        original: &Plan,
        _completed: &[(usize, sovereign_core::types::StepOutput)],
        _failure: &sovereign_core::types::StepError,
        _tools: &[ToolDescriptor],
    ) -> sovereign_core::error::Result<Plan> {
        Ok(original.clone())
    }
}

/// A live turn, parked on its question, and the socket that owns it.
struct ParkedTurn {
    _tmp: tempfile::TempDir,
    _daemon: Arc<EmbeddedDaemon>,
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    /// Everything that arrived before the question.
    before: Vec<TurnFrame>,
    /// The id the question was asked under.
    prompt_id: String,
}

/// Drive a `ComplexTask` turn on a CLAIMED socket until it pauses on its
/// question.
///
/// Bounded: a hang here is the failure mode this fixture exists to catch, and
/// a test that hangs reports nothing.
async fn turn_parked_on_a_question(timers: SocketTimers) -> ParkedTurn {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let provider = TestProvider::new().with_complete_text("searched.");
    let services = common::desktop_services_with_planner(
        engine(tmp.path()),
        Arc::clone(&store),
        Arc::new(provider),
        Box::new(AskPlanner),
    );
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    );
    let addr = spawn_router(turn_router_with(Arc::clone(&daemon), timers)).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream?approvals=true"
    ))
    .await
    .expect("the claimed upgrade is accepted");
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Message {
            content: "search for the adoption figure".to_string(),
            mode: TurnMode::Grounded,
            intent: Some(sovereign_contracts::types::Intent::ComplexTask),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    let mut before: Vec<TurnFrame> = Vec::new();
    let prompt_id = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let Some(Ok(msg)) = ws.next().await else {
                panic!("socket closed before the turn asked its question");
            };
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            if let TurnFrame::Prompt { id, .. } = &frame {
                return id.clone();
            }
            before.push(frame);
        }
    })
    .await
    .expect("the turn reaches its question rather than hanging");

    ParkedTurn {
        _tmp: tmp,
        _daemon: daemon,
        ws,
        before,
        prompt_id,
    }
}

/// sv-surface R3 + the C1 OPEN VERIFICATION, closed: a REAL turn that
/// PAUSES on a question over the wire. Until this, no test anywhere drove
/// a turn that parks the executor through `serve_turn` +
/// `capabilities::scope` + a real WebSocket — the unit tests exercised
/// the channel in isolation, and the stub fixture's `NoOpPlanner` made
/// the ComplexTask path unreachable. The planner knob
/// (`desktop_services_with_planner`) exists for exactly this.
#[tokio::test]
async fn a_turn_pauses_on_a_question_over_the_wire_and_its_answer_resumes_it() {
    let ParkedTurn {
        _tmp,
        _daemon,
        mut ws,
        before: frames,
        prompt_id: id,
    } = turn_parked_on_a_question(SocketTimers::default()).await;

    // On THIS path the pause beats every other frame: ComplexTask is
    // non-streamable, so the whole turn — pause included — runs inside
    // `handle_message` before `serve_non_streaming_turn` has a response
    // to emit. Nothing but the Prompt could have arrived, and nothing
    // did.
    assert!(
        frames.is_empty(),
        "the pause is the wire's first frame; got {frames:?}"
    );
    assert!(
        id.ends_with(":input") && id.len() > ":input".len(),
        "ask_user parks under the one id it mints, PREFIXED by the turn's own nonce — a bare \
         `input` is spelled the same on every turn of this socket, so a late answer would \
         resolve the next turn's question (sv-surface C2); got {id:?}"
    );

    // The answer goes up the same socket; the parked executor resumes.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: id.clone(),
            answer: TurnAnswer::Text("sep".into()),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // And the turn completes. The tail frames are the fallback path's
    // fixed order — TurnStarted (late by nature on this path; the
    // comment in `serve_non_streaming_turn` says so), the whole answer
    // as one Token, then Complete.
    let tail = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut tail: Vec<TurnFrame> = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            let done = matches!(frame, TurnFrame::Complete { .. });
            tail.push(frame);
            if done {
                return tail;
            }
        }
        panic!("socket closed without a Complete after the answer");
    })
    .await
    .expect("the answer resumed the turn");

    let position_of = |pred: &dyn Fn(&TurnFrame) -> bool| {
        tail.iter()
            .position(pred)
            .unwrap_or_else(|| panic!("no matching frame in the tail: {tail:?}"))
    };
    let started = position_of(&|f| {
        matches!(
            f,
            TurnFrame::Notice {
                notice: TurnNotice::TurnStarted { .. }
            }
        )
    });
    let token = position_of(&|f| matches!(f, TurnFrame::Token { .. }));
    let complete = position_of(&|f| matches!(f, TurnFrame::Complete { .. }));
    assert!(
        started < token && token < complete,
        "the tail is TurnStarted, Token, Complete in order; got {tail:?}"
    );

    // The pause and the resume left nothing parked: a SECOND answer with
    // the same id resolves nothing and the refusal says so by name.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: id.clone(),
            answer: TurnAnswer::Text("stale".into()),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let answered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).expect("frames stay well-formed");
            match frame {
                TurnFrame::Notice {
                    notice: TurnNotice::ResolveAck { outcome, .. },
                } => return outcome,
                // sv-surface RB4, the whole point: the double-answer is a
                // recoverable refusal about one question. Riding the
                // terminal frame made a client throw away a turn that had
                // already completed perfectly well.
                TurnFrame::StreamError { message, .. } => {
                    panic!("a stale reply must not be reported as a dead turn; got {message:?}")
                }
                _ => continue,
            }
        }
        panic!("no frame answered the stale reply");
    })
    .await
    .expect("the stale reply is answered, not swallowed");
    assert_eq!(
        answered,
        ResolveOutcome::NoSuchPending,
        "the ack names what the reply found: nothing, because it was already answered"
    );
}

/// sv-surface G3b, end to end: a turn pauses on an INFORMATION request,
/// the client's answer carries its web-search registry rows, and the
/// daemon folds them into the conversation AS PART OF the resolve — one
/// user action (picking a search result), one atomic effect. The store
/// assertion is the point: the registry write landed daemon-side, where
/// the turn's synthesis reads it.
#[tokio::test]
async fn a_search_built_information_answer_folds_its_sources_into_the_conversation() {
    use sovereign_contracts::types::{InformationRequest, SearchedSourceEntry};

    struct AskInfoPlanner;
    #[async_trait::async_trait]
    impl sovereign_core::traits::Planner for AskInfoPlanner {
        async fn plan(
            &self,
            goal: &str,
            _context: &sovereign_contracts::types::ConversationContext,
            _tools: &[sovereign_contracts::types::ToolDescriptor],
        ) -> sovereign_core::error::Result<sovereign_contracts::types::Plan> {
            Ok(sovereign_contracts::types::Plan {
                id: "plan-info".into(),
                goal: goal.to_string(),
                steps: vec![sovereign_contracts::types::Step {
                    id: 0,
                    description: "Ask for the missing figure".into(),
                    kind: sovereign_contracts::types::StepKind::AwaitUserInfo {
                        request: InformationRequest {
                            current_understanding: "The answer needs one figure".into(),
                            gap: "The 2024 adoption rate".into(),
                            relevance: "It decides the trend".into(),
                            satisfying_source: "A statistics agency".into(),
                            search_hints: Vec::new(),
                            task_id: String::new(),
                            step_id: 0,
                            kind: Default::default(),
                            task_title: String::new(),
                            routes: Vec::new(),
                        },
                    },
                    requires_approval: false,
                    inputs: Vec::new(),
                    sampling: None,
                    evaluation: None,
                }],
                edges: Vec::new(),
            })
        }

        async fn replan(
            &self,
            original: &sovereign_contracts::types::Plan,
            _completed: &[(usize, sovereign_core::types::StepOutput)],
            _failure: &sovereign_core::types::StepError,
            _tools: &[sovereign_contracts::types::ToolDescriptor],
        ) -> sovereign_core::error::Result<sovereign_contracts::types::Plan> {
            Ok(original.clone())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn StateStore> = Arc::new(sovereign_store::memory::InMemoryStateStore::new());
    let provider = TestProvider::new().with_complete_text("answered.");
    let services = common::desktop_services_with_planner(
        engine(tmp.path()),
        Arc::clone(&store),
        Arc::new(provider),
        Box::new(AskInfoPlanner),
    );
    let daemon = Arc::new(EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        services,
    ));
    let addr = spawn_router(turn_router(Arc::clone(&daemon))).await;
    let conv = create_conversation(&format!("http://{addr}")).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream?approvals=true"
    ))
    .await
    .unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Message {
            content: "what is the adoption rate?".to_string(),
            mode: TurnMode::Grounded,
            intent: Some(sovereign_contracts::types::Intent::ComplexTask),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Read to the information prompt (bounded — a hang is the failure).
    let prompt_id = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let Some(Ok(msg)) = ws.next().await else {
                panic!("socket closed before the information request");
            };
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).unwrap();
            if let TurnFrame::Prompt { id, .. } = frame {
                return id;
            }
        }
    })
    .await
    .expect("the turn parks on its information request");
    assert!(
        prompt_id.ends_with(":info:0") && prompt_id.len() > ":info:0".len(),
        "the information request parks under the step's id, PREFIXED by the turn's nonce \
         (sv-surface C2); got {prompt_id:?}"
    );

    // The client's search-produced answer, carrying its registry rows.
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Answer {
            id: prompt_id,
            answer: TurnAnswer::Information {
                content: Some(
                    "Web search results for \"adoption rate\" (via duckduckgo):\n[1] 12.4%".into(),
                ),
                sources: vec![SearchedSourceEntry {
                    url: "https://example.org/adoption".into(),
                    title: "Adoption rates 2024".into(),
                    first_seen_turn: 0,
                    last_referenced_turn: 0,
                    search_query: "adoption rate".into(),
                }],
            },
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // ResolveAck arrives, then the turn completes.
    let saw_ack = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        use sovereign_contracts::types::TurnNotice;
        loop {
            let Some(Ok(msg)) = ws.next().await else {
                panic!("socket closed early");
            };
            let tokio_tungstenite::tungstenite::Message::Text(t) = msg else {
                continue;
            };
            let frame: TurnFrame = serde_json::from_str(&t).unwrap();
            match frame {
                TurnFrame::Notice {
                    notice: TurnNotice::ResolveAck { outcome, .. },
                } => return outcome,
                TurnFrame::Complete { .. } => panic!("Complete before the ResolveAck"),
                _ => {}
            }
        }
    })
    .await
    .expect("the answer is acknowledged");
    assert_eq!(
        saw_ack,
        sovereign_contracts::types::ResolveOutcome::Resolved,
        "the acknowledgement says the answer landed"
    );

    // THE assertion: the registry row landed in the daemon's store, part
    // of the same effect that resolved the answer.
    let conv_row = store
        .get_conversation(&conv)
        .await
        .expect("the conversation");
    let sources = conv_row.searched_sources.expect("the registry was written");
    assert_eq!(sources.len(), 1, "one row folded; got {sources:?}");
    assert_eq!(sources[0].url, "https://example.org/adoption");
    assert!(
        sources[0].first_seen_turn >= 1,
        "the daemon stamped the conversation's real turn count ({}), not the client's placeholder",
        sources[0].first_seen_turn
    );
}
