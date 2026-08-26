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
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap().clone();
    drop(guard);

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

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<TurnFrame>();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<String>();
    let sink = DesktopTurnSink {
        frames: frame_tx,
        started: std::sync::Mutex::new(Some(started_tx)),
    };

    // ONE turn driver (TOPOLOGY §10 phase 6). This command used to acquire a
    // stream handle itself, drain it by hand, and carry its own fallback for
    // turns that refuse to stream — the same loop `serve_turn` implements and
    // the same fallback five other hosts each got subtly differently.
    let store_for_turn = store_for_metadata.clone();
    let conv_for_turn = conversation_id.clone();
    tauri::async_runtime::spawn(async move {
        let Some(store) = store_for_turn else {
            return;
        };
        sovereign_core::runtime::serve_turn(
            &runtime,
            store.as_ref(),
            &conv_for_turn,
            &augmented_message,
            mode,
            // The desktop never pins an intent; the router classifies.
            None,
            // No narration subscription on this surface yet — the frontend
            // renders progress from its own state machine.
            None,
            &sink,
        )
        .await;
    });

    // Render the frames as the events the frontend already listens for. The
    // payload shapes are unchanged, so no TypeScript moved with this.
    let app = app_handle.clone();
    let conv_for_events = conversation_id.clone();
    let fallback_id = pending_id.clone();
    tauri::async_runtime::spawn(async move {
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
                            conversation_id: conv_for_events.clone(),
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
                            conversation_id: conv_for_events.clone(),
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

                    // The persisted blob, read IN PROCESS. `serve_turn`
                    // projects typed provenance for callers across a socket;
                    // this one owns the store, and the frontend's
                    // `MessageCompletePayload.metadata` is the raw shape it
                    // has always received. Reading it here is what let this
                    // command adopt the shared driver without a frontend
                    // change.
                    let metadata = match (&store_for_metadata, &real_message_id) {
                        (Some(store), Some(id)) => {
                            message_metadata(store.as_ref(), &conv_for_events, id).await
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
                            conversation_id: conv_for_events.clone(),
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
                // shared-hub concern.
                TurnFrame::Narration { .. } | TurnFrame::QueuePosition { .. } => {}
            }
        }
    });

    // Return as soon as the turn HAS an id, not when it produces output — the
    // frontend puts its placeholder on screen and retrieval is most of a cold
    // turn's wait. A turn that never mints one (graceful guard, or the
    // document path, which has no id until it finishes) returns the pending
    // id immediately rather than holding the UI.
    let message_id = if streaming {
        started_rx.await.unwrap_or(pending_id)
    } else {
        pending_id
    };

    Ok(StreamStartedResponse {
        message_id,
        streaming,
    })
}

/// Bridges [`serve_turn`] to the Tauri event surface.
///
/// Two jobs: forward frames to the async task that renders them (the sink's
/// `emit` is synchronous and the metadata read is not), and publish the
/// message id the moment the turn acquires one, so the command can return it
/// before the first token arrives.
struct DesktopTurnSink {
    frames: tokio::sync::mpsc::UnboundedSender<TurnFrame>,
    started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<String>>>,
}

impl sovereign_core::runtime::TurnSink for DesktopTurnSink {
    fn emit(&self, frame: TurnFrame) {
        let _ = self.frames.send(frame);
    }

    fn on_turn_started(&self, message_id: &str) {
        if let Some(tx) = self
            .started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = tx.send(message_id.to_string());
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
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    state.approval.set_task_id(&conversation_id).await;

    let augmented_message =
        augment_for_turn(&state, &message, &context_chunks, &attached_files).await;

    // The SAME driver `send_message_stream` uses. These two commands answer
    // the same question from the same app and used to run different
    // pipelines — the streaming one and the non-streaming one — so the answer
    // depended on which button the user pressed.
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    }
    .ok_or("Store not ready")?;

