// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The phone serves a turn** — the falsifier for sv-surface R6.
//!
//! # The named failing input (ARCH §18.1)
//!
//! Put back any hand-rolled protocol type in `sovereign-mobile` and these
//! tests fail, because the FIXTURE HOST here serializes the CONTRACT's own
//! `TurnFrame` values. A mirror that has drifted by one field — exactly what
//! `remote/dto.rs::ServerEvent` had done, silently, before R6 — produces a
//! parse error or a dropped frame, and `a_turn_reaches_the_mobile_view`
//! asserts on the rendered result rather than on the frame count, so a
//! dropped citation is a red test rather than a quieter UI.
//!
//! Concretely: revert `Citation` to the old `CitationDto` (no `url`, no
//! `provenance_tier`) and the `metadata.retrieved_chunks[0].url` assertion
//! fails. Delete the `drain_after_complete` call in `remote::stream::settle`
//! and `the_host_settles_after_complete` fails: `Complete` still arrives, no
//! `turn_settled` notice ever does, and the UI would sit on a spinner —
//! which is the G13 repro this bookend exists to close.
//!
//! # Why the fixture is here and not `sovereign-mesh`'s
//!
//! `sovereign-mesh`'s `EmbeddedDaemon` turn fixture lives in
//! `tests/main/common` — an integration-test module, not a published item —
//! so a dev-dependency cannot name it, and the crate itself is gated behind
//! `feature = "treesitter"` and carries corpus-engine, llama.cpp and iroh.
//! What is actually under test here is the CLIENT half, and for that a real
//! listener speaking real frames is the whole requirement. This is the same
//! shape `sovereign-turn-client`'s own `stream_split_tests` uses.
//!
//! What these tests do NOT prove: answer quality, retrieval, or that the
//! daemon emits these frames in this order. That is the daemon's own
//! falsifier (`sovereign-mesh/tests/main/turn_surface.rs`). These prove the
//! phone's half — that what the contract serializes is what the mobile view
//! renders.

use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use rusqlite::Connection;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use sovereign_turn_client::{
    ActionPreview, Citation, Provenance, ProvenanceSource, ResolveOutcome, TurnFrame, TurnMode,
    TurnNotice, TurnPrompt,
};

use sovereign_mobile_lib::cache::schema;
use sovereign_mobile_lib::remote::stream::{self, SenderRegistry, TurnEvents, TurnRun};

// ─── The recorder: what the WebView would have received ───────────

/// Stands in for the `AppHandle`. Every assertion below reads THIS — the
/// events the shared chat FSM consumes — rather than the frames, because a
/// frame the phone parsed and then dropped is the failure mode a
/// frame-counting test cannot see.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
}

impl TurnEvents for Recorder {
    fn emit_json(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .unwrap()
            .push((event.to_string(), payload));
    }
}

impl Recorder {
    fn names(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// The first payload emitted under `event`.
    fn first(&self, event: &str) -> Option<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| n == event)
            .map(|(_, v)| v.clone())
    }

    /// Every `turn-notice` payload whose `kind` matches.
    fn notices(&self, kind: &str) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, v)| n == "turn-notice" && v["kind"] == kind)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

// ─── The cache, with the parent rows the FK constraints need ───────

const HOST: &str = "h1";
const CONV: &str = "c1";

fn cache() -> Arc<Mutex<Connection>> {
    let db = Connection::open_in_memory().expect("in-memory cache");
    schema::migrate(&db).expect("migrate");
    db.execute(
        "INSERT INTO host_connection (id, display_name, tailnet_address, created_at)
         VALUES (?1, 'fixture', '127.0.0.1:0', 0)",
        rusqlite::params![HOST],
    )
    .unwrap();
    db.execute(
        "INSERT INTO conversation (id, host_connection_id, title, created_at, updated_at)
         VALUES (?1, ?2, 'fixture', 0, 0)",
        rusqlite::params![CONV, HOST],
    )
    .unwrap();
    Arc::new(Mutex::new(db))
}

// ─── The fixture host ──────────────────────────────────────────────

fn citation() -> Citation {
    Citation {
        corpus_id: "sep".into(),
        chunk_id: "4211".into(),
        title: Some("Supervenience".into()),
        snippet: "B-properties supervene on A-properties.".into(),
        score: 0.91,
        rank: 0,
        // The two fields the deleted mirror had no room for. They are the
        // reason this test can tell adoption from a rename.
        url: Some("https://plato.stanford.edu/entries/supervenience/".into()),
        provenance_tier: Some("grounded".into()),
    }
}

