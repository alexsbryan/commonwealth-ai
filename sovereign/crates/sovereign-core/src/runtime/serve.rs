// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serving one turn: drive the stream, forward the narration, emit the
//! terminal metadata frame.
//!
//! # Why this is a library function and not a route handler
//!
//! Until 2026-08-25 this loop existed once, inline in the `sovereign-server`
//! binary's WebSocket handler, and the other hosts each carried a partial
//! re-derivation of its tail: desktop `commands/chat.rs` (three sites), CLI
//! `chat_cmd/ask.rs` and `chat_cmd/session.rs`. All of them consume the
//! stream, then go and find the message they just produced in order to learn
//! what the turn actually did — six implementations of one decision (ARCH
//! §10.6).
//!
//! Duplication is the smaller half of the problem. The larger half is that
//! "go and find it in the store" only works from *inside* the process that
//! owns the store, so a turn's result could not cross a process boundary at
//! all — which is the thing `TOPOLOGY.md §3.5` requires, where the daemon
//! serves and every host is a surface. [`serve_turn`] is that seam: it takes
//! a [`Runtime`] and a store and emits `TurnFrame`s, which are values that
//! serialize. Who receives them — an in-process channel today, a socket
//! tomorrow — is the sink's business.
//!
//! # What it does not decide
//!
//! Tenancy. `conversation_id` arrives ALREADY SCOPED: prefixing is a host
//! policy (`sovereign-server`'s `TenantRuntime`), and a turn service that
//! re-applied it would be a second implementation of the same rule. It was
//! applied three times per turn before this — once inside
//! `handle_message_stream`, once for narration matching, once for the
//! metadata re-read — and one disagreement between them is a cross-tenant
//! read.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

use sovereign_contracts::types::projection::{project_epistemic_state, project_message_metadata};
use sovereign_contracts::types::{TurnFrame, TurnNarration};

use crate::runtime::Runtime;
use crate::traits::StateStore;

/// Where a served turn's frames go.
///
/// One method, because a turn only ever does one thing to a sink. A
/// host implements this over whatever carries frames to its client: an
/// in-process channel, a WebSocket, an SSE body, a test collector.
///
/// Infallible on purpose. A sink whose receiver has gone away has nothing
/// to tell the turn — the turn is already running and cancelling it is the
/// caller's decision, not the sink's — so `emit` swallows that rather than
/// making every emit site handle an error it cannot act on.
pub trait TurnSink: Send + Sync {
    /// Deliver one frame. Best-effort.
    fn emit(&self, frame: TurnFrame);
}

impl TurnSink for mpsc::UnboundedSender<TurnFrame> {
    fn emit(&self, frame: TurnFrame) {
        let _ = self.send(frame);
    }
}

impl<T: TurnSink + ?Sized> TurnSink for Arc<T> {
    fn emit(&self, frame: TurnFrame) {
        (**self).emit(frame);
    }
}

/// The persisted `metadata` blob for one message, read back after its turn
/// has completed.
///
/// THE implementation of that lookup (ARCH §10.6). `None` covers every way
/// it can be absent — conversation missing, message missing, message carries
/// no metadata — because the projection layer treats all three identically
/// and a caller that distinguished them would have nothing different to do.
///
/// The read is deliberate rather than incidental: the runtime persists the
/// assistant message as the last act of the stream, so this is how a caller
/// learns what the turn concluded. Callers OUTSIDE the process that owns the
/// store cannot use it, which is why [`serve_turn`] projects the result into
/// a `TurnFrame::Complete` instead of expecting every host to come and look.
pub async fn message_metadata(
    store: &dyn StateStore,
    conversation_id: &str,
    message_id: &str,
) -> Option<Value> {
    let convo = store.get_conversation(conversation_id).await.ok()?;
    convo
        .messages
        .into_iter()
        .find(|m| m.id == message_id)
        .and_then(|m| m.metadata)
}

