// SPDX-License-Identifier: AGPL-3.0-or-later
//! The turn, on the phone — driven through `sovereign-turn-client`.
//!
//! # What changed, and why it is the whole point of sv-surface R6
//!
//! This module used to open its own WebSocket, hand-write
//! `{"type":"message","data":{"content":…}}` onto it, and match the reply
//! against a four-variant `ServerEvent` copied out of `sovereign-server`.
//! That copy was the LAST out-of-process consumer of the wire that was not
//! the client family (`sv-one-client`), and being a copy it could only ever
//! lose: it had no `Prompt`, no `Notice`, no `QueuePosition`, and a
//! `#[serde(other)] Ignored` arm that made every frame it did not know look
//! identical to a frame that did not exist.
//!
//! Now every frame this file handles is a `TurnFrame` from
//! `sovereign-contracts`, read off a `TurnStream`, and every request it
//! sends is a `TurnRequest` built by a `TurnSender`. Mobile's parity with
//! the desktop and the CLI is therefore BY CONSTRUCTION — the three
//! surfaces cannot diverge on the protocol, because only one of them
//! spells it.
//!
//! # The two moments, not one
//!
//! `Complete` ends the TURN. `Notice::TurnSettled` ends the HOST'S TALKING
//! — `MessageRefined` and `LessonProposed` fire after `Complete` from a
//! detached spawn (protocol note E1). So the drive is two phases: read to
//! `Complete`, persist, emit `message-complete`; then
//! [`TurnStream::drain_after_complete`] until the host settles, emitting
//! each late notice. A client that stopped at `Complete` would leave the UI
//! stuck on "Refining your answer" forever, which is the repro G13 names.
//!
//! # Emission is behind a trait
//!
//! [`TurnEvents`] exists so the drive can be tested against a real socket
//! without a Tauri application. `AppHandle` implements it in the app;
//! `tests/turn_wire.rs` implements it with a recorder and asserts on what
//! the WebView would have received.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use sovereign_turn_client::{
    StreamOptions, TurnAnswer, TurnClient, TurnFrame, TurnMode, TurnNotice, TurnObserver,
    TurnPrompt, TurnSender, TurnStream,
};

use crate::cache::store;
use crate::error::{Error, Result};
use crate::remote::dto::MessageDto;
use crate::remote::map::metadata_blob;

/// Where a driven turn's events go.
///
/// One method, taking the already-built JSON: the Tauri event names and
/// their payload shapes are the contract the shared `@sovereign/chat-ui`
/// FSM consumes, and they are spelled ONCE below rather than once per
/// implementation (ARCH §10.6).
pub trait TurnEvents: Send + Sync {
    /// Emit one event to the WebView. Best-effort by contract: a dropped
    /// event must never fail the turn.
    fn emit_json(&self, event: &str, payload: serde_json::Value);
}

impl TurnEvents for AppHandle {
    fn emit_json(&self, event: &str, payload: serde_json::Value) {
        let _ = self.emit(event, payload);
    }
}

/// The live write halves, one per conversation with a turn in flight.
///
/// A `TurnSender` is the R2/G11 split: it puts an `Answer` or a `Cancel` on
/// the wire from a DIFFERENT task than the one reading frames, which is
/// exactly the shape a Tauri command needs — `answer_prompt` runs on the
/// command thread while `run_stream` is parked in `next_frame`.
#[derive(Clone, Default)]
pub struct SenderRegistry {
    inner: Arc<Mutex<HashMap<String, TurnSender>>>,
}

impl SenderRegistry {
    /// The sender for a conversation with a turn in flight, if any.
    pub fn get(&self, conversation_id: &str) -> Option<TurnSender> {
        self.inner
            .lock()
            .ok()
            .and_then(|m| m.get(conversation_id).cloned())
    }

    fn insert(&self, conversation_id: &str, tx: TurnSender) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(conversation_id.to_string(), tx);
        }
    }

    fn remove(&self, conversation_id: &str) {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(conversation_id);
        }
    }
}

