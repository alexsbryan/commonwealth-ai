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

#[tauri::command]
pub async fn send_message_stream(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
    context_chunks: Option<Vec<FocusedChunkRef>>,
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
    let augmented_message = match &context_chunks {
        Some(refs) if !refs.is_empty() => {
            build_context_augmented_message(&state, &message, refs).await
        }
        _ => message.clone(),
    };

    // Try streaming path first.
    match runtime
        .handle_message_stream(&augmented_message, &conversation_id)
        .await
    {
        Ok(handle) => {
            tracing::info!(
                message_id = %handle.message_id,
                %conversation_id,
                "send_message_stream: streaming path engaged"
            );
            let message_id = handle.message_id.clone();
            let conversation_id_owned = conversation_id.clone();
            let app = app_handle.clone();
            let mut stream = handle.stream;
            let store_ref = store_for_metadata.clone();

            tauri::async_runtime::spawn(async move {
                use futures::StreamExt;
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
                                crate::approval::ErrorPayload {
                                    message: e.to_string(),
                                },
                            );
                            return;
                        }
                    }
                }

                // Fetch the saved message's metadata (includes retrieved_chunks
                // and provenance, persisted by handle_message_stream).
                let metadata = if let Some(ref store) = store_ref {
                    store
                        .get_conversation(&conversation_id_owned)
                        .await
                        .ok()
                        .and_then(|c| {
                            c.messages
                                .iter()
                                .find(|m| m.id == message_id)
                                .and_then(|m| m.metadata.clone())
                        })
                } else {
                    None
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
                // Sidebar: updated_at bumped; title may auto-update shortly.
                let _ = app.emit("conversations:changed", ());
            });

            Ok(StreamStartedResponse {
                message_id: handle.message_id,
                streaming: true,
            })
        }
        Err(_not_streamable) => {
            tracing::info!(
                %conversation_id,
                "send_message_stream: runtime not streamable, falling back to non-streaming (ComplexTask)"
            );
            // Fall back to non-streaming for ComplexTask.
            let app = app_handle.clone();
            let conversation_id_owned = conversation_id.clone();
            let runtime = runtime.clone();
            let message_owned = message.clone();
            let pending_id = uuid::Uuid::new_v4().to_string();
            let pending_clone = pending_id.clone();

            tauri::async_runtime::spawn(async move {
                match runtime
                    .handle_message(&message_owned, &conversation_id_owned)
                    .await
                {
                    Ok(response) => {
                        // Use pending_clone as the message_id — the frontend
                        // created a placeholder with this ID and the guard
                        // check in the message-complete handler matches on it.
                        let _ = app.emit(
                            "message-complete",
                            MessageCompletePayload {
                                conversation_id: conversation_id_owned,
                                message_id: pending_clone.clone(),
                                full_text: response.message.content,
                                metadata: response.message.metadata,
                            },
                        );
                    }
                    Err(e) => {
                        // Clear the loading state on error too.
                        let _ = app.emit(
                            "message-complete",
                            MessageCompletePayload {
                                conversation_id: conversation_id_owned,
                                message_id: pending_clone.clone(),
                                full_text: format!("Error: {e}"),
                                metadata: None,
                            },
                        );
                    }
                }
                // Sidebar refresh for both branches.
                let _ = app.emit("conversations:changed", ());
                drop(pending_clone);
            });

            Ok(StreamStartedResponse {
                message_id: pending_id,
                streaming: false,
            })
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

#[tauri::command]
pub async fn send_message(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
    context_chunks: Option<Vec<FocusedChunkRef>>,
) -> Result<MessageResponse, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    state.approval.set_task_id(&conversation_id).await;

    let augmented_message = match &context_chunks {
        Some(refs) if !refs.is_empty() => {
            build_context_augmented_message(&state, &message, refs).await
        }
        _ => message.clone(),
    };

    let response = runtime
        .handle_message(&augmented_message, &conversation_id)
        .await
        .map_err(|e| e.to_string())?;

    // Notify the sidebar — updated_at bumped, title may be auto-generated
    // asynchronously. A second event fires when the title lands (runtime
    // spawns the auto-title task independently, but we emit conservatively
    // here so list ordering refreshes immediately).
    let _ = app_handle.emit("conversations:changed", ());

    let task_summary = response.task.map(|t| TaskSummary {
        id: t.id,
        status: format!("{:?}", t.status),
        steps_completed: t.completed_steps.len(),
    });

    let role = response.message.role_str().to_string();
    Ok(MessageResponse {
        message_id: response.message.id.clone(),
        role,
        content: response.message.content.clone(),
        task: task_summary,
        metadata: response.message.metadata,
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
    if let Some(session) = runtime.sessions.latest_for_conversation(&conversation_id) {
        tracing::info!(
            session_id = %session.id,
            conversation_id,
            "cancel_stream: user requested abort"
        );
        session.cancel.cancel();
    } else {
        tracing::debug!(
            conversation_id,
            "cancel_stream: no live session (stream may have already finished)"
        );
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
                        crate::approval::ErrorPayload {
                            message: e.to_string(),
                        },
                    );
                    return;
                }
            }
        }

        let metadata = if let Some(ref store) = store_ref {
            store
                .get_conversation(&conversation_id_owned)
                .await
                .ok()
                .and_then(|c| {
                    c.messages
                        .iter()
                        .find(|m| m.id == message_id)
                        .and_then(|m| m.metadata.clone())
                })
        } else {
            None
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
                        crate::approval::ErrorPayload {
                            message: e.to_string(),
                        },
                    );
                    return;
                }
            }
        }

        let metadata = if let Some(ref store) = store_ref {
            store
                .get_conversation(&conversation_id_owned)
                .await
                .ok()
                .and_then(|c| {
                    c.messages
                        .iter()
                        .find(|m| m.id == message_id)
                        .and_then(|m| m.metadata.clone())
                })
        } else {
            None
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
