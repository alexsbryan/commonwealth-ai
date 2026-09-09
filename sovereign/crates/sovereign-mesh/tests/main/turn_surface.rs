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
use sovereign_contracts::types::{TurnFrame, TurnMode, TurnRequest};
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

/// Send an `Approve` and read the daemon's answer to it.
async fn approve_and_read_refusal(addr: std::net::SocketAddr, conv: &str, claim: bool) -> String {
    let query = if claim { "?approvals=true" } else { "" };
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/v1/conversations/{conv}/stream{query}"
    ))
    .await
    .expect("the daemon accepts the upgrade");
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&TurnRequest::Approve {
            task_id: conv.to_string(),
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
    .expect("an answer arrives rather than silence")
    .expect("the answer is a well-formed TurnFrame");

    match frame {
        TurnFrame::StreamError { message, .. } => message,
        other => panic!("expected a StreamError, got {other:?}"),
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

    let message = approve_and_read_refusal(addr, &conv, false).await;
    assert!(
        message.contains("did not claim") && message.contains("approvals=true"),
        "the refusal names the claim the client is missing and how to make it; \
         got {message:?}"
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

    let message = approve_and_read_refusal(addr, &conv, true).await;
    assert!(
        message.contains("no approval is pending") && !message.contains("did not claim"),
        "a claimed socket is answered about its QUESTIONS, not about its claim; \
         got {message:?}"
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
