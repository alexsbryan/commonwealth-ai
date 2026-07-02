// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

use sovereign_core::runtime::Runtime;
use sovereign_core::types::TurnNarration;

use crate::approval::{ServerApprovalChannel, ServerEvent};
use crate::auth::TenantId;
use crate::projection::project_message_metadata;
use crate::reciprocity::{user_key, ReciprocityTable};
use crate::scheduler::{FairScheduler, UserKey};
use crate::tenant::TenantRuntime;

/// Client → Server WebSocket messages.
#[derive(serde::Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
enum ClientEvent {
    Message {
        content: String,
    },
    Approve {
        task_id: String,
        step_id: usize,
        approved: bool,
    },
    UserReply {
        task_id: String,
        content: String,
    },
}

/// WebSocket upgrade handler for /v1/conversations/:id/stream
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(runtime): Extension<Arc<Runtime>>,
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
    tenant: TenantId,
    approval: Arc<ServerApprovalChannel>,
    sched: FairScheduler,
    reciprocity: Arc<ReciprocityTable>,
    key: UserKey,
    narration_tx: broadcast::Sender<TurnNarration>,
    conversation_id: String,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let tr = TenantRuntime::new(Arc::clone(&runtime), tenant.0.clone());

    // Single writer to the socket. A forwarder task drains two sources:
    //   (a) this connection's per-turn token stream, via `out_rx`, and
    //   (b) the shared approval/step broadcast (approval + user-input).
    // Streaming tokens go ONLY through `out_tx` — never the broadcast —
    // because the broadcast fans to every connected socket and would
    // leak one tenant's tokens onto another client's stream.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let mut broadcast_rx = approval.subscribe();

    let tx_handle = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                Some(ev) = out_rx.recv() => ev,
                ev = broadcast_rx.recv() => match ev {
                    Ok(ev) => ev,
                    // Slow consumer dropped some broadcast events — skip
                    // and keep going; tokens (on out_rx) are unaffected.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                },
                else => break,
            };
            let json = match serde_json::to_string(&event) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break; // client disconnected
            }
        }
    });

    // Process incoming messages from the WebSocket client.
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        let event: ClientEvent = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Invalid WebSocket message: {e}");
                continue;
            }
        };

        match event {
            ClientEvent::Message { content } => {
                // Fair scheduler — admit this turn, streaming live queue
                // position to the client while it waits. The permit is held
                // for the streamed turn and dropped at the end of this arm
                // (one in-flight turn per socket). On shed, surface the busy
                // state and skip the turn (never an unbounded hang).
                let weight = reciprocity.weight_for(&key);
                let pos_tx = out_tx.clone();
                let admit = sched
                    .admit(key.clone(), weight, move |status| {
                        let _ = pos_tx.send(ServerEvent::QueuePosition {
                            position: status.position,
                            estimated_wait_ms: status.estimated_wait_ms,
                        });
                    })
                    .await;
                let _permit = match admit {
                    Ok(permit) => permit,
                    Err(shed) => {
                        tracing::warn!(
                            conversation_id = %conversation_id,
                            available = sched.available(),
                            would_be_position = shed.would_be_position,
                            "host_busy: shedding WS turn"
                        );
                        let _ = out_tx.send(ServerEvent::StreamError {
                            message: "host busy".to_string(),
                            retry_after_secs: Some(shed.retry_after_secs),
                        });
                        continue;
                    }
                };
                approval.set_task_id(&conversation_id).await;
                // Inline (not spawned): a chat turn streams to completion and
                // v1 has no mid-turn client input (approvals are out of
                // scope). Holding the receive loop here also keeps the permit
                // scoped to exactly one in-flight turn.
                stream_turn(&tr, &conversation_id, &content, &out_tx, &narration_tx).await;
            }
            ClientEvent::Approve {
                task_id,
                step_id,
                approved,
            } => {
                let key = format!("{task_id}:{step_id}");
                approval.submit_approval(&key, approved).await;
            }
            ClientEvent::UserReply { task_id, content } => {
                let key = format!("{task_id}:input");
                approval.submit_input(&key, content).await;
            }
        }
    }

    tx_handle.abort();
}

