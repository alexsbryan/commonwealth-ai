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

use sovereign_contracts::types::projection::{
    project_epistemic_state, project_message_metadata, project_task,
};
use sovereign_contracts::types::{Intent, TurnFrame, TurnMode, TurnNarration};

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

    /// The turn has a message id, and the answer has not started arriving.
    ///
    /// Fires at handle ACQUISITION — after routing and retrieval, before the
    /// first token. Defaulted to nothing because most sinks only care about
    /// frames, and it is not a frame: no client needs it on the wire, so
    /// putting it in [`TurnFrame`] would have widened the protocol to serve
    /// one in-process caller.
    ///
    /// That caller is the desktop. `send_message_stream` returns the message
    /// id to the frontend SYNCHRONOUSLY so the UI can put a placeholder on
    /// screen while retrieval runs — which is most of a cold turn's wait.
    /// Waiting for the first `Token` instead would hold that placeholder back
    /// by exactly the time it exists to cover. This hook is what let that
    /// command move onto [`serve_turn`] with no change to what the user sees.
    fn on_turn_started(&self, _message_id: &str) {}
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
///
/// # The two turns that do not token-stream, and why they are decided here
///
/// [`TurnMode::Naked`] runs the raw model; document-attached turns are owned
/// by the map-reduce document path and [`Runtime::handle_message_stream`]
/// refuses them outright. Both were host knowledge before phase 6, and every
/// host knew a different amount of it:
///
/// - `sovereign-cli-llm`'s chat surface matched the refusal's error **string**
///   and fell back to [`Runtime::handle_turn`].
/// - The eval harness matched the error **variant** and fell back to
///   [`Runtime::handle_message`].
/// - This function did neither: it emitted `StreamError` and the turn died,
///   so a document-attached question asked through the daemon got an error
///   where the same question asked in-process got an answer.
///
/// The first two are not interchangeable, which is the part that makes this a
/// correctness fix rather than a tidy-up: the streaming path persists the user
/// message *before* it bails, so falling back to `handle_message` writes the
/// user's turn to the conversation a second time. Deciding **before** the call
/// instead of catching an error after it removes the ambiguity — nothing has
/// been persisted yet, so `handle_message` is unambiguously the right handler
/// — and leaves one implementation where there were three (ARCH §10.6, §2.1).
///
/// The fallback is invisible to the client on purpose. It asked for a turn,
/// not for a streaming strategy: the answer arrives as one `Token` frame
/// followed by the same `Complete` every other turn ends with.
pub async fn serve_turn(
    runtime: &Runtime,
    store: &dyn StateStore,
    conversation_id: &str,
    content: &str,
    mode: TurnMode,
    intent: Option<Intent>,
    narration: Option<&broadcast::Sender<TurnNarration>>,
    sink: &dyn TurnSink,
) {
    use futures::StreamExt;

    // Decided up front, not discovered from an error. See the doc comment.
    if mode == TurnMode::Grounded && crate::runtime::is_document_attached(content) {
        serve_non_streaming_turn(runtime, store, conversation_id, content, sink).await;
        return;
    }

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
        let acquire = async {
            match mode {
                // A caller-pinned intent skips the router. Same turn, same
                // pipeline; the classification is supplied rather than
                // inferred.
                TurnMode::Grounded => match intent {
                    Some(intent) => {
                        runtime
                            .handle_message_stream_as(content, conversation_id, intent)
                            .await
                    }
                    None => runtime.handle_message_stream(content, conversation_id).await,
                },
                // Raw model: no retrieval, router, grounding gate, tools or
                // atlas. Reachable over the wire since phase 6 — before it,
                // only a host holding its own `Runtime` could ask for this.
                TurnMode::Naked => {
                    runtime
                        .handle_message_stream_naked(content, conversation_id)
                        .await
                }
            }
        };
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
        // The two GRACEFUL guards are not failures and must not be rendered
        // as any. An oversize paste and a contentless message ("?") both come
        // back as `InvalidInput` carrying a hint that is written to be shown
        // to the user verbatim — so they are answered, not errored.
        //
        // This lived in the desktop, which recognised both hints and emitted
        // them as a calm assistant turn "NOT a raw 'Error: Invalid input:'
        // bubble that reads as a crash". Every other host rendered the same
        // two cases as a crash, because the decision sat in one host instead
        // of in the driver. It is here now, so a surface gets the courteous
        // behaviour by connecting rather than by re-deriving it (ARCH §10.6).
        Err(crate::error::Error::InvalidInput(hint))
            if hint == crate::runtime::OVERSIZE_MESSAGE_HINT
                || hint == crate::runtime::DEGENERATE_MESSAGE_HINT =>
        {
            tracing::info!(
                conversation_id = %conversation_id,
                "serve_turn: graceful guard — answering with guidance"
            );
            // No message id: the turn never started, so nothing was
            // persisted. A host that needs an id for its own bookkeeping
            // supplies its own, which is what the desktop already did here.
            sink.emit(TurnFrame::Token {
                message_id: String::new(),
                chunk: hint,
            });
            sink.emit(TurnFrame::Complete {
                message_id: String::new(),
                provenance: None,
                citations: Vec::new(),
                epistemic_state: None,
                task: None,
            });
            return;
        }
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
    sink.on_turn_started(&message_id);
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
    let task = project_task(&metadata);
    sink.emit(TurnFrame::Complete {
        message_id,
        provenance,
        citations,
        epistemic_state,
        task,
    });
}

