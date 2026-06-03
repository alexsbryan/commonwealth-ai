use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use sovereign_core::runtime::Runtime;

use crate::approval::{ServerApprovalChannel, ServerEvent};
use crate::auth::TenantId;
use crate::busy::BusyGuard;
use crate::projection::project_message_metadata;
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
    Extension(busy): Extension<BusyGuard>,
    Path(conversation_id): Path<String>,
) -> Response {
    ws.on_upgrade(move |socket| {
        handle_ws(socket, runtime, tenant, approval, busy, conversation_id)
    })
}

async fn handle_ws(
    socket: WebSocket,
    runtime: Arc<Runtime>,
    tenant: TenantId,
    approval: Arc<ServerApprovalChannel>,
    busy: BusyGuard,
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
                // Busy guard — same semantics as the REST path. The
                // permit is held for the duration of the streamed turn
                // and dropped at the end of this arm.
                let Some(_permit) = busy.try_enter() else {
                    tracing::warn!(
                        conversation_id = %conversation_id,
                        available = busy.available(),
                        "host_busy: rejecting WS turn"
                    );
                    let _ = out_tx.send(ServerEvent::StreamError {
                        message: "host busy".to_string(),
                        retry_after_secs: Some(busy.retry_after_secs()),
                    });
                    continue;
                };
                approval.set_task_id(&conversation_id).await;
                // Inline (not spawned): a chat turn streams to completion
                // and v1 has no mid-turn client input (approvals are out
                // of scope). Holding the receive loop here also keeps the
                // busy permit scoped to exactly one in-flight turn.
                stream_turn(&tr, &conversation_id, &content, &out_tx).await;
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
/// delta as a `Token` frame, then re-read the persisted message and emit
/// a terminal `Complete` frame carrying projected provenance + citations.
/// Mirrors the desktop `commands/chat.rs` consume-then-reread pattern.
async fn stream_turn(
    tr: &TenantRuntime,
    conversation_id: &str,
    content: &str,
    out_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let handle = match tr.handle_message_stream(content, conversation_id).await {
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
    while let Some(item) = stream.next().await {
        match item {
            Ok(delta) => {
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
            Err(e) => {
                tracing::error!(error = %e, "WS stream item error");
                let _ = out_tx.send(ServerEvent::StreamError {
                    message: e.to_string(),
                    retry_after_secs: None,
                });
                return;
            }
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