/// How to drive one turn. Grouped into a struct because the drive takes
/// six things and a positional call of six was already unreadable.
pub struct TurnRun {
    /// Host root, e.g. `http://host.tailnet:8080` — no `/v1`.
    pub base_url: String,
    /// The conversation the turn belongs to.
    pub conversation_id: String,
    /// What the user asked.
    pub content: String,
    /// How much of the pipeline the turn runs through.
    pub mode: TurnMode,
    /// Claim this conversation's approvals (`?approvals=true`).
    ///
    /// FALSE for the shipped app, deliberately. Claiming installs an
    /// obligation — a claimed socket whose client never answers turns the
    /// host's own auto-answer into a hang — and the mobile UI has no
    /// approval card yet. The answer path exists and is exercised by
    /// `tests/turn_wire.rs`; flip this the day the card ships, not before.
    pub claim_approvals: bool,
}

/// Drive one streamed turn end to end.
///
/// Returns once the host has SETTLED, not once the turn completed — see the
/// module header. `db` is locked only for short await-free sections.
pub async fn run_stream(
    events: Arc<dyn TurnEvents>,
    db: Arc<Mutex<Connection>>,
    senders: SenderRegistry,
    run: TurnRun,
) -> Result<()> {
    let client = TurnClient::new(run.base_url.clone());
    let mut stream = client
        .connect_with(
            &run.conversation_id,
            StreamOptions {
                claim_approvals: run.claim_approvals,
            },
        )
        .await
        .map_err(|e| Error::WebSocket(e.to_string()))?;

    senders.insert(&run.conversation_id, stream.sender());
    let result = drive(&events, &db, &mut stream, &run).await;
    senders.remove(&run.conversation_id);
    result
}