/// Run a turn that cannot token-stream and emit it in the streaming turn's
/// own frame vocabulary.
///
/// [`Runtime::handle_message`] persists the user message and then runs the
/// turn chain, so this is only ever correct when nothing has persisted the
/// user message yet — which is exactly why [`serve_turn`] decides *before* it
/// starts the streaming path rather than after it fails. See its doc comment
/// for the two host fallbacks this replaces and the double-write in one of
/// them.
///
/// The whole answer arrives as a single `Token` frame. A client appending
/// deltas renders it identically to a streamed turn; it simply arrives at
/// once, because the document path has nothing to emit until it is done.
async fn serve_non_streaming_turn(
    runtime: &Runtime,
    store: &dyn StateStore,
    conversation_id: &str,
    content: &str,
    sink: &dyn TurnSink,
) {
    tracing::info!(
        conversation_id = %conversation_id,
        "serve_turn: non-streamable turn — running the document path"
    );

    let response = match runtime.handle_message(content, conversation_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                conversation_id = %conversation_id,
                error = %e,
                "serve_turn: non-streaming turn failed"
            );
            sink.emit(TurnFrame::StreamError {
                message: e.to_string(),
                retry_after_secs: None,
            });
            return;
        }
    };

    let message_id = response.message.id.clone();
    // Late by nature: this path has no handle to acquire, so the id does not
    // exist until the turn is done. A caller that needs it early must decide
    // that up front — `is_document_attached` is the same predicate this
    // function was selected by.
    sink.on_turn_started(&message_id);
    sink.emit(TurnFrame::Token {
        message_id: message_id.clone(),
        chunk: response.message.content.clone(),
    });

    // Re-read through the SAME accessor the streamed path uses, rather than
    // projecting `response.message.metadata` directly — one implementation of
    // "what did this turn do" (ARCH §10.6), and the handler may have written
    // more than it returned.
    let metadata = message_metadata(store, conversation_id, &message_id).await;
    let (provenance, citations) = project_message_metadata(&metadata);
    let epistemic_state = project_epistemic_state(&metadata);
    let task = project_task(&metadata);
    sink.emit(TurnFrame::Complete {
        message_id,
        provenance,
        citations,
        epistemic_state,
        task,
    });
}

/// Everything one turn produced, once it has finished.
///
/// The non-streaming shape of [`TurnFrame::Complete`] plus the answer text —
/// what a REST caller, a bench harness or a desktop command wants when it has
/// no use for deltas.
#[derive(Debug, Clone, Default)]
pub struct CollectedTurn {
    /// The assistant message the turn produced.
    pub message_id: String,
    /// The full answer text, in order.
    pub text: String,
    /// How the answer was produced.
    pub provenance: Option<sovereign_contracts::types::projection::Provenance>,
    /// Corpus-grounded citations, in retrieval rank order.
    pub citations: Vec<sovereign_contracts::types::projection::Citation>,
    /// The typed epistemic ledger, when the turn stamped one.
    pub epistemic_state: Option<sovereign_contracts::types::EpistemicState>,
    /// The background task the turn spawned, on the agentic path.
    pub task: Option<sovereign_contracts::types::projection::TaskSummary>,
}

/// A [`TurnSink`] that keeps the frames instead of forwarding them.
#[derive(Default)]
struct Collector {
    inner: std::sync::Mutex<CollectedTurn>,
    failure: std::sync::Mutex<Option<String>>,
}

impl TurnSink for Collector {
    fn emit(&self, frame: TurnFrame) {
        let mut out = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match frame {
            TurnFrame::Token { message_id, chunk } => {
                out.message_id = message_id;
                out.text.push_str(&chunk);
            }
            TurnFrame::Complete {
                message_id,
                provenance,
                citations,
                epistemic_state,
                task,
            } => {
                out.message_id = message_id;
                out.provenance = provenance;
                out.citations = citations;
                out.epistemic_state = epistemic_state;
                out.task = task;
            }
            TurnFrame::StreamError { message, .. } => {
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
            }
            // Progress signals have no meaning to a caller that is not
            // rendering as it goes.
            TurnFrame::Narration { .. } | TurnFrame::QueuePosition { .. } => {}
        }
    }
}

/// Drive one turn to completion and return what it produced — the
/// non-streaming door onto the SAME driver [`serve_turn`] is.
///
/// # Why this is not its own turn implementation
///
/// Every host that wanted a finished answer rather than a stream had written
/// its own: `sovereign-server`'s REST route called `handle_message_any`, the
/// eval and bench harnesses drained `handle_message_stream` by hand, and the
/// desktop had a third shape. Each therefore re-decided, differently and
/// silently, the things [`serve_turn`] decides once — raw-model mode,
/// document-attached turns, which handler runs, and how the result is
/// projected out of the persisted metadata.
///
/// `Runtime::handle_message_any` is the clearest case: it exists ONLY because
/// the REST route needed a non-streaming path, and it re-implements the
/// recipe-author dispatch that `handle_message_stream` already performs
/// internally (`runtime/streaming.rs`, "Recipe-author workspace dispatch …
/// BEFORE the ComplexTask bailout"). Two implementations of one decider, and
/// the streaming one is the one that gets exercised (ARCH §10.6).
///
/// A sink is all that separates the two doors. This one keeps the frames; the
/// streaming callers forward them.
pub async fn collect_turn(
    runtime: &Runtime,
    store: &dyn StateStore,
    conversation_id: &str,
    content: &str,
    mode: TurnMode,
    intent: Option<Intent>,
) -> crate::error::Result<CollectedTurn> {
    let collector = Collector::default();
    serve_turn(
        runtime,
        store,
        conversation_id,
        content,
        mode,
        intent,
        None,
        &collector,
    )
    .await;

    // A `StreamError` frame is the turn's failure, and a caller with no
    // stream to watch would otherwise receive an empty-but-Ok answer — an
    // absence dressed as a result (ARCH §18.3).
    if let Some(message) = collector
        .failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        return Err(crate::error::Error::Inference(message));
    }
    let collected = collector
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Ok(collected)
}