fn provenance() -> Provenance {
    Provenance {
        inference_backend: "Qwen3.5-9B.Q8_0 @ peer mac-peer".into(),
        routing_tier: Some("KnowledgeQuery".into()),
        ttft_ms: Some(180),
        total_ms: Some(2_400),
        finish_reason: Some("stop".into()),
        max_tokens_budget: Some(1_024),
        completion_tokens: Some(37),
        sources: vec![ProvenanceSource {
            origin: "sep".into(),
            count: 3,
            from_peer: Some("mac-peer".into()),
            // sv-surface D7/G9. `None` here: this fixture is a corpus,
            // not a watched folder, so the surface renders `origin`.
            display_name: None,
        }],
    }
}

fn text(frame: &TurnFrame) -> WsMessage {
    WsMessage::Text(
        serde_json::to_string(frame)
            .expect("frames serialize")
            .into(),
    )
}

fn notice(n: TurnNotice) -> WsMessage {
    text(&TurnFrame::Notice { notice: n })
}

/// A listener that serves ONE turn.
///
/// `hang_up_after` is the load-bearing knob: `false` keeps reading the
/// client's requests (the real host's idle window after settling), `true`
/// drops the socket the moment the frames are out — which is how a mid-turn
/// hangup is reproduced, and the ONLY way `a_dropped_socket_is_reported_not_
/// completed` can observe one. A fixture that always kept reading would
/// leave that test parked in `next_frame` forever.
///
/// Returns the bound address and a handle carrying every request the client
/// put on the wire, so a test can assert what was SENT as well as what was
/// rendered.
async fn fixture_host(
    frames: Vec<WsMessage>,
    hang_up_after: bool,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        let mut received = Vec::new();
        // The host does not speak until asked — the real one does not
        // either, and a client that never sent its `TurnRequest::Message`
        // would otherwise pass by accident.
        if let Some(Ok(WsMessage::Text(t))) = ws.next().await {
            received.push(t.to_string());
        }
        for f in frames {
            if ws.send(f).await.is_err() {
                break;
            }
        }
        if hang_up_after {
            // Dropping the sink closes the socket: the client sees the
            // hangup rather than an idle host.
            return received;
        }
        // Otherwise read anything else the client sends (answers, cancels)
        // until it hangs up, so `received` is the complete request log.
        while let Some(Ok(msg)) = ws.next().await {
            if let WsMessage::Text(t) = msg {
                received.push(t.to_string());
            }
        }
        received
    });
    (format!("http://{addr}"), handle)
}

fn run(base_url: String, claim: bool) -> TurnRun {
    TurnRun {
        base_url,
        conversation_id: CONV.to_string(),
        content: "What is supervenience?".into(),
        mode: TurnMode::Grounded,
        claim_approvals: claim,
    }
}

// ─── The tests ─────────────────────────────────────────────────────

