// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use sovereign_contracts::types::{TurnFrame, TurnMode};
use sovereign_core::runtime::message_metadata;

use crate::state::{self, AppState, DesktopConfig};

// ─── Commands ────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct MessageChunkPayload {
    pub conversation_id: String,
    pub message_id: String,
    pub chunk: String,
}

#[derive(Serialize, Clone)]
pub struct MessageCompletePayload {
    pub conversation_id: String,
    pub message_id: String,
    pub full_text: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct StreamStartedResponse {
    pub message_id: String,
    pub streaming: bool,
}

/// `message-error` payload. Carries `conversation_id` + `message_id` (not
/// just the message) so a turn that fails while the user is viewing a
/// DIFFERENT conversation is still attributable on the desktop side —
/// the live-turns registry keys on `conversation_id` to recover the
/// errored turn when the user returns, instead of the error silently
/// vanishing. The generic `error` / `backend-error` events keep using
/// `crate::approval::ErrorPayload`.
#[derive(Serialize, Clone)]
pub struct MessageErrorPayload {
    pub conversation_id: String,
    pub message_id: String,
    pub message: String,
}

/// Start a streaming chat response. Returns the assigned message_id immediately;
/// the frontend should listen for `message-chunk` and `message-complete` events
/// (or `message-error`) filtered by the returned message_id.
///
/// If the runtime cannot stream the request (e.g. ComplexTask intent), this
/// transparently falls back to `handle_message` and emits a single
/// `message-complete` event with the full result. The `streaming` field on the
/// response indicates which path was taken.
///
/// `context_chunks` lets the desktop attach passages the user is
/// currently reading (the "ask about this passage" handoff). Each
/// chunk is fetched via the corpus engine and prepended to the
/// message as a labelled context block before the runtime sees it
/// — keeping the runtime untouched while still scoping the
/// librarian's answer to what the user has open.
#[derive(serde::Deserialize)]
pub struct FocusedChunkRef {
    pub corpus_id: String,
    pub chunk_id: u64,
}

/// A file the user attached for a *tool* to act on (vision, OCR, audio
/// transcription) — distinct from a document attachment (which is ingested for
/// RAG). Its absolute path is surfaced to the model in the turn's message (see
/// `build_tool_files_preamble`) so the planner passes it to an MCP tool like
/// `describe_image(path)` / `transcribe_audio(path)`. Turn-scoped: nothing is
/// ingested or persisted. The path only helps a *local* MCP server that can
/// read it — the privacy-aligned case (bytes never leave the machine).
#[derive(serde::Deserialize)]
pub struct AttachedFile {
    pub path: String,
    pub name: String,
    /// `"image"` | `"audio"` | `"other"` — drives the prompt hint that nudges
    /// routing toward the right tool. Not authoritative; the tool's own schema
    /// validates the real argument.
    #[serde(default)]
    pub kind: String,
}

#[tauri::command]
pub async fn send_message_stream(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
    context_chunks: Option<Vec<FocusedChunkRef>>,
    attached_files: Option<Vec<AttachedFile>>,
) -> Result<StreamStartedResponse, String> {
    // Readiness gate only — the wire drive below needs no Runtime handle;
    // in Local mode the daemon answering the socket IS this process's.
    let _guard = require_runtime!(state);

    state.approval.set_task_id(&conversation_id).await;

    let store_for_metadata = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    };

    // Build the augmented message: prepend a passage-context block
    // for each focused chunk so the librarian can scope its answer
    // to what the user has open. The preamble uses a structured
    // marker (`▸ passage from "<title>" (corpus: <id>, chunk #N)`)
    // so chat-UI rendering can detect and present it nicely later.
    let augmented_message =
        augment_for_turn(&state, &message, &context_chunks, &attached_files).await;

    // Naked mode (a user setting) runs the loaded model raw — no retrieval,
    // router, grounding gate, tools, atlas, or gap-check. It is a turn
    // PARAMETER now rather than a different function to call.
    let mode = if state.config.read().await.naked_mode {
        tracing::info!(%conversation_id, "send_message_stream: NAKED mode — raw model, affordances bypassed");
        TurnMode::Naked
    } else {
        TurnMode::Grounded
    };

    // Whether this turn can token-stream is a property of the message, and
    // `serve_turn` decides it with the same predicate. Asking it here too is
    // not a second decider — it is this command reporting, in its return
    // value, which shape the frontend should expect.
    let streaming = !sovereign_core::runtime::is_document_attached(&augmented_message);

    // Fallback id for a turn that never mints one: the graceful guards
    // (oversize paste, contentless message) answer without starting a turn,
    // and a document-attached turn has no id until it finishes. The frontend
    // keys its placeholder on whatever this command returns, so the id only
    // has to be CONSISTENT with the events that follow — which is exactly
    // what the previous non-streaming branch did with its pending uuid.
    let pending_id = uuid::Uuid::new_v4().to_string();

    // sv-surface R5: THE WIRE IS THE ONE PATH. This command drove the
    // in-process `Runtime` through `DesktopTurnSink`; it now drives the
    // daemon's turn socket — the same frames down the same renderer, and
    // in Local mode the daemon at the other end IS this process's own
    // commission. "Local" means the daemon happens to be in-process, which
    // is the realignment's end state. The socket CLAIMS approvals, so the
    // daemon parks its questions on THIS surface instead of auto-answering
    // them (C1), and the prompt frames become the same cards below.
    let client = sovereign_turn_client::TurnClient::new(state.client_base_url());
    let mut stream = client
        .connect_with(
            &conversation_id,
            sovereign_turn_client::StreamOptions {
                claim_approvals: true,
            },
        )
        .await
        .map_err(|e| format!("opening the turn socket: {e}"))?;
    stream
        .send_message(&augmented_message, mode, None)
        .await
        .map_err(|e| format!("sending the turn: {e}"))?;
    let message_id = finish_wire_turn(
        &state,
        app_handle,
        stream,
        conversation_id,
        pending_id,
        store_for_metadata,
    )
    .await?;

    Ok(StreamStartedResponse {
        message_id,
        streaming,
    })
}

