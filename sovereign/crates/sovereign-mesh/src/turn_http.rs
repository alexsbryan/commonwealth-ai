// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon serves the turn — `POST /v1/conversations` and
//! `GET /v1/conversations/{id}/stream`.
//!
//! # Why this exists
//!
//! `quality/TOPOLOGY.md` §3.5 draws every surface — desktop, `svrn chat`, the
//! server, the bench — above ONE process that assembles a `Runtime`, with a
//! turn protocol between them. Phase 5b made the protocol a value
//! (`sovereign_contracts::types::{TurnRequest, TurnFrame}`), phase 5c made
//! driving a turn a library function (`sovereign_core::runtime::serve_turn`),
//! and this is the door: the first place the daemon itself answers one.
//!
//! Before it, `sovereign daemon run` held the corpus engine, the state store
//! and the routed inference provider — every ingredient of an answer — and
//! served none, so the only way to get a turn was to be a host that had built
//! its own `Runtime`. That is what made "one process assembles" unstatable.
//!
//! # The wire form is `sovereign-server`'s, deliberately
//!
//! Same paths, same frames. A client that speaks to the server speaks to the
//! daemon without knowing which it reached, which is the property that lets a
//! host stop assembling and start connecting (phase 6). The differences are
//! the two the daemon genuinely has and the server does not:
//!
//! - **No tenant scoping.** The server prefixes `{tenant}:{conv}` because it
//!   is a multi-tenant hub; a local daemon has one principal. `serve_turn`
//!   takes an ALREADY-SCOPED id for exactly this reason — prefixing is a host
//!   policy — so the daemon passes the id through and the server keeps its
//!   `TenantRuntime`.
//! - **No fair scheduler, no reciprocity.** Those price a shared hub's
//!   contention between strangers. Loopback callers are one user's own
//!   surfaces.
//!
//! # Loopback only
//!
//! Both layers, mirroring `reading_http` and `admin_http`: the router-level
//! [`crate::loopback_guard::loopback_only`] middleware and a per-handler peer
//! check. A turn runs this host's tools against this host's corpora; it is not
//! a peer-facing surface, and `/v1/chat/completions` — which IS peer-facing —
//! deliberately remains raw completion with none of it.
//!
//! # Known gap, stated rather than discovered
//!
//! Narration frames are NOT emitted. `serve_turn` takes the narration
//! broadcast when a host installed one, and the daemon's `Runtime` carries the
//! recipe's default no-op sink: §3.5 lists `routing_events` among the five
//! capabilities that leave the `Runtime` entirely, because it is a
//! per-connection wire concern, and the daemon does not yet own the
//! per-connection subscription that would make it one. A client therefore sees
//! `Token`s and a terminal `Complete`, and no progress in between. Nothing
//! regresses — no host served a turn from the daemon before this file — but a
//! reader should know it is missing on purpose.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, Extension, Path, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use sovereign_contracts::types::{TurnFrame, TurnRequest};
use sovereign_core::runtime::serve_turn;

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

