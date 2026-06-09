// SPDX-License-Identifier: AGPL-3.0-or-later
//! The streaming contract bridge. Opens the host WebSocket for one
//! turn, forwards each `Token` frame as a `message-chunk` Tauri event,
//! writes the completed message + provenance + citations to the cache
//! in one transaction, and emits `message-complete` with the mapped
//! `metadata` blob — the SAME event names + shape the desktop chat FSM
//! already consumes, so the shared UI renders mobile streams unchanged.
//!
//! On a dropped socket mid-stream the message is left `streaming` in the
//! cache and a `message-error` is emitted; reconnect re-fetches it via
//! `get_conversation` (handles iOS backgrounding suspending the socket).

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::cache::store;
use crate::error::{Error, Result};
use crate::remote::dto::{MessageDto, ServerEvent};
use crate::remote::map::metadata_blob;

#[derive(Serialize, Clone)]
struct ChunkPayload {
    conversation_id: String,
    message_id: String,
    chunk: String,
}

#[derive(Serialize, Clone)]
struct CompletePayload {
    conversation_id: String,
    message_id: String,
    full_text: String,
    metadata: serde_json::Value,
}

#[derive(Serialize, Clone)]
struct ErrorPayload {
    message: String,
    retry_after_secs: Option<u64>,
}

/// A glassbox progress signal forwarded to the webview as a
/// `message-narration` event while the turn is in flight.
#[derive(Serialize, Clone)]
struct NarrationPayload {
    conversation_id: String,
    message_id: String,
    phase: serde_json::Value,
    text: String,
    elapsed_ms: u64,
}

/// Drive one streamed turn end-to-end. `db` is the shared cache
/// connection; it is locked only briefly at completion (no lock is held
/// across an await).
pub async fn run_stream(
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    ws_url: String,
    token: String,
    conversation_id: String,
    content: String,
) -> Result<()> {
    let mut request = ws_url
        .into_client_request()
        .map_err(|e| Error::WebSocket(e.to_string()))?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| Error::WebSocket("bad auth header".into()))?,
    );

    let (ws, _resp) = connect_async(request)
        .await
        .map_err(|e| Error::WebSocket(e.to_string()))?;
    let (mut tx, mut rx) = ws.split();

    let send = serde_json::json!({ "type": "message", "data": { "content": content } }).to_string();
    tx.send(Message::Text(send.into()))
        .await
        .map_err(|e| Error::WebSocket(e.to_string()))?;

    let mut full = String::new();
    let mut current_message_id: Option<String> = None;

    while let Some(frame) = rx.next().await {
        let frame = frame.map_err(|e| Error::WebSocket(e.to_string()))?;
        let text = match frame {
            Message::Text(t) => t.as_str().to_owned(),
            Message::Close(_) => break,
            _ => continue,
        };
        match serde_json::from_str::<ServerEvent>(&text) {
            Ok(ServerEvent::Token { message_id, chunk }) => {
                // The host assigns the assistant message id; the desktop
                // path learns it up front (StreamHandle.message_id) and
                // sends SEND_START before chunks. Over WS we learn it
                // from the first Token, so emit `message-start` here so
                // the FSM creates the streaming placeholder before the
                // first MESSAGE_CHUNK (whose guard requires it).
                if current_message_id.is_none() {
                    let _ = app.emit(
                        "message-start",
                        serde_json::json!({
                            "conversation_id": conversation_id,
                            "message_id": message_id,
                        }),
                    );
                }
                current_message_id = Some(message_id.clone());
                full.push_str(&chunk);
                let _ = app.emit(
                    "message-chunk",
                    ChunkPayload {
                        conversation_id: conversation_id.clone(),
                        message_id,
                        chunk,
                    },
                );
            }
            Ok(ServerEvent::Complete {
                message_id,
                provenance,
                citations,
            }) => {
                // Persist message + provenance + citations atomically so
                // it survives an immediate app kill (criteria 3, 9).
                let m = MessageDto {
                    id: message_id.clone(),
                    conversation_id: conversation_id.clone(),
                    role: "assistant".into(),
                    content: full.clone(),
                    status: Some("complete".into()),
                    created_at: 0,
                    server_version: None,
                    provenance: provenance.clone(),
                    citations: citations.clone(),
                    // The live event below carries the blob; the cached
                    // row stores provenance/citations and rebuilds it on
                    // hydrate (see commands::conversation::attach_metadata).
                    metadata: None,
                };
                if let Ok(mut conn) = db.lock() {
                    let _ = store::upsert_message_full(
                        &mut conn,
                        &m,
                        provenance.as_ref(),
                        &citations,
                    );
                }
                let metadata = metadata_blob(provenance.as_ref(), &citations);
                let _ = app.emit(
                    "message-complete",
                    CompletePayload {
                        conversation_id: conversation_id.clone(),
                        message_id,
                        full_text: full.clone(),
                        metadata,
                    },
                );
                return Ok(());
            }
            Ok(ServerEvent::StreamError {
                message,
                retry_after_secs,
            }) => {
                let _ = app.emit("message-error", ErrorPayload { message, retry_after_secs });
                return Ok(());
            }
            Ok(ServerEvent::Narration {
                message_id,
                phase,
                text,
                elapsed_ms,
            }) => {
                // Live progress for the in-flight turn — forwarded to the
                // ChatScreen narration stack. Best-effort; never persisted.
                let _ = app.emit(
                    "message-narration",
                    NarrationPayload {
                        conversation_id: conversation_id.clone(),
                        message_id,
                        phase,
                        text,
                        elapsed_ms,
                    },
                );
            }
            _ => {}
        }
    }

    // Socket closed without a terminal frame (e.g. iOS suspended the
    // connection). Mark the in-flight message so reconnect re-fetches it.
    if let (Some(mid), Ok(conn)) = (current_message_id, db.lock()) {
        let _ = store::set_message_status(&conn, &mid, "streaming");
    }
    let _ = app.emit(
        "message-error",
        ErrorPayload {
            message: "stream closed before completion".into(),
            retry_after_secs: None,
        },
    );
    Ok(())
}