/// Which frames may legally precede `TurnStarted`, and what the turn's
/// sync id becomes when one arrives that may not.
///
/// The lead window is REAL and it is not disorder. The runtime narrates
/// retrieval before the stream handle is acquired (a cold turn's
/// retrieval is exactly when narration fires), a queued turn reports its
/// place in line, and a `ComplexTask` turn puts its FIRST prompt up
/// before any message exists to stream — sovereign-mesh's
/// `turn_surface.rs` pins that order. Reading a leading `Prompt` as
/// disorder is the RB3 defect: the fallback id was taken AND the prompt
/// frame was handed to the renderer, which drops prompts, so every
/// agentic ask hung with no card.
#[derive(Debug, PartialEq, Eq)]
enum LeadDisposition {
    /// Legal before the id — forward it and keep waiting.
    KeepWaiting,
    /// The turn minted its id (G10).
    SyncId(String),
    /// A message-carrying frame arrived with no `TurnStarted` — the
    /// graceful guards (oversize paste, contentless message) answer
    /// exactly this way. Take the id off the FRAME when it carries one,
    /// the caller's pending uuid otherwise: `render_turn_frames` keys its
    /// events off these same frames, so agreeing with it here is what
    /// keeps the placeholder and the chunks on one id.
    SettleOn(Option<String>),
}

fn lead_disposition(frame: &TurnFrame) -> LeadDisposition {
    use sovereign_contracts::types::TurnNotice;
    match frame {
        TurnFrame::Notice {
            notice: TurnNotice::TurnStarted { message_id },
        } => LeadDisposition::SyncId(message_id.clone()),
        TurnFrame::Narration { .. }
        | TurnFrame::QueuePosition { .. }
        | TurnFrame::Prompt { .. }
        | TurnFrame::Notice { .. } => LeadDisposition::KeepWaiting,
        TurnFrame::Token { message_id, .. } | TurnFrame::Complete { message_id, .. } => {
            LeadDisposition::SettleOn((!message_id.is_empty()).then(|| message_id.clone()))
        }
        TurnFrame::StreamError { .. } => LeadDisposition::SettleOn(None),
    }
}

/// What the pump's verdict on the sync id means to the command that is
/// waiting for it.
///
/// `Ok(Some(id))` and `Ok(None)` are the two ways a turn decides
/// (see [`LeadDisposition`]). `Err` is the pump ending WITHOUT deciding —
/// the socket died, or spoke a frame that would not parse, before
/// anything named a message.
///
/// C3: that third case used to be folded into the fallback id, so the
/// command returned `Ok(StreamStartedResponse)` for a turn that never
/// happened and the frontend held a placeholder no `message-complete`
/// and no `message-error` would ever close — a permanent "preparing". A
/// dropped turn is not an empty one (§18.3): it is reported as dropped.
fn sync_id_or_dropped(
    reported: Result<Option<String>, tokio::sync::oneshot::error::RecvError>,
    fallback: String,
) -> Result<String, String> {
    match reported {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Ok(fallback),
        Err(_) => Err("the turn socket ended before the turn started".to_string()),
    }
}

/// The second half of every wire turn (sv-surface R5): park the write
/// half under THIS conversation, spawn the ONE loop that reads the
/// socket, take the sync id it reports off the `TurnStarted` frame (G10
/// — emitted at handle acquisition, so the placeholder goes up over the
/// same window it always did in-process), then spawn the renderer that
/// turns frames into `message-chunk` / `message-complete`.
///
/// There is no second drain here. The sync id used to come from a
/// private loop that ran to `TurnStarted` forwarding only
/// Narration|Notice, which is how a leading `Prompt` reached a renderer
/// that drops prompts (RB3). The pump owns every frame from the first
/// one and hands the id back over a oneshot; dropping that oneshot
/// without deciding is how it reports a turn that died before it started
/// (C3).
async fn finish_wire_turn(
    state: &Arc<AppState>,
    app_handle: tauri::AppHandle,
    stream: sovereign_turn_client::TurnStream,
    conversation_id: String,
    fallback_id: String,
    store_for_metadata: Option<Arc<dyn sovereign_core::traits::StateStore>>,
) -> Result<String, String> {
    // Park the write half FIRST, keyed by conversation (RB5): a prompt can
    // arrive before the id does, and `cancel_stream` / the submit commands
    // reach THIS turn — not whichever turn started most recently — while
    // the pump keeps reading. The handle comes back so the pump can prove
    // the parked entry is still its own before clearing it.
    let mine = state
        .turn_wire
        .park(&conversation_id, stream.sender())
        .await;

    let (frame_tx, frame_rx) = tokio::sync::mpsc::unbounded_channel::<TurnFrame>();
    let (id_tx, id_rx) = tokio::sync::oneshot::channel::<Option<String>>();

    tauri::async_runtime::spawn(pump_wire_frames(
        app_handle.clone(),
        Arc::clone(state),
        stream,
        conversation_id.clone(),
        frame_tx,
        Some(id_tx),
        mine,
    ));

    let message_id = match sync_id_or_dropped(id_rx.await, fallback_id.clone()) {
        Ok(id) => id,
        Err(message) => {
            tracing::warn!(
                conversation_id = %conversation_id,
                message_id = %fallback_id,
                "finish_wire_turn: {message}"
            );
            let _ = app_handle.emit(
                "message-error",
                MessageErrorPayload {
                    conversation_id,
                    message_id: fallback_id,
                    message: message.clone(),
                },
            );
            return Err(message);
        }
    };

    // Render the frames as the events the frontend already listens for. The
    // payload shapes are unchanged, so no TypeScript moved with this. The
    // pump has been buffering into `frame_tx` since before the id existed,
    // so the lead frames arrive here in order.
    tauri::async_runtime::spawn(render_turn_frames(
        app_handle,
        frame_rx,
        conversation_id,
        message_id.clone(),
        store_for_metadata,
    ));

    Ok(message_id)
}