/// Mount the turn surface. Built from `Arc<Self>` by `start_daemon`, like the
/// mesh, admin and reading routers — so a serving daemon cannot come up
/// without it and `mount_names` reports exactly what it has.
pub fn turn_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/conversations", post(create_conversation))
        .route("/v1/conversations/{id}/stream", get(ws_handler))
        .route("/v1/conversations/{id}/end", post(end_conversation))
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateConversationRequest {
    /// Workspace skill to tag the conversation with, so
    /// `Runtime::resolve_active_mode` routes it into that agent loop from the
    /// first message rather than after one untagged turn.
    #[serde(default)]
    pub skill_id: Option<String>,
    /// The per-conversation retrieval allow-list
    /// (`Conversation::enabled_corpora`), so a surface can scope a turn to
    /// named corpora the way the desktop's chip strip does. Absent means
    /// "every installed corpus". Validated by `Runtime::seed_conversation`
    /// against the corpora this daemon would actually search: an unknown id
    /// is a 400 naming it and the installed list, never a silent widen.
    #[serde(default)]
    pub enabled_corpora: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct CreateConversationResponse {
    pub id: String,
    pub created_at: i64,
    /// The allow-list that was seeded, echoed back VERBATIM when one was
    /// sent and omitted otherwise. The echo is what lets a client tell a
    /// daemon that scoped the conversation from one that predates the field
    /// and ignored it — serde drops unknown keys, so without this a stale
    /// daemon would mint an unscoped conversation and say nothing (§18.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_corpora: Option<Vec<String>>,
}

/// `POST /v1/conversations`
///
/// Seeds the row before the first message, which is what makes the skill tag
/// load-bearing (same reason `sovereign-server`'s create route seeds rather
/// than letting the first turn create the conversation). A missing or
/// malformed body yields an untagged conversation rather than a 4xx — the
/// server's behaviour, kept so one client works against both.
async fn create_conversation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(runtime) = daemon.runtime() else {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    };
    let req: CreateConversationRequest = if body.is_empty() {
        CreateConversationRequest::default()
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Err(e) = runtime
        .seed_conversation(
            &id,
            now,
            req.skill_id.as_deref(),
            req.enabled_corpora.as_deref(),
        )
        .await
    {
        // A bad allow-list is the CALLER's mistake and carries its own remedy
        // (the installed ids); a store failure is the daemon's. Different
        // status codes so a client can tell "fix your flag" from "the daemon
        // is unwell" without parsing prose.
        return match e {
            sovereign_core::error::Error::InvalidInput(msg) => bad_request(&msg),
            other => service_unavailable(&format!("seed conversation: {other}")),
        };
    }
    Json(CreateConversationResponse {
        id,
        created_at: now,
        enabled_corpora: req.enabled_corpora,
    })
    .into_response()
}

/// `POST /v1/conversations/{id}/end`
///
/// Runs the conversation-end memory-extraction pass. This is a lifecycle
/// operation, not a turn, which is why it is a REST route beside `create`
/// rather than a `TurnRequest` variant — `TurnRequest`'s own doc says it is
/// "Client → host, for ONE turn", and a client that quits its REPL is not
/// taking a turn.
///
/// It exists because `svrn chat session` called `Runtime::end_conversation`
/// on `quit`, and phase 6 turns that host into a client. Without a wire form
/// the conversion would have silently stopped extracting long-term memories
/// on the one interactive CLI surface — a capability disappearing because the
/// process that used to hold it stopped holding it, which is the failure mode
/// phase 6 has to not have.
async fn end_conversation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let Some(runtime) = daemon.runtime() else {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    };
    match runtime.end_conversation(&conversation_id).await {
        Ok(()) => Json(serde_json::json!({ "ended": conversation_id })).into_response(),
        // Reported, not swallowed: the extraction pass runs a model, and a
        // caller told "ok" for a pass that never ran cannot tell the
        // difference (ARCH §18.3).
        Err(e) => service_unavailable(&format!("end conversation: {e}")),
    }
}

/// `GET /v1/conversations/{id}/stream` — WebSocket upgrade.
async fn ws_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Path(conversation_id): Path<String>,
) -> Response {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    if daemon.runtime().is_none() {
        return service_unavailable("this daemon serves no turns (mesh-admin)");
    }
    ws.on_upgrade(move |socket| handle_ws(socket, daemon, conversation_id))
}

