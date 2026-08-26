// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

use sovereign_contracts::types::{TurnFrame, TurnRequest};
use sovereign_core::runtime::{serve_turn, Runtime};
use sovereign_core::traits::StateStore;
use sovereign_core::types::TurnNarration;

use crate::approval::ServerApprovalChannel;
use crate::auth::TenantId;
use crate::reciprocity::{user_key, ReciprocityTable};
use crate::scheduler::{FairScheduler, UserKey};
use crate::tenant::TenantRuntime;

/// WebSocket upgrade handler for /v1/conversations/:id/stream
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(store): Extension<Arc<dyn StateStore>>,
    Extension(tenant): Extension<TenantId>,
    Extension(approval): Extension<Arc<ServerApprovalChannel>>,
    Extension(sched): Extension<FairScheduler>,
    Extension(reciprocity): Extension<Arc<ReciprocityTable>>,
    Extension(narration_tx): Extension<broadcast::Sender<TurnNarration>>,
    headers: HeaderMap,
    Path(conversation_id): Path<String>,
) -> Response {
    // Resolve the fairness/reciprocity key once for the socket's lifetime
    // (mesh-routed sockets carry `X-Node-Id`; local ones key on tenant).
    let key = user_key(&tenant, &headers);
    ws.on_upgrade(move |socket| {
        handle_ws(
            socket,
            runtime,
            store,
            tenant,
            approval,
            sched,
            reciprocity,
            key,
            narration_tx,
            conversation_id,
        )
    })
}

#[allow(clippy::too_many_arguments)]
async fn handle_ws(
    socket: WebSocket,
    runtime: Arc<Runtime>,
    store: Arc<dyn StateStore>,
    tenant: TenantId,
    approval: Arc<ServerApprovalChannel>,
    sched: FairScheduler,
    reciprocity: Arc<ReciprocityTable>,
    key: UserKey,
    narration_tx: broadcast::Sender<TurnNarration>,
    conversation_id: String,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let tr = TenantRuntime::new(Arc::clone(&runtime), store, tenant.0.clone());

    // Single writer to the socket. A forwarder task drains two sources:
    //   (a) this connection's per-turn token stream, via `out_rx`, and
    //   (b) the shared approval/step broadcast (approval + user-input).
    // Streaming tokens go ONLY through `out_tx` — never the broadcast —
    // because the broadcast fans to every connected socket and would
    // leak one tenant's tokens onto another client's stream.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<TurnFrame>();
    let mut broadcast_rx = approval.subscribe();

    let tx_handle = tokio::spawn(async move {
        loop {
            // Serialised in the arm rather than after the select: since
            // Phase 5b the per-turn frames and the fan-out events are
            // different types on purpose, and the only thing they have in
            // common is that both render to one JSON envelope.
            let json = tokio::select! {
                Some(ev) = out_rx.recv() => serde_json::to_string(&ev),
                ev = broadcast_rx.recv() => match ev {
                    Ok(ev) => serde_json::to_string(&ev),
                    // Slow consumer dropped some broadcast events — skip
                    // and keep going; tokens (on out_rx) are unaffected.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
                else => break,
            };
            let json = match json {
                Ok(j) => j,
                Err(_) => continue,
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break; // client disconnected
            }
        }
    });

    // Process incoming messages from the WebSocket client.
    //
    // ONE in-flight turn per socket, held as a TASK rather than awaited inline.
    // Inline is what this handler did until 2026-08-25, and it has a defect
    // that is invisible from the server side: while the turn runs, `ws_rx` is
    // never polled, so the connection answers no PINGS. Every standards-
    // compliant client with keepalive (python `websockets` defaults to a 20s
    // ping with a 20s deadline) drops mid-answer — and a grounded turn over a
    // real corpus routinely runs minutes (235s, measured on the daemon's
    // equivalent route). The server's log shows the turn completing normally
    // while the client saw a dead socket. Found on `sovereign_mesh::turn_http`,
    // which was written from this file; fixed in both.
    //
    // The scheduler permit moves INTO the task, so its lifetime is still
    // exactly the turn's — that is what the old comment meant by "holding the
    // receive loop here keeps the permit scoped to one in-flight turn", and it
    // is preserved by ownership instead of by blocking.
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
                tracing::warn!("Invalid WebSocket message: {e}");
                continue;
            }
        };

        match event {
            TurnRequest::Message { content } => {
                if in_flight.is_some() {
                    // Refused, not queued: a client that sent a second turn
                    // and heard nothing cannot tell "queued" from "lost".
                    let _ = out_tx.send(TurnFrame::StreamError {
                        message: "a turn is already in flight on this socket".to_string(),
                        retry_after_secs: None,
                    });
                    continue;
                }
                // Fair scheduler — admit this turn, streaming live queue
                // position to the client while it waits. The permit moves into
                // the spawned turn and drops with it (one in-flight turn per
                // socket). On shed, surface the busy state and skip the turn
                // (never an unbounded hang).
                let weight = reciprocity.weight_for(&key);
                let pos_tx = out_tx.clone();
                let admit = sched
                    .admit(key.clone(), weight, move |status| {
                        let _ = pos_tx.send(TurnFrame::QueuePosition {
                            position: status.position,
                            estimated_wait_ms: status.estimated_wait_ms,
                        });
                    })
                    .await;
                let permit = match admit {
                    Ok(permit) => permit,
                    Err(shed) => {
                        tracing::warn!(
                            conversation_id = %conversation_id,
                            available = sched.available(),
                            would_be_position = shed.would_be_position,
                            "host_busy: shedding WS turn"
                        );
                        let _ = out_tx.send(TurnFrame::StreamError {
                            message: "host busy".to_string(),
                            retry_after_secs: Some(shed.retry_after_secs),
                        });
                        continue;
                    }
                };
                approval.set_task_id(&conversation_id).await;
                // The turn loop itself is `sovereign_core::runtime::serve_turn`
                // — one implementation, shared with every other host
                // (TOPOLOGY.md §10 phase 5c). Scoping is applied HERE, once,
                // BEFORE the spawn: the turn service takes an already-scoped
                // id because prefixing is this host's policy, not the
                // runtime's, and computing it on this side keeps the tenant
                // out of the task entirely.
                let scoped = tr.scoped_id(&conversation_id);
                let (rt, st, ntx, otx) = (
                    Arc::clone(&tr.runtime),
                    Arc::clone(&tr.store),
                    narration_tx.clone(),
                    out_tx.clone(),
                );
                in_flight = Some(tokio::spawn(async move {
                    // Moved, not borrowed: the permit drops when the turn ends.
                    let _permit = permit;
                    serve_turn(&rt, st.as_ref(), &scoped, &content, Some(&ntx), &otx).await;
                }));
            }
            TurnRequest::Approve {
                task_id,
                step_id,
                approved,
            } => {
                let key = format!("{task_id}:{step_id}");
                approval.submit_approval(&key, approved).await;
            }
            TurnRequest::UserReply { task_id, content } => {
                let key = format!("{task_id}:input");
                approval.submit_input(&key, content).await;
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