/// The frame loop. Split out so the registry is cleaned up on every exit
/// path, including the error ones.
async fn drive(
    events: &Arc<dyn TurnEvents>,
    db: &Arc<Mutex<Connection>>,
    stream: &mut TurnStream,
    run: &TurnRun,
) -> Result<()> {
    stream
        .send_message(&run.content, run.mode, None)
        .await
        .map_err(|e| Error::WebSocket(e.to_string()))?;

    let conv = run.conversation_id.as_str();
    let mut full = String::new();
    let mut current_message_id: Option<String> = None;

    // Why a read ERROR and a clean hangup share one exit: both mean the turn
    // was dropped before a terminal frame, and the phone owes the same two
    // things either way — mark the message for re-fetch, tell the view. iOS
    // suspending a backgrounded socket produces the ERROR one ("connection
    // reset without closing handshake"), which is the common case, and
    // routing it to `Err` instead left the message NOT marked `streaming`,
    // so reconnect never re-fetched it (HANDOFF §8's open gap).
    let mut dropped: Option<String> = None;

    loop {
        let frame = match stream.next_frame().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    target: "mobile::turn",
                    conversation_id = conv,
                    error = %e,
                    "turn socket died before a terminal frame"
                );
                dropped = Some(e.to_string());
                break;
            }
        };

        // Exhaustive, with no catch-all: a variant added to the protocol
        // must be answered for here rather than silently dropped, which is
        // precisely what the deleted mirror's `#[serde(other)] Ignored`
        // made impossible (ARCH §18.3).
        match frame {
            TurnFrame::Token { message_id, chunk } => {
                // The desktop learns the assistant message id up front
                // (StreamHandle.message_id) and sends SEND_START before any
                // chunk. Over the wire the id arrives on `Notice::
                // TurnStarted` when the host emits one, and otherwise on the
                // first `Token` — either way `message-start` must precede the
                // first `message-chunk`, whose FSM guard requires it.
                if current_message_id.is_none() {
                    events.emit_json(
                        "message-start",
                        json!({ "conversation_id": conv, "message_id": message_id }),
                    );
                }
                current_message_id = Some(message_id.clone());
                full.push_str(&chunk);
                events.emit_json(
                    "message-chunk",
                    json!({
                        "conversation_id": conv,
                        "message_id": message_id,
                        "chunk": chunk,
                    }),
                );
            }

            TurnFrame::Narration {
                message_id,
                phase,
                text,
                elapsed_ms,
            } => {
                events.emit_json(
                    "message-narration",
                    json!({
                        "conversation_id": conv,
                        "message_id": message_id,
                        "phase": phase,
                        "text": text,
                        "elapsed_ms": elapsed_ms,
                    }),
                );
            }

            // Queue depth: a capability the phone did not have at all,
            // because the deleted mirror had no variant for it. The busy
            // host was legible only as a REST 503 (acceptance §6); a queued
            // turn now says its own place in line.
            TurnFrame::QueuePosition {
                position,
                estimated_wait_ms,
            } => {
                events.emit_json(
                    "turn-queued",
                    json!({
                        "conversation_id": conv,
                        "position": position,
                        "estimated_wait_ms": estimated_wait_ms,
                    }),
                );
            }

            TurnFrame::Prompt { id, prompt } => {
                events.emit_json(
                    "turn-prompt",
                    json!({
                        "conversation_id": conv,
                        "id": id,
                        "kind": prompt_kind(&prompt),
                        "prompt": prompt,
                    }),
                );
            }

            TurnFrame::Notice { notice } => {
                emit_notice(events.as_ref(), conv, &notice);
            }

            TurnFrame::Complete {
                message_id,
                provenance,
                citations,
                epistemic_state,
                task,
                metadata,
            } => {
                // Persist message + provenance + citations atomically so it
                // survives an immediate app kill (acceptance §3, §11).
                let m = MessageDto {
                    id: message_id.clone(),
                    conversation_id: conv.to_string(),
                    role: "assistant".into(),
                    content: full.clone(),
                    status: Some("complete".into()),
                    created_at: 0,
                    server_version: None,
                    provenance: provenance.clone(),
                    citations: citations.clone(),
                    // The live event below carries the blob; the cached row
                    // stores provenance/citations and rebuilds it on hydrate
                    // (commands::conversation::attach_metadata).
                    metadata: None,
                };
                if let Ok(mut conn) = db.lock() {
                    let _ =
                        store::upsert_message_full(&mut conn, &m, provenance.as_ref(), &citations);
                }
                let mut blob = metadata_blob(provenance.as_ref(), &citations);
                // Three facts the wire carries that the mirror had no field
                // for. Attached rather than dropped: a client that cannot
                // see the epistemic ledger renders a confident answer and a
                // hedged one identically.
                if let Some(obj) = blob.as_object_mut() {
                    if let Some(es) = &epistemic_state {
                        obj.insert("epistemic_state".into(), json!(es));
                    }
                    if let Some(t) = &task {
                        obj.insert("task".into(), json!(t));
                    }
                    if let Some(md) = &metadata {
                        obj.insert("turn_metadata".into(), json!(md));
                    }
                }
                events.emit_json(
                    "message-complete",
                    json!({
                        "conversation_id": conv,
                        "message_id": message_id,
                        "full_text": full,
                        "metadata": blob,
                    }),
                );
                // The turn is over; the HOST is not necessarily done.
                return settle(events, stream, conv).await;
            }

            TurnFrame::StreamError {
                message,
                retry_after_secs,
            } => {
                events.emit_json(
                    "message-error",
                    json!({ "message": message, "retry_after_secs": retry_after_secs }),
                );
                return settle(events, stream, conv).await;
            }
        }
    }

    // The socket closed without a terminal frame (e.g. iOS suspended the
    // connection). Mark the in-flight message so reconnect re-fetches it.
    if let (Some(mid), Ok(conn)) = (current_message_id, db.lock()) {
        let _ = store::set_message_status(&conn, &mid, "streaming");
    }
    // The reason, not a generic line: "closed before completion" and
    // "connection reset" are different things to a user on a train, and the
    // banner can only say which if the drive passes it along (ARCH §18.3).
    events.emit_json(
        "message-error",
        json!({
            "message": dropped
                .unwrap_or_else(|| "stream closed before completion".to_string()),
            "retry_after_secs": serde_json::Value::Null,
        }),
    );
    Ok(())
}