/// THE per-turn frame loop: the only reader of this socket. Tokens and the
/// terminal frame go to the ONE renderer unchanged; prompts and notices
/// map onto the event vocabulary the frontend already listens for, payload
/// for payload — the same cards the in-process `TauriApprovalChannel` used
/// to raise. The routing notices also record session→conversation, which is
/// how a redirect click (a session id alone) finds its socket.
///
/// `sync_id` is the channel `finish_wire_turn` is waiting on: the first
/// frame that decides the turn's id fills it (see [`lead_disposition`]),
/// and a socket that ends before any frame does drops it, which is the
/// signal that the turn never started.
async fn pump_wire_frames(
    app_handle: tauri::AppHandle,
    state: Arc<AppState>,
    mut stream: sovereign_turn_client::TurnStream,
    conversation_id: String,
    frames: tokio::sync::mpsc::UnboundedSender<TurnFrame>,
    mut sync_id: Option<tokio::sync::oneshot::Sender<Option<String>>>,
    mine: Arc<sovereign_turn_client::TurnSender>,
) {
    let mut frames_seen = 0u32;
    loop {
        let frame = match stream.next_frame().await {
            Ok(Some(f)) => f,
            ended => {
                tracing::info!(
                    conversation_id = %conversation_id,
                    frames_seen,
                    ended_cleanly = matches!(ended, Ok(None)),
                    turn_started = sync_id.is_none(),
                    "pump_wire_frames: socket ended"
                );
                break; // socket closed; the renderer speaks next
            }
        };
        frames_seen += 1;

        // The sync id, decided ONCE, by the same loop that raises the
        // cards — so a `Prompt` in the lead window gets its card AND
        // leaves the id still to be minted.
        if let Some(tx) = sync_id.take() {
            match lead_disposition(&frame) {
                LeadDisposition::KeepWaiting => sync_id = Some(tx),
                LeadDisposition::SyncId(id) => {
                    tracing::info!(
                        conversation_id = %conversation_id,
                        id_from = "turn_started",
                        frames_seen,
                        "pump_wire_frames: sync id taken"
                    );
                    let _ = tx.send(Some(id));
                }
                LeadDisposition::SettleOn(id) => {
                    tracing::info!(
                        conversation_id = %conversation_id,
                        id_from = if id.is_some() { "frame" } else { "fallback" },
                        frames_seen,
                        "pump_wire_frames: sync id taken with no TurnStarted"
                    );
                    let _ = tx.send(id);
                }
            }
        }

        match &frame {
            TurnFrame::Prompt { id, prompt } => {
                state
                    .pending_prompts
                    .park(id, &conversation_id, prompt.clone())
                    .await;
                raise_prompt_card(&app_handle, &conversation_id, id, prompt);
            }
            TurnFrame::Notice { notice } => {
                use sovereign_contracts::types::{ResolveOutcome, TurnNotice};
                match notice {
                    TurnNotice::MessageRefined(payload) => {
                        let _ = app_handle.emit("message-refined", payload.clone());
                    }
                    TurnNotice::LessonProposed(payload) => {
                        let _ = app_handle.emit("lesson-proposed", payload.clone());
                    }
                    TurnNotice::StepDone {
                        task_id,
                        step_id,
                        description,
                        status,
                    } => {
                        let _ = app_handle.emit(
                            "step-done",
                            crate::approval::StepDonePayload {
                                task_id: task_id.clone(),
                                step_id: *step_id,
                                description: description.clone(),
                                status: status.to_string(),
                            },
                        );
                    }
                    // RB4, client half: an ack is NEVER terminal. The
                    // daemon's answer refusals ride here — they used to
                    // ride `StreamError`, which this surface ends the turn
                    // on, so a double-clicked approve discarded a healthy
                    // turn. Nothing below breaks the loop.
                    TurnNotice::ResolveAck { id, outcome } => match outcome {
                        ResolveOutcome::Resolved => {
                            // The question is consumed; a stale submit now
                            // fails fast instead of guessing.
                            state.pending_prompts.resolve(id).await;
                        }
                        ResolveOutcome::WrongKind => {
                            // The question SURVIVES — the desk left it
                            // parked for the right answer, so the card
                            // comes back. Same event, same payload, from
                            // the prompt parked with it: no new frontend
                            // vocabulary, because the listeners are the
                            // ones `raise_prompt_card` already feeds.
                            match state.pending_prompts.get(id).await {
                                Some(parked) => {
                                    tracing::warn!(
                                        conversation_id = %conversation_id,
                                        prompt_id = %id,
                                        "pump_wire_frames: wrong-kind answer refused — re-raising the card"
                                    );
                                    raise_prompt_card(
                                        &app_handle,
                                        &conversation_id,
                                        id,
                                        &parked.prompt,
                                    );
                                }
                                None => tracing::warn!(
                                    conversation_id = %conversation_id,
                                    prompt_id = %id,
                                    "pump_wire_frames: wrong-kind ack for a card this surface never parked"
                                ),
                            }
                        }
                        // The three that reached nothing. Each drops our
                        // record of the card so the next submit refuses
                        // here instead of spending a round trip, and each
                        // says WHICH nothing — the distinction is the
                        // whole point of the typed outcome (§18.3).
                        ResolveOutcome::NoSuchPending => {
                            tracing::warn!(
                                conversation_id = %conversation_id,
                                prompt_id = %id,
                                "pump_wire_frames: nothing was parked under that id — already answered, or the turn moved on"
                            );
                            state.pending_prompts.resolve(id).await;
                        }
                        ResolveOutcome::WaiterGone => {
                            tracing::warn!(
                                conversation_id = %conversation_id,
                                prompt_id = %id,
                                "pump_wire_frames: the answer was taken but the executor had already gone — nothing resumed"
                            );
                            state.pending_prompts.resolve(id).await;
                        }
                        ResolveOutcome::Unclaimed => {
                            // The socket never claimed this turn's
                            // approvals, so no card of ours could have
                            // been parked there. Every streaming command
                            // on this surface connects with
                            // `claim_approvals: true`, so seeing this at
                            // all means the claim was refused — worth
                            // saying loudly rather than swallowing.
                            tracing::error!(
                                conversation_id = %conversation_id,
                                prompt_id = %id,
                                "pump_wire_frames: this socket holds no approval desk — the turn's approvals were never claimed"
                            );
                            state.pending_prompts.resolve(id).await;
                        }
                    },
                    TurnNotice::InterpretationProposed(p) => {
                        state
                            .session_conversations
                            .write()
                            .await
                            .insert(p.session_id.clone(), p.conversation_id.clone());
                    }
                    TurnNotice::ClarificationRequest(p) => {
                        state
                            .session_conversations
                            .write()
                            .await
                            .insert(p.session_id.clone(), p.conversation_id.clone());
                    }
                    // The socket's own closer (RB1). `Complete` is the
                    // terminal frame of the TURN, not of the host's
                    // talking — `MessageRefined` and `LessonProposed`
                    // fire after it from a detached spawn — so this is
                    // the first moment a client can know the socket has
                    // finished being useful. Ending here rather than on
                    // the far end's close is what stops one leaked
                    // connection + writer task per turn. The renderer
                    // ignores `Notice` frames, so nothing is lost by not
                    // forwarding it.
                    TurnNotice::TurnSettled { message_id } => {
                        tracing::info!(
                            conversation_id = %conversation_id,
                            message_id = %message_id,
                            frames_seen,
                            "pump_wire_frames: turn settled — closing the socket"
                        );
                        break;
                    }
                    TurnNotice::TurnStarted { .. } => {}
                }
            }
            _ => {}
        }
        let _ = frames.send(frame);
    }

    // RB5: unpark THIS turn, and only if the entry is still ours. A
    // redirect parks a second socket on the same conversation while this
    // drain is still finishing; the loser must not clear the winner.
    let released = state.turn_wire.release(&conversation_id, &mine).await;
    tracing::info!(
        conversation_id = %conversation_id,
        released,
        "pump_wire_frames: turn unparked"
    );
}