async fn handle_ws(socket: WebSocket, daemon: Arc<EmbeddedDaemon>, conversation_id: String) {
    let (Some(runtime), Some(store)) = (daemon.runtime(), daemon.state_store()) else {
        // Re-checked after the upgrade because the borrow cannot cross it.
        // Unreachable in practice — `ws_handler` refused above.
        return;
    };
    let runtime = Arc::clone(runtime);
    let store = Arc::clone(store);

    let (mut ws_tx, mut ws_rx) = socket.split();

    // One writer to the socket, fed by the per-turn frame channel. There is no
    // second source here: the server multiplexes a fan-out approval broadcast
    // onto the same socket, and phase 5b split those into a different type
    // precisely because mixing them leaked one tenant's tokens to every
    // client. The daemon has no fan-out events, so it has one channel.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<TurnFrame>();
    let tx_handle = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let Ok(json) = serde_json::to_string(&frame) else {
                continue;
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break; // client disconnected
            }
        }
    });

    // ONE in-flight turn per socket, held as a task rather than awaited inline.
    //
    // The obvious shape — `serve_turn(..).await` right here — is what
    // `sovereign-server`'s WebSocket handler does, and it has a defect this
    // route would have inherited: while the turn runs, the receive stream is
    // never polled, so the connection answers no PINGS. Every standards-
    // compliant client with keepalive (python `websockets` defaults to a 20s
    // ping with a 20s deadline) therefore drops mid-answer — and a grounded
    // turn over a large corpus routinely runs minutes on a contended host.
    // Observed here on the very first real turn against a deployed daemon:
    // `keepalive ping timeout` at 20s while the daemon's own log showed the
    // turn's retrieval completing normally. Spawning it and continuing to poll
    // `ws_rx` is what keeps the socket alive; the `in_flight` guard is what
    // keeps "one turn per socket" true without the receive loop having to
    // block to enforce it.
    let mut in_flight: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let turn_finished = async {
            match in_flight.as_mut() {
                Some(h) => {
                    let _ = h.await;
                }
                // No turn running: never resolve, so the select waits on the
                // socket alone.
                None => std::future::pending().await,
            }
        };
        let incoming = tokio::select! {
            _ = turn_finished => {
                in_flight = None;
                continue;
            }
            incoming = ws_rx.next() => incoming,
        };
        let Some(Ok(msg)) = incoming else {
            break;
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            // Ping/Pong are handled beneath us by the WebSocket codec — but
            // only while this stream is being polled, which is the whole
            // reason the turn above is a task.
            _ => continue,
        };
        let event: TurnRequest = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("turn_http: invalid WebSocket message: {e}");
                let _ = out_tx.send(TurnFrame::StreamError {
                    message: format!("could not parse that as a TurnRequest: {e}"),
                    retry_after_secs: None,
                });
                continue;
            }
        };
        match event {
            TurnRequest::Message {
                content,
                mode,
                intent,
            } => {
                if in_flight.is_some() {
                    // Refused, not queued: a client that sent a second turn
                    // and heard nothing cannot tell "queued" from "lost".
                    let _ = out_tx.send(TurnFrame::StreamError {
                        message: "a turn is already in flight on this socket".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }
                let (rt, st, cid, tx) = (
                    Arc::clone(&runtime),
                    Arc::clone(&store),
                    conversation_id.clone(),
                    out_tx.clone(),
                );
                in_flight = Some(tokio::spawn(async move {
                    serve_turn(
                        &rt,
                        st.as_ref(),
                        &cid,
                        &content,
                        mode,
                        intent,
                        // See the module docs — the daemon has no narration
                        // broadcast yet, and a `None` here is that fact rather
                        // than a dropped channel.
                        None,
                        &tx,
                    )
                    .await;
                }));
            }
            // v1 has no daemon-side session owner to route an approval to
            // (TOPOLOGY hazard 12, a phase 5 deliverable). Refusing loudly
            // beats accepting and silently doing nothing: a client that sent
            // an approval and got no frame cannot tell "granted" from "never
            // arrived" (ARCH §18.3).
            TurnRequest::Approve { .. } | TurnRequest::UserReply { .. } => {
                let _ = out_tx.send(TurnFrame::StreamError {
                    message: "this daemon does not accept mid-turn approvals or \
                              user replies (no daemon-side session owner yet)"
                        .to_string(),
                    retry_after_secs: None,
                });
            }
        }
    }

    if let Some(h) = in_flight {
        // The client hung up mid-turn. Nothing is left to receive the frames,
        // and a turn whose sink is gone is work nobody asked to keep.
        h.abort();
    }
    tx_handle.abort();
}

fn bad_request(reason: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}

fn service_unavailable(reason: &str) -> Response {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": reason })),
    )
        .into_response()
}