/// THE R6 claim, end to end: a real socket, real `TurnFrame`s in, and the
/// mobile view carrying the answer AND its citations — including the two
/// citation fields the deleted mirror could not represent.
#[tokio::test]
async fn a_turn_reaches_the_mobile_view() {
    let (base, host) = fixture_host(
        vec![
            notice(TurnNotice::TurnStarted {
                message_id: "m1".into(),
            }),
            text(&TurnFrame::Token {
                message_id: "m1".into(),
                chunk: "Supervenience is ".into(),
            }),
            text(&TurnFrame::Token {
                message_id: "m1".into(),
                chunk: "a dependence relation.".into(),
            }),
            text(&TurnFrame::Complete {
                message_id: "m1".into(),
                provenance: Some(provenance()),
                citations: vec![citation()],
                epistemic_state: None,
                task: None,
                metadata: None,
            }),
            notice(TurnNotice::TurnSettled {
                message_id: "m1".into(),
            }),
        ],
        false,
    )
    .await;

    let events = Arc::new(Recorder::default());
    let db = cache();
    stream::run_stream(
        events.clone() as Arc<dyn TurnEvents>,
        db.clone(),
        SenderRegistry::default(),
        run(base, false),
    )
    .await
    .expect("the turn drives to completion");

    // The FSM's ordering contract: the placeholder exists before any chunk.
    let names = events.names();
    let start = names.iter().position(|n| n == "message-start");
    let chunk = names.iter().position(|n| n == "message-chunk");
    assert!(
        start.is_some() && start < chunk,
        "message-start must precede the first message-chunk, got {names:?}"
    );

    let complete = events
        .first("message-complete")
        .expect("the turn completes for the WebView, not just on the wire");
    assert_eq!(
        complete["full_text"], "Supervenience is a dependence relation.",
        "the rendered answer is every Token concatenated in order"
    );

    // The citation, as `SourceAttribution` reads it.
    let chunks = complete["metadata"]["retrieved_chunks"]
        .as_array()
        .expect("citations reach the view as retrieved_chunks");
    assert_eq!(chunks.len(), 1, "one citation in, one citation rendered");
    assert_eq!(chunks[0]["corpus_id"], "sep");
    assert_eq!(chunks[0]["chunk_id"], "4211");
    assert_eq!(
        chunks[0]["url"], "https://plato.stanford.edu/entries/supervenience/",
        "the wire has carried Citation::url for weeks; the mirror had no field for it"
    );
    assert_eq!(
        chunks[0]["provenance_tier"], "grounded",
        "the host's own tier, not the hardcoded \"corpus\" the mirror forced"
    );

    // Provenance, as `RoutingMeta` reads it (acceptance §5).
    assert_eq!(
        complete["metadata"]["provenance"]["inference_backend"],
        "Qwen3.5-9B.Q8_0 @ peer mac-peer"
    );

    // And it is PERSISTED — an app killed here still renders it (§3, §11).
    {
        let conn = db.lock().unwrap();
        let (content, status): (String, String) = conn
            .query_row(
                "SELECT content, status FROM message WHERE id = 'm1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the assistant message is in the cache");
        assert_eq!(content, "Supervenience is a dependence relation.");
        assert_eq!(status, "complete");
        let url: Option<String> = conn
            .query_row(
                "SELECT url FROM citation WHERE message_id = 'm1'",
                [],
                |r| r.get(0),
            )
            .expect("the citation is in the cache");
        assert_eq!(
            url.as_deref(),
            Some("https://plato.stanford.edu/entries/supervenience/"),
            "an offline read must render what a live one did (ARCH §18.3)"
        );
    }

    // The request the phone actually sent is the contract's own shape.
    let sent = host.await.unwrap();
    let first: Value = serde_json::from_str(&sent[0]).expect("the request is JSON");
    assert_eq!(
        first["type"], "message",
        "the phone sends TurnRequest::Message, built by TurnSender — not a hand-written blob"
    );
    assert_eq!(first["data"]["content"], "What is supervenience?");
}

/// `Complete` ends the turn; `Notice::TurnSettled` ends the host's talking.
/// The phone must read the second window, or a post-`Complete` refinement
/// never arrives and the UI hangs on "Refining your answer" (protocol note
/// E1, G13).
#[tokio::test]
async fn the_host_settles_after_complete() {
    let (base, host) = fixture_host(
        vec![
            text(&TurnFrame::Token {
                message_id: "m1".into(),
                chunk: "hi".into(),
            }),
            text(&TurnFrame::Complete {
                message_id: "m1".into(),
                provenance: None,
                citations: vec![],
                epistemic_state: None,
                task: None,
                metadata: None,
            }),
            // AFTER the terminal frame — the window a drain that stopped at
            // Complete would never see.
            notice(TurnNotice::ResolveAck {
                id: "q1".into(),
                outcome: ResolveOutcome::NoSuchPending,
            }),
            notice(TurnNotice::TurnSettled {
                message_id: "m1".into(),
            }),
        ],
        false,
    )
    .await;

    let events = Arc::new(Recorder::default());
    stream::run_stream(
        events.clone() as Arc<dyn TurnEvents>,
        cache(),
        SenderRegistry::default(),
        run(base, false),
    )
    .await
    .expect("the drive returns once the host settles");
    drop(host);

    let acks = events.notices("resolve_ack");
    assert_eq!(
        acks.len(),
        1,
        "a notice sent after Complete still reaches the view"
    );
    assert_eq!(
        acks[0]["notice"]["resolve_ack"]["outcome"], "no_such_pending",
        "ResolveOutcome crosses as itself — the bool it replaced could not \
         tell 'accepted' from 'never arrived'"
    );
    assert_eq!(
        events.notices("turn_settled").len(),
        1,
        "the bookend arrives, so the drive knows the socket is finished"
    );
}