/// One parked prompt becomes one card — the same events, payloads and
/// `key` echo the frontend's approval machinery already speaks, with the
/// wire prompt's id in the `key` slot. The in-process channel raised
/// these from `TauriApprovalChannel`; over the wire the frames carry the
/// same content, so the cards are byte-identical.
fn raise_prompt_card(
    app: &tauri::AppHandle,
    conversation_id: &str,
    id: &str,
    prompt: &sovereign_contracts::types::TurnPrompt,
) {
    use sovereign_contracts::types::TurnPrompt;
    use tauri::Emitter;
    match prompt {
        TurnPrompt::Approval { preview } => {
            let _ = app.emit(
                "approval-request",
                crate::approval::ApprovalRequestPayload {
                    task_id: conversation_id.to_string(),
                    step_id: 0,
                    key: id.to_string(),
                    tool_id: preview.tool_id.clone(),
                    description: preview.description.clone(),
                    params: preview.params.clone(),
                },
            );
        }
        TurnPrompt::UserInput { question } => {
            let _ = app.emit(
                "user-input-request",
                crate::approval::UserInputRequestPayload {
                    task_id: conversation_id.to_string(),
                    key: id.to_string(),
                    question: question.clone(),
                },
            );
        }
        TurnPrompt::Information { request } => {
            let _ = app.emit(
                "information-request",
                crate::approval::InformationRequestPayload {
                    task_id: conversation_id.to_string(),
                    step_id: request.step_id,
                    key: id.to_string(),
                    current_understanding: request.current_understanding.clone(),
                    gap: request.gap.clone(),
                    relevance: request.relevance.clone(),
                    satisfying_source: request.satisfying_source.clone(),
                    search_hints: request.search_hints.clone(),
                    kind: request.kind,
                    task_title: request.task_title.clone(),
                    routes: request.routes.clone(),
                },
            );
        }
    }
}