/// Read the post-terminal window until the host says it is finished.
///
/// A failure here is NOT a failed turn — the answer is already rendered and
/// persisted — so it is reported as a notice-channel error and swallowed,
/// rather than turning a delivered answer into an `Err`.
async fn settle(
    events: &Arc<dyn TurnEvents>,
    stream: &mut TurnStream,
    conversation_id: &str,
) -> Result<()> {
    let sink = events.clone();
    let conv = conversation_id.to_string();
    let mut on_notice = move |n: &TurnNotice| emit_notice(sink.as_ref(), &conv, n);
    let mut observer = TurnObserver {
        on_notice: Some(&mut on_notice),
        ..Default::default()
    };
    if let Err(e) = stream.drain_after_complete(&mut observer).await {
        tracing::warn!(target: "mobile::turn", error = %e, "post-turn window ended badly");
        events.emit_json(
            "turn-notice-error",
            json!({ "conversation_id": conversation_id, "message": e.to_string() }),
        );
    }
    Ok(())
}

/// One notice → one `turn-notice` event, tagged with its variant name.
///
/// Exhaustive on purpose: the point of adopting the contract enum is that a
/// new notice becomes a compile error here instead of silent nothing.
fn emit_notice(events: &dyn TurnEvents, conversation_id: &str, notice: &TurnNotice) {
    let kind = match notice {
        TurnNotice::TurnStarted { .. } => "turn_started",
        TurnNotice::TurnSettled { .. } => "turn_settled",
        TurnNotice::StepDone { .. } => "step_done",
        TurnNotice::MessageRefined(_) => "message_refined",
        TurnNotice::LessonProposed(_) => "lesson_proposed",
        TurnNotice::ResolveAck { .. } => "resolve_ack",
        TurnNotice::InterpretationProposed(_) => "interpretation_proposed",
        TurnNotice::ClarificationRequest(_) => "clarification_request",
    };
    events.emit_json(
        "turn-notice",
        json!({ "conversation_id": conversation_id, "kind": kind, "notice": notice }),
    );
}

/// The prompt's variant name, so the WebView can pick a card without
/// re-deriving it from the payload's shape.
fn prompt_kind(prompt: &TurnPrompt) -> &'static str {
    match prompt {
        TurnPrompt::Approval { .. } => "approval",
        TurnPrompt::UserInput { .. } => "user_input",
        TurnPrompt::Information { .. } => "information",
    }
}

/// Answer a parked `TurnFrame::Prompt` — the wire form of the desktop's
/// three `submit_*` commands, which are one command here because
/// [`TurnAnswer`] is one type (protocol note: three key formats and three
/// submits collapse to one id and one answer).
///
/// Whether the answer RESOLVED anything comes back asynchronously as
/// `Notice::ResolveAck`, never as this function's return: a bool here could
/// not tell "accepted" from "never arrived" (ARCH §18.3).
pub fn answer_prompt(
    senders: &SenderRegistry,
    conversation_id: &str,
    id: &str,
    answer: &TurnAnswer,
) -> Result<()> {
    let tx = senders
        .get(conversation_id)
        .ok_or_else(|| Error::WebSocket(format!("no turn in flight on {conversation_id}")))?;
    tx.send_answer(id, answer)
        .map_err(|e| Error::WebSocket(e.to_string()))
}

/// Cancel the in-flight turn. The host trips the turn's own cancellation,
/// so it ends the way a cancelled turn always ended — a `Complete` carrying
/// `finish_reason: "cancelled"` — rather than a dead socket with no
/// terminal frame.
pub fn cancel_turn(senders: &SenderRegistry, conversation_id: &str) -> Result<()> {
    let tx = senders
        .get(conversation_id)
        .ok_or_else(|| Error::WebSocket(format!("no turn in flight on {conversation_id}")))?;
    tx.send_cancel()
        .map_err(|e| Error::WebSocket(e.to_string()))
}