/// The write half, from another task: the host parks on a `Prompt`, the
/// phone's `answer_prompt` command answers it through the `TurnSender`
/// while the drive is inside `next_frame`, and the host acknowledges with a
/// typed `ResolveAck` (R2/G11 + G8).
#[tokio::test]
async fn a_prompt_is_answered_from_the_command_thread() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let host = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        // The turn request.
        let _ = ws.next().await;
        // Ask, and do not stream until answered — the parked executor.
        ws.send(text(&TurnFrame::Prompt {
            id: "step:1".into(),
            prompt: TurnPrompt::Approval {
                preview: ActionPreview {
                    tool_id: "shell".into(),
                    description: "Run it".into(),
                    params: serde_json::json!({ "cmd": "ls" }),
                },
            },
        }))
        .await
        .unwrap();
        let answer = match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await
        {
            Ok(Some(Ok(WsMessage::Text(t)))) => t.to_string(),
            other => panic!("the host was left parked instead of answered: {other:?}"),
        };
        ws.send(notice(TurnNotice::ResolveAck {
            id: "step:1".into(),
            outcome: ResolveOutcome::Resolved,
        }))
        .await
        .unwrap();
        ws.send(text(&TurnFrame::Complete {
            message_id: "m1".into(),
            provenance: None,
            citations: vec![],
            epistemic_state: None,
            task: None,
            metadata: None,
        }))
        .await
        .unwrap();
        ws.send(notice(TurnNotice::TurnSettled {
            message_id: "m1".into(),
        }))
        .await
        .unwrap();
        answer
    });

    let events = Arc::new(Recorder::default());
    let senders = SenderRegistry::default();

    // The answerer: a SEPARATE task, the way a Tauri command is. It waits
    // for the prompt to reach the view, then answers by id.
    let answerer = {
        let events = events.clone();
        let senders = senders.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if let Some(p) = events.first("turn-prompt") {
                    assert_eq!(p["kind"], "approval", "the card knows which prompt it is");
                    let id = p["id"].as_str().unwrap().to_string();
                    return stream::answer_prompt(
                        &senders,
                        CONV,
                        &id,
                        &sovereign_turn_client::TurnAnswer::Approved(true),
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            panic!("no prompt ever reached the view");
        })
    };

    stream::run_stream(
        events.clone() as Arc<dyn TurnEvents>,
        cache(),
        senders,
        run(format!("http://{addr}"), true),
    )
    .await
    .expect("the turn completes once the prompt is answered");
    answerer
        .await
        .unwrap()
        .expect("the answer went onto the wire from the command thread");

    let on_the_wire = host.await.unwrap();
    let sent: Value = serde_json::from_str(&on_the_wire).unwrap();
    assert_eq!(
        sent["type"], "answer",
        "the phone sends TurnRequest::Answer — the one inbound variant"
    );
    assert_eq!(
        sent["data"]["id"], "step:1",
        "answered by the host's own id"
    );
    assert_eq!(sent["data"]["answer"]["approved"], true);

    let acks = events.notices("resolve_ack");
    assert_eq!(acks.len(), 1, "the acknowledgement reaches the view");
    assert_eq!(acks[0]["notice"]["resolve_ack"]["outcome"], "resolved");
}

/// A socket that dies mid-turn is a DROPPED turn, said so: the in-flight
/// message is left `streaming` for reconnect to re-fetch, and the view is
/// told. Never a half-written answer presented as the answer (ARCH §18.3).
#[tokio::test]
async fn a_dropped_socket_is_reported_not_completed() {
    let (base, host) = fixture_host(
        vec![text(&TurnFrame::Token {
            message_id: "m1".into(),
            chunk: "half an ans".into(),
        })],
        // Hang up mid-turn: no Complete, no TurnSettled, socket gone.
        true,
    )
    .await;

    let events = Arc::new(Recorder::default());
    let db = cache();
    // The message row must pre-exist for the status flip to land on it.
    db.lock()
        .unwrap()
        .execute(
            "INSERT INTO message (id, conversation_id, role, content, status, created_at)
             VALUES ('m1', ?1, 'assistant', '', 'streaming', 0)",
            rusqlite::params![CONV],
        )
        .unwrap();

    // The fixture sends its one token and then drops the socket.
    stream::run_stream(
        events.clone() as Arc<dyn TurnEvents>,
        db.clone(),
        SenderRegistry::default(),
        run(base, false),
    )
    .await
    .expect("a hangup is reported, not an Err");
    host.await.unwrap();

    assert!(
        events.first("message-complete").is_none(),
        "a dropped socket must not look like a finished turn"
    );
    let err = events
        .first("message-error")
        .expect("the view is told the stream died");
    // The reason travels. An abrupt reset — what iOS does to a backgrounded
    // socket — is not a clean close, and the banner can only say which if
    // the drive passes the words along rather than flattening both.
    let msg = err["message"].as_str().expect("the error names itself");
    assert!(
        msg.contains("reset") || msg.contains("closed before completion"),
        "the hangup reason reaches the view, got {msg:?}"
    );

    let status: String = db
        .lock()
        .unwrap()
        .query_row("SELECT status FROM message WHERE id = 'm1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        status, "streaming",
        "left streaming so reconnect re-fetches it"
    );
}