/// Bridges [`serve_turn`] to the Tauri event surface.
///
/// The ONE frame renderer: turn frames in, the frontend's event vocabulary
/// out. Every turn-shaped command drives the wire (sv-surface R5: the
/// daemon's turn socket through `finish_wire_turn`) and renders through
/// this loop — the payload shapes are unchanged, so no TypeScript moves
/// with any conversion onto it (sv-surface rung 0). The in-process
/// `DesktopTurnSink` this used to pair with is deleted with the local
/// drive; its two jobs — frame forwarding and the sync id — live in
/// `finish_wire_turn` and the `TurnStarted` frame now.
///
/// `fallback_id` keys the placeholder for a turn that never mints an id
/// (graceful guards, the document path); a command that already knows its
/// id passes it here so the events are consistent from the first chunk.
async fn render_turn_frames(
    app: tauri::AppHandle,
    mut frame_rx: tokio::sync::mpsc::UnboundedReceiver<TurnFrame>,
    conversation_id: String,
    fallback_id: String,
    store_for_metadata: Option<Arc<dyn sovereign_core::traits::StateStore>>,
) {
    let mut full_text = String::new();
    let mut real_message_id: Option<String> = None;

    while let Some(frame) = frame_rx.recv().await {
        match frame {
            TurnFrame::Token { message_id, chunk } => {
                if !message_id.is_empty() {
                    real_message_id = Some(message_id);
                }
                full_text.push_str(&chunk);
                let _ = app.emit(
                    "message-chunk",
                    MessageChunkPayload {
                        conversation_id: conversation_id.clone(),
                        message_id: real_message_id
                            .clone()
                            .unwrap_or_else(|| fallback_id.clone()),
                        chunk,
                    },
                );
            }
            TurnFrame::StreamError { message, .. } => {
                let _ = app.emit(
                    "message-error",
                    MessageErrorPayload {
                        conversation_id: conversation_id.clone(),
                        message_id: real_message_id
                            .clone()
                            .unwrap_or_else(|| fallback_id.clone()),
                        message,
                    },
                );
                return;
            }
            TurnFrame::Complete { message_id, .. } => {
                if !message_id.is_empty() {
                    real_message_id = Some(message_id);
                }
                let emit_id = real_message_id
                    .clone()
                    .unwrap_or_else(|| fallback_id.clone());

                // The persisted blob, read IN PROCESS. The driver projects
                // typed provenance for callers across a socket; this surface
                // owns the store, and the frontend's
                // `MessageCompletePayload.metadata` is the raw shape it has
                // always received.
                let metadata = match (&store_for_metadata, &real_message_id) {
                    (Some(store), Some(id)) => {
                        message_metadata(store.as_ref(), &conversation_id, id).await
                    }
                    // A turn that never started (a graceful guard) has no
                    // row to read. Mark the intent so the turn is visible
                    // to the provenance surface and the loading state
                    // clears, instead of an intent-less blank.
                    _ => Some(serde_json::json!({
                        "intent": if full_text == sovereign_core::runtime::OVERSIZE_MESSAGE_HINT {
                            "oversize_guidance"
                        } else if full_text == sovereign_core::runtime::DEGENERATE_MESSAGE_HINT {
                            "clarification"
                        } else {
                            "error"
                        }
                    })),
                };

                // Strip phantom tool-call envelopes the chat model
                // reflexes for code/lookup questions — chat wires no
                // executable tools, so the raw call must not leak.
                //
                // EXEMPT recipe-author: that path parses and EXECUTES
                // tool calls from the assistant's prose server-side
                // before this point, so present_answer must not touch its
                // display. EXEMPT a cancelled turn: it is shown exactly
                // as it streamed, and present_answer's empty-input path
                // would substitute a fallback that both misrepresents a
                // turn the user stopped AND breaks stream integrity
                // (concat(chunks) == full_text).
                let is_recipe_author = metadata
                    .as_ref()
                    .and_then(|m| m.get("intent"))
                    .and_then(|v| v.as_str())
                    == Some("RecipeAuthor");
                let was_cancelled = metadata
                    .as_ref()
                    .and_then(|m| m.get("provenance"))
                    .and_then(|p| p.get("finish_reason"))
                    .and_then(|f| f.as_str())
                    == Some("cancelled");
                let full_text = if is_recipe_author || was_cancelled {
                    std::mem::take(&mut full_text)
                } else {
                    sovereign_core::pipeline::presenter::present_answer(&full_text)
                };

                let _ = app.emit(
                    "message-complete",
                    MessageCompletePayload {
                        conversation_id: conversation_id.clone(),
                        message_id: emit_id,
                        full_text,
                        metadata,
                    },
                    // Glassbox (first-turn hang): the terminal render —
                    // its absence beside a pump "socket ended" line
                    // localizes any frame loss.
                );
                // Sidebar: updated_at bumped; title may auto-update.
                let _ = app.emit("conversations:changed", ());
                return;
            }
            // Everything that is not a token, an error or the terminal
            // frame is already handled UPSTREAM, by `pump_wire_frames`:
            // narration, queue position, the prompt cards and every
            // notice are emitted there against the frontend vocabulary
            // that expects them. They pass through here so this renderer
            // stays the single place a turn's `message-chunk` /
            // `message-complete` / `message-error` are decided — not
            // because they belong to some other path. (This comment said
            // a `Prompt` belongs to the ATTACH path and that no frame is
            // ever produced; since R5 BOTH boot modes drive the wire and
            // the daemon produces exactly these frames.)
            TurnFrame::Narration { .. }
            | TurnFrame::QueuePosition { .. }
            | TurnFrame::Prompt { .. }
            | TurnFrame::Notice { .. } => {}
        }
    }
}

/// Per-chunk character budget for the focused-passage preamble.
/// Bounded so a hugely-long chunk doesn't blow up the runtime's
/// turn-message size cap. The preamble is meant to scope the
/// answer, not replace retrieval.
const CONTEXT_PASSAGE_CHAR_BUDGET: usize = 2000;

/// Build a message with each focused chunk prepended as a labelled
/// passage block. The marker syntax (`▸ passage from "<title>"`)
/// is detectable for future chat-UI rendering that wants to show
/// these as collapsed chips instead of inline text.
async fn build_context_augmented_message(
    state: &State<'_, Arc<AppState>>,
    user_message: &str,
    refs: &[FocusedChunkRef],
) -> String {
    let engine_opt = state.corpus_engine.read().await.clone();
    let Some(engine) = engine_opt else {
        return user_message.to_string();
    };

    // Dedupe by (corpus_id, chunk_id) — preserves first-seen order.
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<&FocusedChunkRef> = refs
        .iter()
        .filter(|r| seen.insert((r.corpus_id.clone(), r.chunk_id)))
        .collect();

    let mut blocks: Vec<String> = Vec::new();
    for r in unique {
        let index = match engine.open_index_for_corpus(&r.corpus_id).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(
                    corpus = %r.corpus_id,
                    chunk_id = r.chunk_id,
                    error = %e,
                    "context preamble: open_index failed; skipping chunk",
                );
                continue;
            }
        };
        let mut rows = match index.chunks_by_ids(&[r.chunk_id]).await {
            Ok(rs) => rs,
            Err(e) => {
                tracing::warn!(
                    corpus = %r.corpus_id,
                    chunk_id = r.chunk_id,
                    error = %e,
                    "context preamble: chunks_by_ids failed; skipping chunk",
                );
                continue;
            }
        };
        let Some(row) = rows.pop() else { continue };
        let title = row.title.as_deref().unwrap_or("untitled passage");
        let content = if row.content.chars().count() > CONTEXT_PASSAGE_CHAR_BUDGET {
            let truncated: String = row
                .content
                .chars()
                .take(CONTEXT_PASSAGE_CHAR_BUDGET)
                .collect();
            format!("{truncated}…")
        } else {
            row.content.clone()
        };
        blocks.push(format!(
            "▸ passage from \"{title}\" (corpus: {}, chunk #{})\n\n{content}",
            r.corpus_id, r.chunk_id
        ));
    }

    if blocks.is_empty() {
        return user_message.to_string();
    }

    format!("{}\n\n---\n\n{}", blocks.join("\n\n---\n\n"), user_message)
}

/// Apply both turn augmentations: focused-passage context (corpus-backed) then
/// the tool-files preamble (attached image/audio paths). The user's message
/// stays at the end; each augmentation prepends its block above it. Shared by
/// the streaming and non-streaming send commands so they stay identical.
async fn augment_for_turn(
    state: &State<'_, Arc<AppState>>,
    message: &str,
    context_chunks: &Option<Vec<FocusedChunkRef>>,
    attached_files: &Option<Vec<AttachedFile>>,
) -> String {
    let mut augmented = match context_chunks {
        Some(refs) if !refs.is_empty() => {
            build_context_augmented_message(state, message, refs).await
        }
        _ => message.to_string(),
    };
    if let Some(files) = attached_files {
        if !files.is_empty() {
            augmented = build_tool_files_preamble(files, &augmented);
        }
    }
    augmented
}

