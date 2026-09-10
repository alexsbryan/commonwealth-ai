// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat surface — three commands over one turn socket.
//!
//! `send_message_stream` mirrors the desktop's command of the same name:
//! it returns immediately while a background task drives the turn and emits
//! the events the shared chat FSM consumes. `answer_prompt` and
//! `cancel_turn` are the write half of that same socket
//! ([`sovereign_turn_client::TurnSender`], the R2/G11 split) — they run on
//! the command thread while the drive task is parked reading frames, which
//! is the shape that split exists for.
//!
//! `answer_prompt` is ONE command where the desktop has three
//! (`submit_approval` / `submit_input` / `submit_information_response`),
//! because [`TurnAnswer`] is one closed enum and the prompt id is one
//! host-minted key. That collapse is the protocol's, not this file's.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

use sovereign_contracts::types::{TurnAnswer, TurnMode};

use crate::error::{Error, Result};
use crate::remote::stream::{self, TurnRun};
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
    let base_url = client.base_url().to_string();
    let db = state.db.clone();
    let senders = state.senders.clone();
    let conv = conversation_id.clone();

    // Drive the turn in the background; the drive emits the chunk /
    // complete / error / notice events the frontend listens for. A start
    // error (e.g. a busy host) is surfaced as `message-error` here so the
    // FSM leaves no bubble spinning.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let events: Arc<dyn stream::TurnEvents> = Arc::new(app2.clone());
        let run = TurnRun {
            base_url,
            conversation_id: conv,
            content: message,
            mode: TurnMode::Grounded,
            // See `TurnRun::claim_approvals`: the mobile UI has no approval
            // card yet, and claiming without one turns the host's own
            // auto-answer into a hang.
            claim_approvals: false,
        };
        if let Err(e) = stream::run_stream(Arc::clone(&events), db, senders, run).await {
            let retry = match &e {
                Error::HostBusy { retry_after_secs } => Some(*retry_after_secs),
                _ => None,
            };
            events.emit_json(
                "message-error",
                serde_json::json!({ "message": e.to_string(), "retry_after_secs": retry }),
            );
        }
    });

    Ok(StreamStarted { conversation_id })
}

/// Answer a parked `TurnFrame::Prompt`.
///
/// Returns `()`, not a bool: whether the answer resolved anything arrives
/// asynchronously as `Notice::ResolveAck`, because a bool here could not
/// tell "accepted" from "never arrived" (ARCH §18.3 — the reason
/// `ResolveOutcome` crossed the wire in the first place).
#[tauri::command]
pub async fn answer_prompt(
    state: State<'_, AppState>,
    conversation_id: String,
    id: String,
    answer: TurnAnswer,
) -> Result<()> {
    stream::answer_prompt(&state.senders, &conversation_id, &id, &answer)
}

/// Cancel the in-flight turn. The turn still ends with a `Complete` frame
/// (carrying `finish_reason: "cancelled"`), so the UI closes the bubble the
/// same way it closes any other.
#[tauri::command]
pub async fn cancel_turn(state: State<'_, AppState>, conversation_id: String) -> Result<()> {
    stream::cancel_turn(&state.senders, &conversation_id)
}
