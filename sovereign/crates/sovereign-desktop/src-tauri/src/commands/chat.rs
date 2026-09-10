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

/// The second half of every wire turn (sv-surface R5): park the write
/// half, take the sync id off the `TurnStarted` frame (G10 — emitted at
/// handle acquisition, so the placeholder goes up over the same window it
/// always did in-process), then spawn the pump that maps frames onto the
/// frontend's event vocabulary and the renderer that turns them into
/// `message-chunk` / `message-complete`. A turn that dies before minting
/// an id falls back to `fallback_id`, exactly the old contract.
async fn finish_wire_turn(
    state: &Arc<AppState>,
    app_handle: tauri::AppHandle,
    mut stream: sovereign_turn_client::TurnStream,
    conversation_id: String,
    fallback_id: String,
    store_for_metadata: Option<Arc<dyn sovereign_core::traits::StateStore>>,
) -> Result<String, String> {
    // Park the write half FIRST: a prompt can arrive before the id does,
    // and the submit commands answer with this sender while the pump
    // keeps reading — the split the client family's socket gave us.
    *state.turn_wire.write().await = Some(stream.sender());

    let (frame_tx, frame_rx) = tokio::sync::mpsc::unbounded_channel::<TurnFrame>();
    let first = stream.next_frame().await;
    let message_id = match first {
        Ok(Some(TurnFrame::Notice {
            notice: sovereign_contracts::types::TurnNotice::TurnStarted { message_id },
        })) => message_id,
        Ok(Some(other)) => {
            // Unexpected order — keep the frame for the renderer and use
            // the fallback id rather than dropping evidence.
            let _ = frame_tx.send(other);
            fallback_id
        }
        _ => fallback_id,
    };

    tauri::async_runtime::spawn(pump_wire_frames(
        app_handle.clone(),
        Arc::clone(state),
        stream,
        conversation_id.clone(),
        frame_tx,
    ));

    // Render the frames as the events the frontend already listens for. The
    // payload shapes are unchanged, so no TypeScript moved with this.
    tauri::async_runtime::spawn(render_turn_frames(
        app_handle,
        frame_rx,
        conversation_id,
        message_id.clone(),
        store_for_metadata,
    ));

    Ok(message_id)
}

/// The per-turn frame pump: tokens and the terminal frame go to the ONE
/// renderer unchanged; prompts and notices map onto the event vocabulary
/// the frontend already listens for, payload for payload — the same cards
/// the in-process `TauriApprovalChannel` used to raise. The routing
/// notices also record session→conversation, which is how a redirect
/// click (a session id alone) finds its socket.
async fn pump_wire_frames(
    app_handle: tauri::AppHandle,
    state: Arc<AppState>,
    mut stream: sovereign_turn_client::TurnStream,
    conversation_id: String,
    frames: tokio::sync::mpsc::UnboundedSender<TurnFrame>,
) {
    loop {
        let frame = match stream.next_frame().await {
            Ok(Some(f)) => f,
            _ => break, // socket closed; the renderer speaks next
        };
        match &frame {
            TurnFrame::Prompt { id, prompt } => {
                state.pending_prompts.write().await.insert(id.clone());
                raise_prompt_card(&app_handle, &conversation_id, id, prompt);
            }
            TurnFrame::Notice { notice } => {
                use sovereign_contracts::types::TurnNotice;
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
                    TurnNotice::ResolveAck { id, outcome } => {
                        // The ack cleared the pending key; a stale submit
                        // now fails fast instead of guessing.
                        if *outcome == sovereign_contracts::types::ResolveOutcome::Resolved {
                            state.pending_prompts.write().await.remove(id);
                        }
                    }
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
                    _ => {}
                }
            }
            _ => {}
        }
        let _ = frames.send(frame);
    }
    *state.turn_wire.write().await = None;
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
                );
                // Sidebar: updated_at bumped; title may auto-update.
                let _ = app.emit("conversations:changed", ());
                return;
            }
            // No narration channel is installed, and queue position is a
            // shared-hub concern. A Prompt belongs to the ATTACH path —
            // in-process, this host's own `TauriApprovalChannel` raises
            // the card and no frame is ever produced (rung 6 commit C2
            // wires the attached side). Notices render when their
            // daemon-side producers exist (sv-surface R3).
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
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    };
    let turn = sovereign_turn_client::TurnClient::new(state.client_base_url())
        .send_message(&conversation_id, &augmented_message)
        .await
        .map_err(|e| e.to_string())?;

    // Notify the sidebar — updated_at bumped, title may be auto-generated
    // asynchronously. A second event fires when the title lands (runtime
    // spawns the auto-title task independently, but we emit conservatively
    // here so list ordering refreshes immediately).
    let _ = app_handle.emit("conversations:changed", ());
    let _ = store; // held for the renderer parity below; the wire carried the rest

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
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
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
            live_sessions = ?runtime.sessions.conversation_ids(),
            "cancel_stream: nothing in flight — cancel is a no-op (already finished?)"
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
            let guard = require_runtime!(state);
            let runtime = guard.as_ref().unwrap();
            runtime
                .sessions
                .get(&session_id)
                .map(|s| s.conversation_id.clone())
                .ok_or_else(|| format!("session {session_id} not found"))?
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