/// Prepend a labelled block naming each attached file's path so the model can
/// pass it to a tool. Pure + synchronous (no corpus engine, unlike the passage
/// preamble) — the path is the payload. The `▸ attached file:` marker mirrors
/// the passage marker so chat-UI rendering can present these as chips later.
/// A kind-aware hint nudges routing toward the right tool class.
fn build_tool_files_preamble(files: &[AttachedFile], user_message: &str) -> String {
    if files.is_empty() {
        return user_message.to_string();
    }
    let blocks: Vec<String> = files
        .iter()
        .map(|f| {
            let hint = match f.kind.as_str() {
                "image" => "Use an image tool to inspect it (e.g. describe or OCR).",
                "audio" => "Use a transcription tool to convert it to text.",
                _ => "Call a tool with its path to work with it.",
            };
            let kind_label = if f.kind.is_empty() {
                String::new()
            } else {
                format!(" ({})", f.kind)
            };
            format!(
                "▸ attached file: {}{}\n  path: {}\n  {hint}",
                f.name, kind_label, f.path
            )
        })
        .collect();
    format!("{}\n\n---\n\n{}", blocks.join("\n\n"), user_message)
}

#[cfg(test)]
mod tool_files_tests {
    use super::*;

    fn file(name: &str, path: &str, kind: &str) -> AttachedFile {
        AttachedFile {
            path: path.into(),
            name: name.into(),
            kind: kind.into(),
        }
    }

    #[test]
    fn empty_attachments_pass_message_through() {
        assert_eq!(build_tool_files_preamble(&[], "hello"), "hello");
    }

    #[test]
    fn preamble_carries_path_and_keeps_message_last() {
        let out = build_tool_files_preamble(
            &[file("memo.m4a", "/home/u/memo.m4a", "audio")],
            "transcribe this",
        );
        // The path is present for the model to pass to a tool…
        assert!(out.contains("/home/u/memo.m4a"), "{out}");
        // …with a kind-aware hint…
        assert!(out.contains("transcription tool"), "{out}");
        // …and the user's message stays at the very end.
        assert!(out.trim_end().ends_with("transcribe this"), "{out}");
    }

    #[test]
    fn image_kind_hints_image_tool() {
        let out =
            build_tool_files_preamble(&[file("err.png", "/t/err.png", "image")], "what is this?");
        assert!(out.contains("image tool"), "{out}");
        assert!(out.contains("/t/err.png"), "{out}");
    }
}

#[tauri::command]
pub async fn send_message(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
    context_chunks: Option<Vec<FocusedChunkRef>>,
    attached_files: Option<Vec<AttachedFile>>,
) -> Result<MessageResponse, String> {
    // Readiness gate only — the one-shot answer crosses the wire.
    let _guard = require_runtime!(state);

    state.approval.set_task_id(&conversation_id).await;

    let augmented_message =
        augment_for_turn(&state, &message, &context_chunks, &attached_files).await;

    // The SAME driver `send_message_stream` uses. These two commands answer
    // the same question from the same app and used to run different
    // pipelines — the streaming one and the non-streaming one — so the answer
    // depended on which button the user pressed.
    // sv-surface R5: the one-shot answer crosses the wire too — the REST
    // turn (POST /v1/conversations/{id}/messages) drives the SAME driver
    // the socket serves, through the client family. DELTA, named: the
    // `metadata` this returns is the wire's TYPED projection
    // (`Complete.metadata`) where the in-process read returned the raw
    // persisted blob — the G9 row's divergence, now the frontend's to
    // consume field-by-field (its only blob-key gates, recipe-author and
    // cancelled, have wire homes: `routed_intent` (stamped by R3) and
    // `provenance.finish_reason`).
    let turn = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .send_message(&conversation_id, &augmented_message)
        .await
        .map_err(|e| e.to_string())?;

    // Notify the sidebar — updated_at bumped, title may be auto-generated
    // asynchronously. A second event fires when the title lands (runtime
    // spawns the auto-title task independently, but we emit conservatively
    // here so list ordering refreshes immediately).
    let _ = app_handle.emit("conversations:changed", ());

    Ok(MessageResponse {
        message_id: turn.message_id,
        // Always the assistant: this command returns the reply to the message
        // just sent.
        role: "assistant".to_string(),
        content: turn.text,
        task: turn.task.map(|t| TaskSummary {
            id: t.id,
            status: t.status,
            steps_completed: t.steps_completed,
        }),
        metadata: turn
            .metadata
            .map(|m| serde_json::to_value(&m).unwrap_or(serde_json::Value::Null)),
    })
}

// ─── Antifragile-routing commands ────────────────────────────