/// Forward one turn-matched narration frame. Returns `false` only when the
/// broadcast has closed, so the caller stops selecting on it (in practice
/// the sink lives on the `Runtime` and never closes).
///
/// The broadcast fans every conversation's events to every subscriber, so
/// frames belonging to another turn are dropped here. That filter is why
/// `conversation_id` must be the same string the runtime was handed.
fn forward_narration(
    narration: Result<TurnNarration, RecvError>,
    conversation_id: &str,
    message_id: &str,
    sink: &dyn TurnSink,
) -> bool {
    match narration {
        Ok(n) if n.conversation_id == conversation_id => {
            sink.emit(TurnFrame::Narration {
                message_id: message_id.to_string(),
                phase: n.event.phase,
                text: n.event.text,
                elapsed_ms: n.event.elapsed_ms,
            });
            true
        }
        Ok(_) => true,                     // another conversation's turn
        Err(RecvError::Lagged(_)) => true, // dropped a frame; tokens unaffected
        Err(RecvError::Closed) => false,   // sink gone — stop selecting
    }
}

/// Drive one streamed turn to completion, emitting every frame a client
/// needs to render it: live narration while the host works, a `Token` per
/// delta, and a terminal `Complete` carrying the turn's projected
/// provenance, citations and epistemic ledger.
///
/// `conversation_id` must ALREADY be scoped the way the host scopes it (see
/// the module docs). `narration` is the runtime's narration broadcast when
/// the host installed one; `None` simply means no progress frames.
///
/// Emits `StreamError` and returns early on either failure mode — the turn
/// failing to start, and the stream erroring mid-flight. Both are terminal
/// for this turn; neither is silently swallowed, because a client that
/// received tokens and then nothing cannot tell a finished turn from a dead
/// one (ARCH §18.3).
pub async fn serve_turn(
    runtime: &Runtime,
    store: &dyn StateStore,
    conversation_id: &str,
    content: &str,
    narration: Option<&broadcast::Sender<TurnNarration>>,
    sink: &dyn TurnSink,
) {
    use futures::StreamExt;

    // Subscribe BEFORE starting the turn so nothing emitted during the
    // pre-stream work is lost.
    let mut narration_rx = narration.map(|tx| tx.subscribe());

    // Acquire the stream handle while CONCURRENTLY forwarding narration.
    // The heavy pre-stream work (routing, retrieval — the bulk of a cold
    // turn's wait) runs INSIDE this await, and that is exactly when the user
    // is waiting on a blank screen. Forwarding those stage frames live here,
    // rather than after the await returns, is what makes the progress
    // glassbox useful. `message_id` is not known yet, so it goes out empty;
    // the client shows one turn's progress at a time.
    let handle_result = {
        let acquire = runtime.handle_message_stream(content, conversation_id);
        tokio::pin!(acquire);
        loop {
            tokio::select! {
                h = &mut acquire => break h,
                narration = async {
                    match narration_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        // No narration channel: never resolve, so the select
                        // waits on the turn alone.
                        None => std::future::pending().await,
                    }
                } => {
                    if !forward_narration(narration, conversation_id, "", sink) {
                        narration_rx = None;
                    }
                }
            }
        }
    };

    let handle = match handle_result {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(
                conversation_id = %conversation_id,
                error = %e,
                "serve_turn: stream start failed"
            );
            sink.emit(TurnFrame::StreamError {
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
                Some(Ok(delta)) => sink.emit(TurnFrame::Token {
                    message_id: message_id.clone(),
                    chunk: delta,
                }),
                Some(Err(e)) => {
                    tracing::error!(
                        conversation_id = %conversation_id,
                        error = %e,
                        "serve_turn: stream item error"
                    );
                    sink.emit(TurnFrame::StreamError {
                        message: e.to_string(),
                        retry_after_secs: None,
                    });
                    return;
                }
                None => break, // stream exhausted
            },
            narration = async {
                match narration_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if !forward_narration(narration, conversation_id, &message_id, sink) {
                    narration_rx = None;
                }
            },
        }
    }

    // Stream exhausted → the runtime has persisted the assistant message and
    // its metadata. Project it for the terminal frame. Absent metadata
    // (handlers that don't persist any) degrades to `(None, [])` — the client
    // still gets a well-formed `Complete`, which is what tells it the turn
    // ended rather than died.
    let metadata = message_metadata(store, conversation_id, &message_id).await;
    let (provenance, citations) = project_message_metadata(&metadata);
    let epistemic_state = project_epistemic_state(&metadata);
    sink.emit(TurnFrame::Complete {
        message_id,
        provenance,
        citations,
        epistemic_state,
    });
}
