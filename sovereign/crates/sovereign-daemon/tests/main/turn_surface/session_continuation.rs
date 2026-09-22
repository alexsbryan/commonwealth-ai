// SPDX-License-Identifier: AGPL-3.0-or-later
//! Part of the turn_surface suite — split for the §3.2 size ceiling (behaviour-preserving move).
//!
//! Session continuation over the wire (sv-surface rung 6, C2-b):
//! `Redirect` and `Resume`, their refusals, and the rule that a
//! socket cannot name a session belonging to another conversation.

use crate::common::{spawn_router, TestProvider};

use std::sync::Arc;

use futures::StreamExt;
use sovereign_contracts::types::{TurnFrame, TurnMode, TurnRequest};
use sovereign_daemon::{turn_http::turn_router, EmbeddedDaemon};

use super::{create_conversation, open_stream, send_request, serving_daemon};

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
