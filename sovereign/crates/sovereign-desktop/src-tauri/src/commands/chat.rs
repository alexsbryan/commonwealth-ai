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

    // Naked mode (a user setting) runs the loaded model raw — no
    // retrieval, router, grounding gate, tools, atlas, or gap-check.
    // Otherwise the full situated streaming path. Both return the same
    // StreamHandle, so the forwarding below is identical.
    let naked_mode = state.config.read().await.naked_mode;
    let stream_result = if naked_mode {
        tracing::info!(%conversation_id, "send_message_stream: NAKED mode — raw model, affordances bypassed");
        runtime
            .handle_message_stream_naked(&augmented_message, &conversation_id)
            .await
    } else {
        runtime
            .handle_message_stream(&augmented_message, &conversation_id)
            .await
    };

    // Try streaming path first.
    match stream_result {
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

                // Strip phantom tool-call envelopes the chat model reflexes
                // (`<tool_call>`/`<tool_code>`/`:code_search(...)`) for code/lookup
                // questions — chat wires no executable tools, so the raw call must
                // not leak; if that WAS the whole answer, an honest fallback shows.
                //
                // EXEMPT the recipe-author workspace (intent=RecipeAuthor): that
                // path passes REAL tools and parses + EXECUTES tool calls from the
                // assistant's prose server-side (handlers/recipe_author.rs) BEFORE
                // this point, so present_answer must not touch its display — doing
                // so could strip a legitimate tool envelope or mis-fire the
                // fallback. (The runtime gate-output strip is already exempt — that
                // path uses inference.complete, never the gated stream.)
                let is_recipe_author = metadata
                    .as_ref()
                    .and_then(|m| m.get("intent"))
                    .and_then(|v| v.as_str())
                    == Some("RecipeAuthor");
                let full_text = if is_recipe_author {
                    full_text
                } else {
                    sovereign_core::pipeline::presenter::present_answer(&full_text)
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
                        let content = response.message.content;
                        // Emit the body as a single chunk first, so the
                        // stream-integrity contract — concat(message-chunk)
                        // == full_text — holds for the non-streaming fallback
                        // exactly as it does for the streaming path above. The
                        // frontend accumulates chunks identically either way.
                        let _ = app.emit(
                            "message-chunk",
                            MessageChunkPayload {
                                conversation_id: conversation_id_owned.clone(),
                                message_id: pending_clone.clone(),
                                chunk: content.clone(),
                            },
                        );
                        let _ = app.emit(
                            "message-complete",
                            MessageCompletePayload {
                                conversation_id: conversation_id_owned,
                                message_id: pending_clone.clone(),
                                full_text: content,
                                metadata: response.message.metadata,
                            },
                        );
                    }
                    Err(e) => {
                        // A rejected oversize message lands here. That is a
                        // NORMAL user action (a big paste), not a system
                        // failure — so present the runtime's guidance as a calm
                        // assistant turn (the hint is written to be shown
                        // unchanged), NOT a raw "Error: Invalid input:" bubble
                        // that reads as a crash. Every other error keeps the
                        // diagnostic "Error:" framing. Either way it is a
                        // contract-compliant turn: one chunk so concat ==
                        // full_text, plus an `intent` marker so the turn is
                        // visible to the provenance surface (and clears the
                        // loading state) instead of an intent-less blank.
                        let oversize = matches!(
                            &e,
                            sovereign_core::Error::InvalidInput(m)
                                if m.as_str() == sovereign_core::runtime::OVERSIZE_MESSAGE_HINT
                        );
                        let (body, intent) = if oversize {
                            (
                                sovereign_core::runtime::OVERSIZE_MESSAGE_HINT.to_string(),
                                "oversize_guidance",
                            )
                        } else {
                            (format!("Error: {e}"), "error")
                        };
                        let _ = app.emit(
                            "message-chunk",
                            MessageChunkPayload {
                                conversation_id: conversation_id_owned.clone(),
                                message_id: pending_clone.clone(),
                                chunk: body.clone(),
                            },
                        );
                        let _ = app.emit(
                            "message-complete",
                            MessageCompletePayload {
                                conversation_id: conversation_id_owned,
                                message_id: pending_clone.clone(),
                                full_text: body,
                                metadata: Some(serde_json::json!({ "intent": intent })),
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