/// Drive one streamed turn: open the runtime stream, forward each token
/// delta as a `Token` frame AND each turn-matched narration as a
/// `Narration` frame (the glassbox progress handles), then re-read the
/// persisted message and emit a terminal `Complete` frame carrying
/// projected provenance + citations. Mirrors the desktop
/// `commands/chat.rs` consume-then-reread pattern, plus the desktop's
/// live progress narration.
/// Forward one turn-matched narration frame to the client. Returns
/// `false` only when the broadcast has closed (so the caller stops
/// selecting on it — in practice the sink lives on the Runtime and never
/// closes). The broadcast fans every tenant's events to every subscriber,
/// so we drop frames whose `conversation_id` isn't this turn's.
fn forward_narration(
    narration: Result<TurnNarration, RecvError>,
    scoped: &str,
    message_id: &str,
    out_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> bool {
    match narration {
        Ok(n) if n.conversation_id == scoped => {
            let phase = serde_json::to_value(&n.event.phase).unwrap_or(serde_json::Value::Null);
            let _ = out_tx.send(ServerEvent::Narration {
                message_id: message_id.to_string(),
                phase,
                text: n.event.text,
                elapsed_ms: n.event.elapsed_ms,
            });
            true
        }
        Ok(_) => true,                     // another tenant's turn
        Err(RecvError::Lagged(_)) => true, // dropped a frame; tokens unaffected
        Err(RecvError::Closed) => false,   // sink gone — stop selecting
    }
}

async fn stream_turn(
    tr: &TenantRuntime,
    conversation_id: &str,
    content: &str,
    out_tx: &mpsc::UnboundedSender<ServerEvent>,
    narration_tx: &broadcast::Sender<TurnNarration>,
) {
    // The runtime tags narration with the SCOPED conversation id (what it
    // was handed). Subscribe BEFORE starting the turn so nothing is lost.
    let scoped = tr.scoped_id(conversation_id);
    let mut narration_rx = narration_tx.subscribe();
    // Disables the narration select branch once the broadcast closes so a
    // closed channel can't spin a loop.
    let mut narration_open = true;

    // Acquire the stream handle while CONCURRENTLY forwarding narration.
    // The heavy pre-stream work (routing, retrieval — the bulk of a cold
    // turn's wait) runs INSIDE this await, and that's exactly when the
    // user is waiting on a blank screen. Forwarding those stage frames
    // live here — rather than after the await returns — is what makes the
    // progress glassbox useful. `message_id` isn't known yet, so send it
    // empty; the client shows one turn's progress at a time.
    let handle_result = {
        let acquire = tr.handle_message_stream(content, conversation_id);
        tokio::pin!(acquire);
        loop {
            tokio::select! {
                h = &mut acquire => break h,
                narration = narration_rx.recv(), if narration_open => {
                    narration_open = forward_narration(narration, &scoped, "", out_tx);
                }
            }
        }
    };
    let handle = match handle_result {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "WS stream start failed");
            let _ = out_tx.send(ServerEvent::StreamError {
                message: e.to_string(),
                retry_after_secs: None,
            });
            return;
        }
    };

    let message_id = handle.message_id.clone();
    let mut stream = handle.stream;
    loop {
        tokio::select! {
            item = stream.next() => match item {
                Some(Ok(delta)) => {
                    if out_tx
                        .send(ServerEvent::Token {
                            message_id: message_id.clone(),
                            chunk: delta,
                        })
                        .is_err()
                    {
                        return; // forwarder gone (client disconnected)
                    }
                }
                Some(Err(e)) => {
                    tracing::error!(error = %e, "WS stream item error");
                    let _ = out_tx.send(ServerEvent::StreamError {
                        message: e.to_string(),
                        retry_after_secs: None,
                    });
                    return;
                }
                None => break, // stream exhausted
            },
            narration = narration_rx.recv(), if narration_open => {
                narration_open = forward_narration(narration, &scoped, &message_id, out_tx);
            },
        }
    }

    // Stream exhausted → the runtime has persisted the assistant message
    // and its metadata. Project provenance + citations for the terminal
    // frame. Absent metadata (handlers that don't persist it) degrades to
    // `(None, [])` — the client still gets a well-formed Complete frame.
    let metadata = tr.message_metadata(conversation_id, &message_id).await;
    let (provenance, citations) = project_message_metadata(&metadata);
    let _ = out_tx.send(ServerEvent::Complete {
        message_id,
        provenance,
        citations,
    });
}
