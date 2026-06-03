use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Extension, Path, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};

use sovereign_core::runtime::Runtime;

use crate::approval::ServerApprovalChannel;
use crate::auth::TenantId;
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

// Server → Client traffic flows through `ServerEvent` over the approval
// channel's broadcast subscription (see `handle_ws` below). There is no
// separate `WsResponse` envelope; the broadcast already carries everything
// the client needs to render.

/// WebSocket upgrade handler for /v1/conversations/:id/stream
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Extension(approval): Extension<Arc<ServerApprovalChannel>>,
    Path(conversation_id): Path<String>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, runtime, tenant, approval, conversation_id))
}

async fn handle_ws(
    socket: WebSocket,
    runtime: Arc<Runtime>,
    tenant: TenantId,
    approval: Arc<ServerApprovalChannel>,
    conversation_id: String,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let tr = TenantRuntime::new(Arc::clone(&runtime), tenant.0.clone());

    // Subscribe to server events for this connection.
    let mut event_rx = approval.subscribe();

    // Spawn a task to forward server events to the WebSocket client.
    let tx_handle = tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let json = match serde_json::to_string(&event) {
                Ok(j) => j,
                Err(_) => continue,
            };
            if ws_tx.send(Message::Text(json.into())).await.is_err() {
                break;
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
                approval.set_task_id(&conversation_id).await;

                match tr.handle_message(&content, &conversation_id).await {
                    Ok(response) => {
                        // Final assistant message is delivered via the
                        // broadcast subscription as a `ServerEvent`. We log
                        // here only for server-side debugging.
                        tracing::info!(
                            message_id = %response.message.id,
                            chars = response.message.content.len(),
                            "WS message handled"
                        );
                    }
                    Err(e) => {
                        tracing::error!("WS message handling error: {e}");
                    }
                }
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