/// PR6 — cancel the current in-flight stream for a conversation.
/// Finds the most-recent live QuerySession for that conversation
/// and cancels its token. The sampler's per-iteration check
/// notices, breaks the decode loop, and closes the stream — the
/// frontend's existing `message-complete` listener transitions
/// chat.machine back to idle. Returns Ok even if no session was
/// live; the UI may have raced the stream closing naturally, and
/// the user's intent ("I want to stop") is still satisfied.
#[tauri::command]
pub async fn cancel_stream(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<(), String> {
    // sv-surface R5/G2: wire-first — the daemon trips the turn's own
    // cancellation (session token + preparing token, the same pair this
    // command used to trip in-process), and the ordinary Complete with
    // finish_reason=cancelled closes the stream.
    //
    // RB5: THIS conversation's turn. The sender used to be one global
    // slot, so a Stop on conversation A cancelled whichever turn started
    // most recently.
    //
    // C9: `send_cancel() == Ok` means the writer task took the bytes —
    // QUEUED, not acted. The acknowledgement is the turn's own terminal
    // frame (`Complete` with `provenance.finish_reason: "cancelled"`,
    // which the renderer already turns into `message-complete`), and it
    // arrives later or not at all. So a queued send does NOT license
    // skipping the local pair below: in Local mode the daemon's turn IS
    // this process's, and tripping its session token is idempotent with
    // the daemon's own abort; in attach mode there is no local session to
    // trip and the block is a no-op. Only the wire send is skipped when
    // no turn is parked here.
    let wire_sent = match state.turn_wire.sender_for(&conversation_id).await {
        Some(sender) => match sender.send_cancel() {
            Ok(()) => {
                tracing::info!(
                    conversation_id,
                    "cancel_stream: wire cancel queued — the turn's Complete will say cancelled"
                );
                true
            }
            Err(e) => {
                tracing::warn!(
                    conversation_id,
                    error = %e,
                    "cancel_stream: the turn socket's writer is gone — falling back to the local pair"
                );
                false
            }
        },
        None => {
            tracing::info!(
                conversation_id,
                "cancel_stream: no wire turn is parked for this conversation"
            );
            false
        }
    };

    let guard = state.runtime.read().await;
    let Some(runtime) = guard.as_ref() else {
        // No local turn machinery at all (attach mode, or bootstrap still
        // in flight). If the wire took the cancel that is the whole
        // answer; if it did not, nothing was cancelled and saying `Ok`
        // would be the §18.3 substitution.
        return if wire_sent {
            Ok(())
        } else {
            Err(format!(
                "cancel_stream: no wire turn is parked for conversation {conversation_id} \
                 and this surface holds no local runtime"
            ))
        };
    };
    // Cancel the registered session if one exists…
    let hit_session = runtime
        .sessions
        .latest_for_conversation(&conversation_id)
        .map(|session| {
            session.cancel.cancel();
            session.id.clone()
        });
    // …AND trip any reserved preparing-window token. On a slow model the
    // Stop click races session registration: `latest_for_conversation`
    // above may have cancelled the PREVIOUS (stale) session while the real
    // turn is still in preparing (build-context + classify + retrieve, ~5s
    // on a 4B). `cancel_preparing` trips the token `sessions.begin` will
    // ADOPT, so the cancel carries through no matter which side of
    // registration we landed on. (2026-07-07 slow-model race.)
    let hit_preparing = runtime.sessions.cancel_preparing(&conversation_id);
    match (&hit_session, hit_preparing) {
        (Some(id), _) => tracing::info!(
            session_id = %id,
            preparing = hit_preparing,
            conversation_id,
            "cancel_stream: user requested abort"
        ),
        (None, true) => tracing::info!(
            conversation_id,
            "cancel_stream: cancelled a preparing turn (raced session registration)"
        ),
        (None, false) => tracing::info!(
            // Neither a live session nor a preparing turn — the stream
            // likely already finished. The desktop UI recovers optimistically
            // in `handleStop` regardless; log the inventory so an id/timing
            // mismatch stays legible.
            conversation_id,
            wire_sent,
            live_sessions = ?runtime.sessions.conversation_ids(),
            "cancel_stream: nothing in flight locally — cancel is a no-op (already finished?)"
        ),
    }
    Ok(())
}

/// PR2c — cancel the in-flight Propose-mode sampler AND start a new
/// stream against the chosen alternative intent. The original user
/// message + conversation id are pulled from the SessionStore (saved
/// at classify time) so the frontend only passes the session id +
/// intent hint.
///
/// Returns a `StreamStartedResponse` just like `send_message_stream`
/// — the frontend listens for `message-chunk` / `message-complete`
/// events keyed on the new `message_id`. The old assistant message
/// is marked `redirected_away=true` in its metadata (added by
/// `handle_message_stream_with_classification` when it detects a
/// pre-existing cancelled stream on this conversation).
#[tauri::command]
pub async fn redirect_turn(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
    intent_hint: String,
) -> Result<StreamStartedResponse, String> {
    // sv-surface R5: the redirect crosses the wire (`send_redirect`, the
    // client family's sender). The daemon owns the session store, so the
    // conversation comes from the surface's OWN map — recorded as the
    // routing notices that raised this card arrived — with the in-process
    // session store as the fallback while unconverted shapes remain.
    let conversation_id = {
        let map = state.session_conversations.read().await;
        map.get(&session_id).cloned()
    };
    let conversation_id = match conversation_id {
        Some(cid) => cid,
        None => {
            // sv-surface D9 — the third no-fork degradation, named
            // rather than left to read an empty register. The fallback
            // asks whichever `Runtime` THIS process happens to host: in
            // Local that is the same object the embedded daemon serves
            // the turn from, so it answers; in attach this process has
            // never seen the session and `require_runtime!` reported
            // that as "Backend is still loading", which is a different
            // and wrong fact (ARCH §18.3). A soft read, so the mode is
            // not branched on — "Local" is just the boot where the
            // daemon happens to be in-process — and a named refusal
            // when nobody here knows the pairing.
            let hosted = state.runtime.read().await;
            hosted
                .as_ref()
                .and_then(|rt| rt.sessions.get(&session_id).map(|s| s.conversation_id.clone()))
                .ok_or_else(|| {
                    format!(
                        "session {session_id} is not paired with a conversation on this surface                          — the routing card that would have recorded it never arrived, and this                          process does not host the session store that owns the pairing"
                    )
                })?
        }
    };

    let store_for_metadata = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let client = sovereign_turn_client::TurnClient::new(state.client_base_url());
    let mut stream = client
        .connect_with(
            &conversation_id,
            sovereign_turn_client::StreamOptions {
                claim_approvals: true,
            },
        )
        .await
        .map_err(|e| format!("opening the turn socket: {e}"))?;
    // Redirect carries NO content on purpose: the daemon re-answers the
    // message the session already holds — a client re-sending text could
    // disagree with what was actually asked.
    stream
        .send_redirect(&session_id, &intent_hint)
        .await
        .map_err(|e| format!("sending the redirect: {e}"))?;
    let message_id = finish_wire_turn(
        &state,
        app_handle,
        stream,
        conversation_id,
        uuid::Uuid::new_v4().to_string(),
        store_for_metadata,
    )
    .await?;

    Ok(StreamStartedResponse {
        message_id,
        streaming: true,
    })
}

/// PR2 — resume a prior session with an explicit intent (from
/// ClarificationCard option click or NextStepOffer button). Skips
/// router classification and dispatches the `message` through the
/// hinted intent. Returns a `StreamStartedResponse` just like
/// `send_message_stream` so the desktop listener machinery is
/// identical; the frontend receives `message-chunk` +
/// `message-complete` events as usual.
#[tauri::command]
pub async fn resume_session(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
    session_id: String,
    intent_hint: String,
) -> Result<StreamStartedResponse, String> {
    let store_for_metadata = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    };

    // sv-surface R5: the resume crosses the wire (`send_resume`). An
    // expired session is the daemon's call to make — it answers by
    // running the turn normally (provenance, not key), which is why
    // there is no expiry check here.
    let client = sovereign_turn_client::TurnClient::new(state.client_base_url());
    let mut stream = client
        .connect_with(
            &conversation_id,
            sovereign_turn_client::StreamOptions {
                claim_approvals: true,
            },
        )
        .await
        .map_err(|e| format!("opening the turn socket: {e}"))?;
    stream
        .send_resume(&message, &session_id, &intent_hint)
        .await
        .map_err(|e| format!("sending the resume: {e}"))?;
    let message_id = finish_wire_turn(
        &state,
        app_handle,
        stream,
        conversation_id,
        uuid::Uuid::new_v4().to_string(),
        store_for_metadata,
    )
    .await?;

    Ok(StreamStartedResponse {
        message_id,
        streaming: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::types::{ActionPreview, NarrationPhase, TurnNotice, TurnPrompt};

    fn narration() -> TurnFrame {
        TurnFrame::Narration {
            message_id: String::new(),
            phase: NarrationPhase::RoutingCommitted,
            text: "reading the corpus".to_string(),
            elapsed_ms: 12,
        }
    }

    fn approval_prompt() -> TurnFrame {
        TurnFrame::Prompt {
            id: "task-1:0".to_string(),
            prompt: TurnPrompt::Approval {
                preview: ActionPreview {
                    tool_id: "web_search".to_string(),
                    description: "search the web".to_string(),
                    params: serde_json::json!({}),
                },
            },
        }
    }

    /// RB3. A `ComplexTask` turn sends its FIRST prompt before any message
    /// exists to stream (sovereign-mesh `turn_surface.rs` pins that order).
    /// Watched red against the shipped rule, which forwarded only
    /// Narration|Notice and treated everything else — a leading `Prompt`
    /// included — as "settle on the fallback id": the assertion below
    /// failed with `SettleOn(None)`, which is the hang. Every agentic ask
    /// took a fallback id AND handed its prompt to a renderer that drops
    /// prompts, so no card ever went up.
    #[test]
    fn a_prompt_may_precede_turn_started() {
        assert_eq!(
            lead_disposition(&approval_prompt()),
            LeadDisposition::KeepWaiting,
            "a leading Prompt is the ComplexTask shape, not disorder: the \
             card goes up and the id is still to be minted"
        );
    }

    /// The lead window's other legal residents: retrieval narration (the
    /// runtime narrates before the stream handle is acquired) and a
    /// queued turn's place in line.
    #[test]
    fn narration_and_queue_position_may_precede_turn_started() {
        assert_eq!(lead_disposition(&narration()), LeadDisposition::KeepWaiting);
        assert_eq!(
            lead_disposition(&TurnFrame::QueuePosition {
                position: 2,
                estimated_wait_ms: 4_000,
            }),
            LeadDisposition::KeepWaiting
        );
        assert_eq!(
            lead_disposition(&TurnFrame::Notice {
                notice: TurnNotice::ResolveAck {
                    id: "task-1:0".to_string(),
                    outcome: sovereign_contracts::types::ResolveOutcome::Resolved,
                },
            }),
            LeadDisposition::KeepWaiting
        );
    }

    #[test]
    fn turn_started_is_the_sync_id() {
        assert_eq!(
            lead_disposition(&TurnFrame::Notice {
                notice: TurnNotice::TurnStarted {
                    message_id: "msg-real".to_string(),
                },
            }),
            LeadDisposition::SyncId("msg-real".to_string())
        );
    }

    /// A graceful guard (oversize paste, contentless message) answers with
    /// no `TurnStarted` at all. Take the id off the frame that carries one:
    /// `render_turn_frames` keys its events off the same frames, so the
    /// returned id and the emitted id agree — which is the property the
    /// first-turn hang broke.
    #[test]
    fn a_message_carrying_frame_settles_the_id_on_itself() {
        assert_eq!(
            lead_disposition(&TurnFrame::Token {
                message_id: "msg-real".to_string(),
                chunk: "hi".to_string(),
            }),
            LeadDisposition::SettleOn(Some("msg-real".to_string()))
        );
        assert_eq!(
            lead_disposition(&TurnFrame::StreamError {
                message: "boom".to_string(),
                retry_after_secs: None,
            }),
            LeadDisposition::SettleOn(None),
            "an error names no message; the caller's pending id keys the render"
        );
    }

    /// C3. Watched red against the shipped arm (`_ => break
    /// fallback_id.clone()`), which reported a socket that died before the
    /// turn started as an ordinary turn on the fallback id: the assertion
    /// failed with `Ok("pending-uuid")`. The frontend then held a
    /// placeholder that no `message-complete` and no `message-error` would
    /// ever close — permanent "preparing".
    #[tokio::test]
    async fn a_turn_that_never_started_is_reported_not_faked() {
        // The real signal: the pump ended without deciding, so its half of
        // the channel is gone.
        let (id_tx, id_rx) = tokio::sync::oneshot::channel::<Option<String>>();
        drop(id_tx);
        let dropped = sync_id_or_dropped(id_rx.await, "pending-uuid".to_string());
        assert!(
            dropped.is_err(),
            "a socket that ended before the turn started is a dropped turn, \
             not an empty one (§18.3) — got {dropped:?}"
        );
    }

    #[test]
    fn a_decided_id_is_passed_through_and_none_takes_the_fallback() {
        assert_eq!(
            sync_id_or_dropped(Ok(Some("msg-real".to_string())), "pending".to_string()),
            Ok("msg-real".to_string())
        );
        assert_eq!(
            sync_id_or_dropped(Ok(None), "pending".to_string()),
            Ok("pending".to_string())
        );
    }
}