    let turn = sovereign_core::runtime::collect_turn(
        runtime,
        store.as_ref(),
        &conversation_id,
        &augmented_message,
        TurnMode::Grounded,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    // Notify the sidebar — updated_at bumped, title may be auto-generated
    // asynchronously. A second event fires when the title lands (runtime
    // spawns the auto-title task independently, but we emit conservatively
    // here so list ordering refreshes immediately).
    let _ = app_handle.emit("conversations:changed", ());

    // The persisted blob, read in-process — the frontend's `metadata` field
    // is the raw shape it has always received (see `send_message_stream`).
    let metadata = message_metadata(store.as_ref(), &conversation_id, &turn.message_id).await;

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
        metadata,
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
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap().clone();
    drop(guard);

    let store_for_metadata = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let handle = runtime
        .redirect_turn_stream(&session_id, &intent_hint)
        .await
        .map_err(|e| e.to_string())?;

    let message_id = handle.message_id.clone();
    let message_id_for_return = handle.message_id.clone();
    let conversation_id_owned = {
        // Pull conversation_id from the session so we know where
        // chunks should be routed. The session lookup above already
        // confirmed it exists.
        runtime
            .sessions
            .get(&session_id)
            .map(|s| s.conversation_id.clone())
            .unwrap_or_default()
    };
    let app = app_handle.clone();
    let mut stream = handle.stream;
    let store_ref = store_for_metadata.clone();

    tauri::async_runtime::spawn(async move {
        let mut full_text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    full_text.push_str(&chunk);
                    let _ = app.emit(
                        "message-chunk",
                        MessageChunkPayload {
                            conversation_id: conversation_id_owned.clone(),
                            message_id: message_id.clone(),
                            chunk,
                        },
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "message-error",
                        MessageErrorPayload {
                            conversation_id: conversation_id_owned.clone(),
                            message_id: message_id.clone(),
                            message: e.to_string(),
                        },
                    );
                    return;
                }
            }
        }

        let metadata = match store_ref {
            Some(ref store) => {
                message_metadata(store.as_ref(), &conversation_id_owned, &message_id).await
            }
            None => None,
        };

        let _ = app.emit(
            "message-complete",
            MessageCompletePayload {
                conversation_id: conversation_id_owned,
                message_id,
                full_text,
                metadata,
            },
        );
        let _ = app.emit("conversations:changed", ());
    });

    Ok(StreamStartedResponse {
        message_id: message_id_for_return,
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
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap().clone();
    drop(guard);

    state.approval.set_task_id(&conversation_id).await;

    let store_for_metadata = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let resume = sovereign_core::types::ResumeSession {
        session_id,
        intent_hint,
    };
    let handle = runtime
        .resume_session_stream(&message, &conversation_id, resume)
        .await
        .map_err(|e| e.to_string())?;

    let message_id = handle.message_id.clone();
    let message_id_for_return = handle.message_id.clone();
    let conversation_id_owned = conversation_id.clone();
    let app = app_handle.clone();
    let mut stream = handle.stream;
    let store_ref = store_for_metadata.clone();

    tauri::async_runtime::spawn(async move {
        let mut full_text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    full_text.push_str(&chunk);
                    let _ = app.emit(
                        "message-chunk",
                        MessageChunkPayload {
                            conversation_id: conversation_id_owned.clone(),
                            message_id: message_id.clone(),
                            chunk,
                        },
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "message-error",
                        MessageErrorPayload {
                            conversation_id: conversation_id_owned.clone(),
                            message_id: message_id.clone(),
                            message: e.to_string(),
                        },
                    );
                    return;
                }
            }
        }

        let metadata = match store_ref {
            Some(ref store) => {
                message_metadata(store.as_ref(), &conversation_id_owned, &message_id).await
            }
            None => None,
        };

        let _ = app.emit(
            "message-complete",
            MessageCompletePayload {
                conversation_id: conversation_id_owned,
                message_id,
                full_text,
                metadata,
            },
        );
        let _ = app.emit("conversations:changed", ());
    });

    Ok(StreamStartedResponse {
        message_id: message_id_for_return,
        streaming: true,
    })
}
