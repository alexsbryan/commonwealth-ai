// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streaming chat command. Mirrors desktop's `send_message_stream`:
//! returns immediately with the (server-assigned) stream handle while a
//! background task drives the WebSocket and emits `message-chunk` /
//! `message-complete` / `message-error` — the events the shared chat
//! FSM consumes.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::error::{Error, Result};
use crate::remote::stream;
use crate::state::AppState;

#[derive(Serialize)]
pub struct StreamStarted {
    pub conversation_id: String,
}

#[tauri::command]
pub async fn send_message_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    message: String,
) -> Result<StreamStarted> {
    let client = state.active_client().await?;
    let ws_url = client.ws_url(&conversation_id);
    let token = client.token().to_string();
    let db = state.db.clone();
    let conv = conversation_id.clone();

    // Drive the stream in the background; the WS handler emits the
    // chunk/complete/error events the frontend listens for. A start
    // error (e.g. busy host) is surfaced as a `message-error` by the
    // stream task, and also returned here for the caller's await.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = stream::run_stream(app2.clone(), db, ws_url, token, conv.clone(), message).await
        {
            use tauri::Emitter;
            let retry = match &e {
                Error::HostBusy { retry_after_secs } => Some(*retry_after_secs),
                _ => None,
            };
            let _ = app2.emit(
                "message-error",
                serde_json::json!({ "message": e.to_string(), "retry_after_secs": retry }),
            );
        }
    });

    Ok(StreamStarted { conversation_id })
}
