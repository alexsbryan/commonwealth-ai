use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::state::{self, AppState, DesktopConfig};

// ─── Response Types ──────────────────────────────────────────

#[derive(Serialize)]
pub struct MessageResponse {
    pub message_id: String,
    pub role: String,
    pub content: String,
    pub task: Option<TaskSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub status: String,
    pub steps_completed: usize,
}

#[derive(Serialize)]
pub struct ConversationEntry {
    pub id: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct ConversationDetail {
    pub id: String,
    pub title: Option<String>,
    pub messages: Vec<MessageEntry>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct MessageEntry {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct CreateConversationResponse {
    pub id: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub content: String,
    pub conversation_id: String,
}

#[derive(Serialize)]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub trust_level: String,
}

#[derive(Deserialize)]
pub struct SetupConfig {
    pub model_path: String,
    #[serde(default)]
    pub primary_model_path: Option<String>,
    #[serde(default)]
    pub embed_model_path: Option<String>,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub active_skills: Vec<String>,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub search_provider: Option<String>,
    #[serde(default)]
    pub search_api_key: Option<String>,
    #[serde(default)]
    pub selected_tier: Option<String>,
    /// M3 — opt-in for the Recipe Author workspace. `None` from a
    /// wizard step that doesn't surface the toggle preserves the
    /// existing `DesktopConfig.enable_recipe_authoring` value rather
    /// than silently defaulting to `false`.
    #[serde(default)]
    pub enable_recipe_authoring: Option<bool>,
}

#[derive(Serialize)]
pub struct CorpusEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_compressed_gb: f64,
    pub size_indexed_gb: f64,
    pub license: String,
    pub tiers: Vec<String>,
    /// "installed", "installing", or "not_installed".
    pub status: String,
    /// Chunk count when installed; null otherwise.
    pub chunks_count: Option<u64>,
    /// True when the recipe enables the epistemic enrichment phase.
    pub enrichment_enabled: bool,
    /// Unix timestamp (seconds) when the index was created. Null unless installed.
    pub indexed_at: Option<u64>,
    /// Embedding model name used when indexing. Null unless installed.
    pub embedding_model: Option<String>,
    /// Embedding vector dimensions. Null unless installed.
    pub embedding_dimensions: Option<usize>,
    /// True when the IVF-PQ vector index is built and semantic search is available.
    /// False means FTS-only search is used (fast but keyword-only).
    pub vector_index_ready: bool,
    /// URL of the recipe TOML in the public registry. Null for user-added corpora.
    pub registry_url: Option<String>,
    /// Recipe schema version (1 = initial). Used for compatibility checks.
    pub schema_version: Option<u32>,
    /// Parent corpus id when this entry is a layer/satellite (e.g.
    /// `wikipedia-simple` and `wikipedia-newsworthy` carry
    /// `parent_corpus_id = "wikipedia"`). The desktop hides children
    /// from the top-level picker and surfaces them as toggles under
    /// the parent's row. `null` for top-level corpora.
    pub parent_corpus_id: Option<String>,
}

/// Detailed health report for a single installed corpus, loaded on demand
/// (avoids opening every LanceDB index on every `list_corpora` call).
#[derive(Serialize)]
pub struct CorpusHealthDetail {
    pub corpus_id: String,
    /// Number of extracted claims (0 if no claims table).
    pub claims_count: u64,
    /// Number of stored relationships (0 if no relationships table).
    pub relationships_count: u64,
    /// True if an article_profiles table exists (structured Wikipedia only).
    pub has_article_profiles: bool,
    /// Number of chunks whose enrichment parse failed and can be retried
    /// without re-running inference (0 if no failures file exists).
    pub parse_failure_count: u64,
}

/// Progress payload sent to the frontend during a corpus install.
/// `phase` covers the entire pipeline including enrichment, so the
/// download bar can keep moving through claim and relationship
/// extraction rather than appearing to stall after "indexing".
#[derive(Serialize, Clone)]
pub struct CorpusProgressPayload {
    pub corpus_id: String,
    /// One of: "downloading", "extracting", "chunking", "embedding",
    /// "indexing", "extracting_claims", "finding_relationships",
    /// "extracting_relationships", "complete", "failed".
    pub phase: String,
    pub percent: f32,
    pub chunks_processed: u64,
    /// Optional human-readable status line for the more verbose phases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Tier groupings for the desktop knowledge picker. Pure UI metadata —
/// the engine doesn't care about tiers.
fn tiers_for(corpus_id: &str) -> Vec<String> {
    match corpus_id {
        // Wikipedia Core ships in every tier — its scoped 100K + Vital
        // Articles is the baseline general-knowledge corpus.
        "wikipedia" => vec![
            "essential".into(),
            "research".into(),
            "technical".into(),
            "full".into(),
        ],
        // Simple English ships alongside Core in every tier — Layer 0 of
        // the layered Wikipedia stack, ready for chat in 2-3 min.
        "wikipedia-simple" => vec![
            "essential".into(),
            "research".into(),
            "technical".into(),
            "full".into(),
        ],
        "sep" => vec!["research".into(), "full".into()],
        "openalex" => vec!["research".into(), "full".into()],
        "stackexchange" => vec!["technical".into(), "full".into()],
        "gutenberg" => vec!["full".into()],
        "crs_reports" => vec!["research".into(), "full".into()],
        _ => vec!["full".into()],
    }
}

// ─── Helpers ─────────────────────────────────────────────────

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

macro_rules! require_runtime {
    ($state:expr) => {{
        let guard = $state.runtime.read().await;
        if guard.is_none() {
            return Err("Backend is still loading. Please wait.".to_string());
        }
        guard
    }};
}

// ─── Hardware Detection ─────────────────────────────────────

#[derive(Serialize)]
pub struct HardwareInfo {
    pub system_ram_gb: f64,
    pub gpu_available: bool,
    pub gpu_name: Option<String>,
}

#[tauri::command]
pub async fn detect_hardware() -> Result<HardwareInfo, String> {
    let profile = tokio::task::spawn_blocking(|| {
        sovereign_inference::hardware::HardwareProfile::detect()
    })
    .await
    .map_err(|e| format!("Hardware detection failed: {e}"))?;

    Ok(HardwareInfo {
        system_ram_gb: profile.system_ram_gb(),
        gpu_available: profile.gpu_available,
        gpu_name: profile.gpu_name,
    })
}

/// Expose the result of `bootstrap::detect` to the frontend so the
/// setup wizard can skip screens that are already covered by the
/// CLI-written `SetupConfig`. Called once at app start (or any time
/// the wizard wants to re-probe, e.g. after the user runs
/// `sovereign setup` in a terminal).
#[tauri::command]
pub async fn detect_bootstrap() -> Result<crate::bootstrap::BootstrapSnapshot, String> {
    let mode = crate::bootstrap::detect().await;
    Ok(crate::bootstrap::BootstrapSnapshot::from(&mode))
}

/// Eagerly load the primary chat slot so the next chat-completions
/// call doesn't pay the lazy-load tax.
///
/// Idempotent and fire-and-forget from the UI's perspective —
/// callers don't await on the load. The frontend dispatches this
/// on window-focus and ChatView mount so the slot is hot by the
/// time the user finishes typing.
///
/// Returns immediately as `Ok(())` when no inference provider has
/// been configured yet (pre-setup wizard, model files missing) so
/// the focus handler can stay a fire-and-forget without surfacing
/// errors that aren't user-actionable.
#[tauri::command]
pub async fn warmup_primary_slot(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let provider = {
        let guard = state.inference.read().await;
        guard.as_ref().map(Arc::clone)
    };
    let Some(provider) = provider else {
        // Setup hasn't run / model files unconfigured. Fire-and-
        // forget contract — this isn't an error from the UI's
        // perspective, just nothing to warm.
        return Ok(());
    };
    // Spawn so the Tauri command returns immediately. The load can
    // take 10–90s; we don't want the focus handler to block on it.
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match provider.warmup_primary().await {
            Ok(()) => tracing::info!(
                latency_ms = started.elapsed().as_millis() as u64,
                "warmup_primary_slot: complete"
            ),
            Err(e) => tracing::warn!(error = %e, "warmup_primary_slot: failed"),
        }
    });
    Ok(())
}

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

    format!(
        "{}\n\n---\n\n{}",
        blocks.join("\n\n---\n\n"),
        user_message
    )
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

#[tauri::command]
pub async fn create_conversation() -> Result<CreateConversationResponse, String> {
    Ok(CreateConversationResponse {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: now_epoch(),
    })
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<ConversationEntry>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let convos = runtime
        .store
        .list_conversations(limit.unwrap_or(50), offset.unwrap_or(0))
        .await
        .map_err(|e| e.to_string())?;

    Ok(convos
        .into_iter()
        .map(|c| ConversationEntry {
            id: c.id,
            title: c.title,
            created_at: c.created_at,
            updated_at: c.updated_at,
        })
        .collect())
}

#[tauri::command]
pub async fn get_conversation(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<ConversationDetail, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let convo = runtime
        .store
        .get_conversation(&conversation_id)
        .await
        .map_err(|e| e.to_string())?;

    Ok(ConversationDetail {
        id: conversation_id,
        title: convo.title,
        messages: convo
            .messages
            .into_iter()
            .map(|m| {
                let role = m.role_str().to_string();
                MessageEntry {
                    id: m.id,
                    role,
                    content: m.content,
                    created_at: m.created_at,
                    metadata: m.metadata,
                }
            })
            .collect(),
        created_at: convo.created_at,
        updated_at: convo.updated_at,
    })
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    runtime
        .store
        .delete_conversation(&conversation_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rename_conversation(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    title: String,
) -> Result<(), String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("Title cannot be empty".to_string());
    }
    // Guard against unreasonably long titles.
    let title = if trimmed.chars().count() > 200 {
        trimmed.chars().take(200).collect::<String>()
    } else {
        trimmed.to_string()
    };

    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    runtime
        .store
        .update_conversation_title(&conversation_id, &title)
        .await
        .map_err(|e| e.to_string())?;

    let _ = app_handle.emit("conversations:changed", ());
    Ok(())
}

#[tauri::command]
pub async fn search_messages(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<SearchResult>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let messages = runtime
        .store
        .search_messages(&query)
        .await
        .map_err(|e| e.to_string())?;

    Ok(messages
        .into_iter()
        .take(50)
        .map(|m| SearchResult {
            content: m.content,
            conversation_id: m.conversation_id,
        })
        .collect())
}

#[tauri::command]
pub async fn submit_approval(
    state: State<'_, Arc<AppState>>,
    key: String,
    approved: bool,
) -> Result<bool, String> {
    Ok(state.approval.submit_approval(&key, approved).await)
}

#[tauri::command]
pub async fn submit_input(
    state: State<'_, Arc<AppState>>,
    key: String,
    response: String,
) -> Result<bool, String> {
    Ok(state.approval.submit_input(&key, response).await)
}

/// Resolve a pending information-request the agent surfaced via an
/// `AwaitUserInfo` step. `content = None` means the user pressed skip;
/// `Some(text)` means they pasted a passage / paragraph / source.
/// Returns true when the key was matched, false when no pending request
/// exists for that key (e.g. stale UI submission).
#[tauri::command]
pub async fn submit_information_response(
    state: State<'_, Arc<AppState>>,
    key: String,
    content: Option<String>,
) -> Result<bool, String> {
    Ok(state
        .approval
        .submit_information_response(&key, content)
        .await)
}

/// Trigger memory extraction on a finished inner-work conversation.
///
/// Until 2026-05-05 the desktop had no path to invoke memory
/// extraction — `Runtime::end_conversation` was called only from the
/// CLI, so a desktop-only inner-work user accumulated zero
/// long-term memory across sessions despite the storage and recall
/// pipelines being fully wired. This command closes that gap.
///
/// Caller is `InnerWorkSurface.onDestroy`. Best-effort: we ignore
/// errors at the runtime layer so a failure here doesn't stall the
/// surface unmount. The runtime's own `end_conversation` is a no-op
/// when the conversation has fewer than 4 messages, so empty inner-
/// work entries don't trigger extraction noise.
///
/// The skill_id wall is enforced inside `Runtime::end_conversation`:
/// each extracted memory is stamped with `source_skill_id` =
/// `conversations.skill_id`. Inner-work conversations therefore
/// produce inner-work-scoped memories, never general-pool ones.
#[tauri::command]
pub async fn finalize_inner_work_conversation(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    if let Err(e) = runtime.end_conversation(&conversation_id).await {
        tracing::warn!(
            error = %e,
            conversation_id = %conversation_id,
            "finalize_inner_work_conversation: extraction failed"
        );
    }
    Ok(())
}

/// Tombstone a memory the user has flagged as wrong. Soft-delete via
/// `delete_memory` (sets `deleted_at`) — the row is preserved for
/// audit but excluded from all recall paths. Used by the inner-work
/// "drop this memory" affordance.
#[tauri::command]
pub async fn forget_memory(
    state: State<'_, Arc<AppState>>,
    memory_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    runtime
        .store
        .delete_memory(&memory_id)
        .await
        .map_err(|e| e.to_string())
}

/// Halve the confidence of a memory. Used by the "this is partly
/// right but the witness over-extrapolated" path — the memory stays
/// recallable but with reduced weight, and the standard decay floor
/// will eventually prune it if the user keeps weakening.
#[tauri::command]
pub async fn weaken_memory(
    state: State<'_, Arc<AppState>>,
    memory_id: String,
) -> Result<(), String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    let all = runtime
        .store
        .get_all_memories()
        .await
        .map_err(|e| e.to_string())?;
    let current = all
        .iter()
        .find(|m| m.id == memory_id)
        .ok_or_else(|| format!("memory {memory_id} not found"))?;
    let new_conf = (current.confidence * 0.5).max(0.0);
    runtime
        .store
        .update_memory_confidence(&memory_id, new_conf)
        .await
        .map_err(|e| e.to_string())
}

/// Glassbox: return the most recent witness-turn provenance the
/// runtime captured for `conversation_id`, if any.
///
/// Used by the desktop's inner-work surface bound to Cmd+? to surface
/// "what did the model actually see" — the assembled system prompt,
/// the recalled memories, the conversation history slice (today: empty
/// — the streaming witness path doesn't pass prior turns to the
/// model), the model id + token budget, and Pass A timing.
///
/// Returns `Ok(None)` when no provenance is recorded for that
/// conversation in this Runtime's lifetime — typically because the
/// conversation hasn't received a streaming witness response yet, or
/// because it ran on the non-streaming path (we don't capture there
/// today; mirror the capture in `handle_expressive_query` if needed).
#[tauri::command]
pub async fn get_last_turn_provenance(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<Option<sovereign_core::runtime::TurnProvenance>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();
    Ok(runtime.get_last_turn_provenance(&conversation_id))
}

#[tauri::command]
pub async fn list_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillEntry>, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    let all_skills = runtime.skills.list();
    let active_ids: Vec<String> = runtime
        .skills
        .active_skills()
        .iter()
        .map(|s| s.id.clone())
        .collect();

    Ok(all_skills
        .into_iter()
        .map(|s| SkillEntry {
            active: active_ids.contains(&s.id),
            id: s.id.clone(),
            name: s.name.clone(),
            description: s.description.clone(),
            trust_level: format!("{:?}", s.trust_level).to_lowercase(),
        })
        .collect())
}

#[tauri::command]
pub async fn toggle_skill(
    state: State<'_, Arc<AppState>>,
    skill_id: String,
    active: bool,
) -> Result<(), String> {
    // Update the config's active_skills list and rebuild Runtime.
    {
        let mut config = state.config.write().await;
        if active {
            if !config.active_skills.contains(&skill_id) {
                config.active_skills.push(skill_id);
            }
        } else {
            config.active_skills.retain(|id| *id != skill_id);
        }
        config.save()?;
    }

    state::rebuild_runtime(&state).await
}

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<DesktopConfig, String> {
    Ok(state.config.read().await.clone())
}

#[tauri::command]
pub async fn save_config(
    state: State<'_, Arc<AppState>>,
    config: DesktopConfig,
) -> Result<(), String> {
    config.save()?;
    let old = state.config.read().await.clone();
    let old_embed = old.embed_model_path.clone();
    let new_embed = config.embed_model_path.clone();

    // Mirror shared fields (model paths + data_dir) into SetupConfig
    // on disk, then ask the daemon to hot-reload. This is the
    // "desktop Settings → daemon stays up" path: the user picks a new
    // primary model and the CLI-owned daemon swaps its provider
    // without the UI seeing a restart gap.
    //
    // Best-effort: a failure to write SetupConfig or to reach the
    // daemon must not block the desktop's local save. We log and
    // move on — next desktop save attempt will retry the mirror.
    if let Err(e) = mirror_to_setup_config(&config).await {
        tracing::warn!("save_config: could not mirror to SetupConfig: {e}");
    }
    if let Err(e) = request_daemon_reload().await {
        tracing::warn!("save_config: admin/reload failed: {e}");
    }

    *state.config.write().await = config;
    // If the embedding model changed, drop the cached inference so bootstrap
    // reloads it with the new embed model path.
    if old_embed != new_embed {
        *state.inference.write().await = None;
    }
    state::rebuild_runtime(&state).await
}

/// Mirror the three model paths + data_dir from `DesktopConfig` into
/// `SetupConfig`. Creates the config file on first write if it didn't
/// exist (matches `sovereign setup` behaviour). Leaves `daemon`
/// defaults in place — port changes go through the CLI's `sovereign
/// setup`, not the desktop Settings panel.
async fn mirror_to_setup_config(
    desktop: &DesktopConfig,
) -> Result<(), String> {
    use sovereign_core::setup_config::{
        DaemonSection, DataSection, ModelsSection, SetupConfig,
    };

    let mut cli = SetupConfig::load().unwrap_or_else(|_| SetupConfig {
        models: ModelsSection {
            primary: desktop
                .primary_model_path
                .clone()
                .unwrap_or_else(|| desktop.model_path.clone()),
            fast: desktop.model_path.clone(),
            embed: desktop
                .embed_model_path
                .clone()
                .unwrap_or_else(|| desktop.model_path.clone()),
            code: desktop.code_model_path.clone(),
            context_size: None,
            extra: std::collections::BTreeMap::new(),
            max_extras_memory_gb: None,
            primary_pool: None,
        },
        daemon: DaemonSection::default(),
        data: DataSection { dir: desktop.data_dir.clone() },
        watched_folders: Default::default(),
    });

    let mut changed = false;
    if cli.models.fast != desktop.model_path {
        cli.models.fast = desktop.model_path.clone();
        changed = true;
    }
    if let Some(p) = &desktop.primary_model_path {
        if &cli.models.primary != p {
            cli.models.primary = p.clone();
            changed = true;
        }
    }
    if let Some(e) = &desktop.embed_model_path {
        if &cli.models.embed != e {
            cli.models.embed = e.clone();
            changed = true;
        }
    }
    if cli.data.dir != desktop.data_dir {
        cli.data.dir = desktop.data_dir.clone();
        changed = true;
    }

    if changed {
        cli.save()?;
        tracing::info!("save_config: mirrored shared fields into SetupConfig");
    }
    Ok(())
}

/// POST `http://127.0.0.1:9741/v1/admin/reload` so a CLI-started
/// daemon picks up the `SetupConfig` changes we just wrote. When the
/// daemon replies `{restart_required: true}` — typically a port or
/// data_dir change — fall back to `launchctl kickstart` / `systemctl
/// --user restart`. Swallows all errors: if no daemon is running,
/// the next `sovereign daemon run` will read the fresh config anyway.
async fn request_daemon_reload() -> Result<(), String> {
    let url = "http://127.0.0.1:9741/v1/admin/reload";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let resp = client
        .post(url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| format!("POST admin/reload: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
    if !status.is_success() {
        return Err(format!("admin/reload returned {status}: {body}"));
    }
    let restart_required = body
        .get("restart_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    tracing::info!(
        reloaded = ?body.get("reloaded_fields"),
        restart_required,
        "save_config: admin/reload completed"
    );
    if restart_required {
        if let Err(e) = kickstart_daemon() {
            tracing::warn!("save_config: kickstart fallback failed: {e}");
        }
    }
    Ok(())
}

/// Best-effort restart of the `sovereign-daemon` service. Used only
/// when the admin/reload handler reported `restart_required` (port
/// or data_dir change) — hot reload can't rebind listeners.
fn kickstart_daemon() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // `id -u` avoids pulling in `libc` just for `getuid()`.
        let uid_out = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| format!("spawn id: {e}"))?;
        let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
        let label = format!("gui/{uid}/com.sovereign.daemon");
        let out = Command::new("launchctl")
            .args(["kickstart", "-k", &label])
            .output()
            .map_err(|e| format!("spawn launchctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "launchctl kickstart {label} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let out = Command::new("systemctl")
            .args(["--user", "restart", "sovereign"])
            .output()
            .map_err(|e| format!("spawn systemctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user restart sovereign failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("service restart is only supported on macOS and Linux".into())
    }
}

#[tauri::command]
pub async fn is_setup_complete(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.config.read().await.setup_complete)
}

/// Auto-config first-launch flow. Takes no input — runs hardware
/// probe → catalog selection → 3-model download → DB open → model
/// load → smoke test, narrating progress on the `setup-progress`
/// Tauri event channel. Returns when the backend is ready to serve
/// chat. Drives the desktop's `SetupFlow.svelte` (the *lazy
/// sunbeam* onboarding flow); the legacy `complete_setup` stays
/// available for tests/scripts that hand-build a `SetupConfig`.
#[tauri::command]
pub async fn complete_setup_auto(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    crate::setup_flow::run(app_handle, state.inner().clone()).await
}

/// Fire-and-forget background install of the default
/// `wikipedia-simple` corpus (Layer 0, ~2–3 min). Idempotent —
/// `install_corpus` short-circuits when the daemon is already
/// ingesting it. The desktop's `App.svelte` calls this on the
/// transition into chat after first-launch setup completes; the
/// install runs silently with no setup-flow UI surface, surfacing
/// only on the regular `corpus-progress` channel that
/// `Settings → Knowledge` already listens to.
#[tauri::command]
pub async fn start_default_corpus_install(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    install_corpus(app_handle, state, "wikipedia-simple".into()).await
}

#[tauri::command]
pub async fn complete_setup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    setup: SetupConfig,
) -> Result<(), String> {
    // When the wizard skipped the model picker (because `SetupConfig`
    // from `sovereign setup` was detected), the incoming `setup` has
    // empty model paths. Fall back to what the CLI already wrote on
    // disk rather than clobbering the desktop config with empties.
    let cli_cfg = sovereign_core::setup_config::SetupConfig::load().ok();
    let mut config = state.config.write().await;
    if !setup.model_path.is_empty() {
        config.model_path = setup.model_path.into();
    } else if let Some(c) = cli_cfg.as_ref() {
        config.model_path = c.models.fast.clone();
    }
    config.primary_model_path = setup
        .primary_model_path
        .map(std::path::PathBuf::from)
        .or_else(|| cli_cfg.as_ref().map(|c| c.models.primary.clone()));
    config.embed_model_path = setup
        .embed_model_path
        .map(std::path::PathBuf::from)
        .or_else(|| cli_cfg.as_ref().map(|c| c.models.embed.clone()));
    if let Some(dir) = setup.data_dir {
        config.data_dir = dir.into();
    } else if let Some(c) = cli_cfg.as_ref() {
        config.data_dir = c.data.dir.clone();
    }
    config.active_skills = setup.active_skills;
    if !setup.enabled_tools.is_empty() {
        config.enabled_tools = setup.enabled_tools;
    }
    if let Some(provider) = setup.search_provider {
        config.search_backend.provider = provider;
    }
    config.search_backend.api_key = setup.search_api_key;
    config.selected_tier = setup.selected_tier.clone();
    if let Some(flag) = setup.enable_recipe_authoring {
        config.enable_recipe_authoring = flag;
    }
    config.setup_complete = true;

    config.save()?;
    drop(config);

    state::bootstrap(&state).await?;

    // Notify the frontend that the backend is ready so the loading screen unblocks.
    let _ = app_handle.emit("backend-ready", ());

    // Trigger background corpus installs for the selected tier.
    if let Some(ref tier) = setup.selected_tier {
        let tier = tier.clone();
        let state_ref = Arc::clone(&state);
        let app = app_handle.clone();
        tokio::spawn(async move {
            start_tier_installs(&app, &state_ref, &tier).await;
        });
    }

    Ok(())
}

// ─── Web Search ─────────────────────────────────────────────

#[tauri::command]
pub async fn search_web(
    state: State<'_, Arc<AppState>>,
    query: String,
    conversation_id: String,
) -> Result<MessageResponse, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    // Save user message.
    let user_msg = sovereign_core::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: sovereign_core::types::Role::User,
        content: query.clone(),
        created_at: now_epoch(),
        metadata: None,
        version: now_epoch(),
    };
    runtime
        .store
        .save_message(&user_msg)
        .await
        .map_err(|e| e.to_string())?;

    // Execute search tool directly.
    let tool = runtime
        .tools
        .get("search")
        .or(runtime.tools.get("web_search"))
        .map_err(|_| "Search tool is not enabled.".to_string())?;

    let params = serde_json::json!({ "query": query });
    let ctx = sovereign_core::types::ToolContext {
        conversation_id: conversation_id.clone(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
    };

    let output = tool
        .execute(&params, &ctx)
        .await
        .map_err(|e| format!("Web search failed: {e}"))?;

    let content = match output {
        sovereign_core::types::StepOutput::Text(t) => t,
        sovereign_core::types::StepOutput::Json(ref v) => v
            .get("answer")
            .and_then(|a| a.as_str())
            .unwrap_or_else(|| "No results found.")
            .to_string(),
        sovereign_core::types::StepOutput::ReasonWithToolsResult { text, .. } => text,
        _ => "No results found.".to_string(),
    };

    // Save assistant message.
    let msg_id = uuid::Uuid::new_v4().to_string();
    let assistant_msg = sovereign_core::types::Message {
        id: msg_id.clone(),
        conversation_id,
        role: sovereign_core::types::Role::Assistant,
        content: content.clone(),
        created_at: now_epoch(),
        metadata: None,
        version: now_epoch(),
    };
    runtime
        .store
        .save_message(&assistant_msg)
        .await
        .map_err(|e| e.to_string())?;

    Ok(MessageResponse {
        message_id: msg_id,
        role: "assistant".to_string(),
        content,
        task: None,
        metadata: None,
    })
}

// ─── Model Discovery & Download ─────────────────────────────

#[derive(Serialize)]
pub struct DiscoveredModel {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub location_label: String,
}

#[derive(Deserialize)]
pub struct DownloadRequest {
    pub url: String,
    pub file_name: String,
    /// Advertised file size from the model catalogue (models.toml
    /// / ModelSelector.svelte's EMBED_MODELS). Optional for back-
    /// compat; when present, `download_model` applies a 50% floor
    /// via `sovereign_inference::GgufExpectation::from_size_gb`
    /// so a CDN-served 200 KB HTML stub doesn't silently land at
    /// the final path as a "30 GB" model.
    #[serde(default)]
    pub size_gb: Option<f64>,
}

#[derive(Serialize, Clone)]
pub struct DownloadProgress {
    pub file_name: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<f64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Returns true iff `path` starts with the GGUF magic (`GGUF`, 4 bytes
/// ASCII).
///
/// Discovery scanners list every `*.gguf` file by extension, but a
/// failed download (HTML 404 page, captive-portal interstitial, Git-LFS
/// pointer) saved with a `.gguf` extension still slips through. The
/// picker would surface those as selectable options and the user would
/// land on `state::bootstrap` → `LlamaModel::load_from_file` → "null
/// result from llama cpp" with no path back. The magic-byte check is a
/// 4-byte read — much cheaper than `validate_gguf` (which also size-
/// checks) and sufficient to weed out non-GGUFs at discovery time.
fn looks_like_gguf(path: &Path) -> bool {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = [0u8; 4];
            f.read_exact(&mut buf).is_ok() && &buf == b"GGUF"
        }
        Err(_) => false,
    }
}

fn scan_directory_flat(dir: &Path, label: &str, results: &mut Vec<DiscoveredModel>, seen: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "gguf") {
            if !looks_like_gguf(&path) {
                tracing::warn!(
                    path = %path.display(),
                    "scan_directory_flat: skipping non-GGUF (likely failed download or LFS pointer)"
                );
                continue;
            }
            if let Ok(canonical) = path.canonicalize() {
                if seen.insert(canonical.clone()) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    results.push(DiscoveredModel {
                        path: canonical.display().to_string(),
                        file_name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                        size_bytes: size,
                        location_label: label.to_string(),
                    });
                }
            }
        }
    }
}

fn scan_directory_deep(dir: &Path, label: &str, max_depth: usize, results: &mut Vec<DiscoveredModel>, seen: &mut HashSet<PathBuf>) {
    if !dir.exists() { return; }
    for entry in walkdir::WalkDir::new(dir).max_depth(max_depth).into_iter().flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "gguf") {
            if !looks_like_gguf(path) {
                tracing::warn!(
                    path = %path.display(),
                    "scan_directory_deep: skipping non-GGUF (likely failed download or LFS pointer)"
                );
                continue;
            }
            if let Ok(canonical) = path.canonicalize() {
                if seen.insert(canonical.clone()) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    results.push(DiscoveredModel {
                        path: canonical.display().to_string(),
                        file_name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                        size_bytes: size,
                        location_label: label.to_string(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod scan_tests {
    use super::looks_like_gguf;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn looks_like_gguf_accepts_real_magic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("real.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"GGUF\x03\x00\x00\x00rest of file...").unwrap();
        assert!(looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_html() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stub.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"<!doctype html><html>404 Not Found</html>").unwrap();
        assert!(!looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_lfs_pointer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ptr.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 12345\n").unwrap();
        assert!(!looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_too_short_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("short.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"GG").unwrap();
        assert!(!looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.gguf");
        assert!(!looks_like_gguf(&path));
    }
}

#[tauri::command]
pub async fn scan_for_models() -> Result<Vec<DiscoveredModel>, String> {
    // Run filesystem scanning on a blocking thread.
    tokio::task::spawn_blocking(|| {
        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        // Priority 1: Sovereign models directory
        let sovereign_models = home.join(".sovereign").join("models");
        scan_directory_flat(&sovereign_models, "Sovereign Models", &mut results, &mut seen);

        // Priority 2: Local models/ directory
        let local_models = std::env::current_dir().unwrap_or_default().join("models");
        scan_directory_flat(&local_models, "Local Models", &mut results, &mut seen);

        // Priority 3: HuggingFace cache (deep scan, GGUF files nested in snapshots)
        let hf_cache = home.join(".cache").join("huggingface").join("hub");
        scan_directory_deep(&hf_cache, "HuggingFace Cache", 5, &mut results, &mut seen);

        // Priority 4: Downloads folder
        let downloads = home.join("Downloads");
        scan_directory_flat(&downloads, "Downloads", &mut results, &mut seen);

        Ok(results)
    })
    .await
    .map_err(|e| format!("Scan failed: {e}"))?
}

#[tauri::command]
pub async fn download_model(
    app_handle: tauri::AppHandle,
    request: DownloadRequest,
) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let models_dir = home.join(".sovereign").join("models");
    std::fs::create_dir_all(&models_dir)
        .map_err(|e| format!("Failed to create models directory: {e}"))?;

    let dest = models_dir.join(&request.file_name);
    let expected = match request.size_gb {
        Some(gb) => sovereign_inference::GgufExpectation::from_size_gb(gb),
        None => sovereign_inference::GgufExpectation::unknown(),
    };

    // Validate any pre-existing file at the destination. A stub
    // from a previous bad download (HTML error page, truncated
    // stream) must be deleted — the old early-return-on-exists
    // behaviour locked users into re-running setup from a clean
    // slate. Now we just re-download whatever's invalid.
    if dest.exists() {
        match sovereign_inference::validate_gguf(&dest, &expected) {
            Ok(()) => {
                let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
                let _ = app_handle.emit(
                    "download-progress",
                    DownloadProgress {
                        file_name: request.file_name,
                        downloaded_bytes: size,
                        total_bytes: Some(size),
                        percent: Some(100.0),
                        status: "complete".to_string(),
                        error: None,
                    },
                );
                return Ok(dest.display().to_string());
            }
            Err(e) => {
                tracing::warn!(
                    path = %dest.display(),
                    reason = %e,
                    "download_model: existing file failed validation, redownloading"
                );
                let _ = std::fs::remove_file(&dest);
            }
        }
    }

    let part_path = models_dir.join(format!("{}.part", &request.file_name));

    // Build the request with optional HF_TOKEN bearer auth.
    // Authenticated HF requests bypass anonymous rate-limits and
    // the CDN's bot-detection paths that return HTML.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("Build http client: {e}"))?;
    let mut req = client.get(&request.url);
    if let Ok(tok) = std::env::var("HF_TOKEN") {
        if !tok.is_empty() {
            req = req.bearer_auth(tok);
        }
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("Download request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }

    // Pre-stream content-type sniff. When HuggingFace returns an
    // error page (rate limit, bot detection, gated repo), the
    // body is HTML or JSON — catch it before streaming MB of
    // garbage to disk. The post-stream `validate_gguf` check
    // backstops this for cases where the server lies about
    // content-type.
    if let Some(ct) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let lower = ct.to_ascii_lowercase();
        if lower.starts_with("text/") || lower.starts_with("application/json") {
            return Err(format!(
                "HuggingFace returned content-type={ct} for {} — likely \
                 bot-detection, rate limiting, or a gated-repo login page. \
                 Set HF_TOKEN and retry, or try a different model.",
                request.url
            ));
        }
    }

    let total_bytes = response.content_length();
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| format!("Failed to create file: {e}"))?;

    let mut stream = response.bytes_stream();

    let mut last_emit: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download error: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write error: {e}"))?;
        downloaded += chunk.len() as u64;

        // Emit progress every ~200KB.
        if downloaded - last_emit >= 200_000 {
            last_emit = downloaded;
            let _ = app_handle.emit(
                "download-progress",
                DownloadProgress {
                    file_name: request.file_name.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes,
                    percent: total_bytes.map(|t| (downloaded as f64 / t as f64) * 100.0),
                    status: "downloading".to_string(),
                    error: None,
                },
            );
        }
    }

    file.flush().await.map_err(|e| format!("Flush error: {e}"))?;
    drop(file);

    // Post-stream validation. Covers CDN responses that advertised
    // `application/octet-stream` but delivered HTML, silent TCP
    // resets mid-body, and any other way a response can look
    // successful but not actually be a GGUF. On failure we delete
    // the `.part` so a retry starts clean rather than resuming a
    // partial bogus file.
    if let Err(e) = sovereign_inference::validate_gguf(&part_path, &expected) {
        let _ = tokio::fs::remove_file(&part_path).await;
        let msg = format!("download validation failed: {e}");
        let _ = app_handle.emit(
            "download-progress",
            DownloadProgress {
                file_name: request.file_name.clone(),
                downloaded_bytes: downloaded,
                total_bytes,
                percent: None,
                status: "error".to_string(),
                error: Some(msg.clone()),
            },
        );
        return Err(msg);
    }

    // Rename .part to final.
    tokio::fs::rename(&part_path, &dest)
        .await
        .map_err(|e| format!("Failed to finalize download: {e}"))?;

    let _ = app_handle.emit(
        "download-progress",
        DownloadProgress {
            file_name: request.file_name,
            downloaded_bytes: downloaded,
            total_bytes: Some(downloaded),
            percent: Some(100.0),
            status: "complete".to_string(),
            error: None,
        },
    );

    Ok(dest.display().to_string())
}

// ─── Corpus Management ──────────────────────────────────────
//
// All corpus operations route through the shared `CorpusEngine` stored
// in `AppState::corpus_engine`. The catalog of available corpora comes
// from the `RecipeRegistry` bundled snapshot (registry_snapshot.toml),
// and installed state comes from `installed_indexes()` scanning
// `~/.sovereign/indexes`. The legacy `CorpusManager` /
// `CorpusRegistry` / `data/corpora.toml` path has been removed.

/// Map a `corpus_engine::IngestProgress` variant to a frontend-friendly
/// `CorpusProgressPayload`. Covers the full pipeline including the
/// optional enrichment phases so progress reporting doesn't go silent
/// during the (often long) claim/relationship extraction stages.
fn ingest_progress_to_payload(
    corpus_id: &str,
    progress: &corpus_engine::IngestProgress,
) -> CorpusProgressPayload {
    use corpus_engine::IngestProgress;
    match progress {
        IngestProgress::Downloading {
            percent,
            bytes_downloaded,
            ..
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "downloading".into(),
            percent: *percent,
            chunks_processed: 0,
            message: Some(format!("{:.1} MB", *bytes_downloaded as f64 / 1_048_576.0)),
        },
        IngestProgress::Extracting {
            documents_processed,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "extracting".into(),
            percent: 0.0,
            chunks_processed: *documents_processed,
            message: Some(format!("{} documents", documents_processed)),
        },
        IngestProgress::Chunking { chunks_created } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "chunking".into(),
            percent: 0.0,
            chunks_processed: *chunks_created,
            message: None,
        },
        IngestProgress::Embedding {
            chunks_embedded,
            total,
            docs_processed,
            chunks_per_sec,
            expected_docs,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "embedding".into(),
            // Live-event path has no shard-scan signal, so all we can
            // do is the legacy chunk-total ratio (0 until the pipeline
            // knows the chunk count). The polling path
            // (`status_entry_to_payload`) carries shard-scan progress
            // via `entry.estimated_fraction` and is the primary signal
            // the desktop banner consumes.
            //
            // We deliberately do NOT compute `docs_processed /
            // expected_docs` here, even with a clamp. For Wikipedia
            // JSONL one accepted article emits ~10× sections; the
            // ratio hits 100% within minutes of an embed run that has
            // hours left. The "X / Y articles" message below carries
            // the filter-scope context without lying about completion.
            percent: if *total > 0 {
                (*chunks_embedded as f32 / *total as f32) * 100.0
            } else {
                0.0
            },
            chunks_processed: *chunks_embedded,
            message: Some(format_embed_message(
                *chunks_embedded,
                *docs_processed,
                *chunks_per_sec,
                *expected_docs,
            )),
        },
        IngestProgress::Indexing {
            chunks_indexed,
            total,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "indexing".into(),
            percent: if *total > 0 {
                (*chunks_indexed as f32 / *total as f32) * 100.0
            } else {
                0.0
            },
            chunks_processed: *chunks_indexed,
            message: None,
        },
        IngestProgress::OptimizingIndex { current_chunks } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "optimizing_index".into(),
            // The rebuild is one-shot — no incremental progress to
            // report. Surface it as in-flight (50%) so the banner's
            // bar doesn't snap from 100% (Indexing) back to 0% (no
            // bar) and disorient the user mid-expansion.
            percent: 50.0,
            chunks_processed: *current_chunks,
            message: Some(format!(
                "Retraining vector index over {} chunks",
                pretty_count(*current_chunks)
            )),
        },
        IngestProgress::Complete {
            total_chunks,
            duration_secs,
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "complete".into(),
            percent: 100.0,
            chunks_processed: *total_chunks,
            message: Some(format!("Done in {duration_secs}s")),
        },
    }
}

/// List all corpora available to the user — a union of:
/// - Built-in recipes (Wikipedia, SEP, …) from `corpus_engine::builtin_corpora()`
/// - Locally-installed indexes from `corpus_engine::installed_indexes()`
///
/// Built-in entries that are also installed get their `status` set to
/// "installed" with the live chunk count from the on-disk index.
#[tauri::command]
pub async fn list_corpora(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<CorpusEntry>, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Ok(Vec::new()),
    };
    drop(engine_guard);

    // Pull built-in catalog from registry snapshot (no network required).
    //
    // Layer/satellite corpora (those with a `parent_corpus_id` —
    // `wikipedia-simple`, `wikipedia-newsworthy`) are still returned
    // here. The desktop frontend hides them from the top-level picker
    // and re-renders them as toggleable layers under the parent's row.
    // Returning them in this list keeps the install/remove/progress
    // wiring uniform — every layer is still a real corpus with its own
    // id, status, and progress payload.
    let builtins: Vec<_> = engine.builtin_corpora().into_iter().collect();

    // Look up live install status. Failure here is non-fatal — we still
    // want to render the catalog so the user can choose what to install.
    let installed = engine.installed_indexes().await.unwrap_or_default();

    let installing = state.install_progress.read().await;

    // Snapshot vector index readiness from the store for all installed corpora.
    let store_guard = state.store.read().await;
    let store_opt = store_guard.as_ref().map(Arc::clone);
    drop(store_guard);

    let mut entries = Vec::new();
    for b in &builtins {
        let registry_entry = engine.registry().find_entry(&b.id);
        // An index dir with zero committed chunks is an abandoned shell
        // (e.g. a previous install that crashed before the first
        // tier-2 flush). The recipe got far enough to write
        // `_corpus_meta.json` but no chunks landed in LanceDB. Treating
        // it as "installed" misleads the user into thinking the
        // corpus is partially populated when in fact nothing is
        // searchable. Filter those out so the row falls back to
        // "not_installed" and the Install button reappears — the
        // ingest pipeline will resume cleanly from `committed_iter_pos
        // = 0` since the on-disk state is consistent.
        let installed_info = installed
            .iter()
            .find(|i| i.corpus_id == b.id && !i.is_shard && i.chunk_count > 0);
        let is_installing = installing
            .get(&b.id)
            .is_some_and(|p| p.phase != "complete" && p.phase != "failed");

        let status = if installed_info.is_some() {
            "installed"
        } else if is_installing {
            "installing"
        } else {
            "not_installed"
        };

        // `vector_index_ready` is what the UI uses to decide whether
        // to show "Build Index" or "Hybrid search ready". Two sources
        // of truth historically, easy to drift apart:
        //
        //   1. `_corpus_meta.json.vector_index_built` — written by the
        //      ingest pipeline when IVF-PQ actually finishes.
        //   2. The SQLite `vector_index_ready` flag — set ONLY by the
        //      explicit `build_corpus_index` Tauri command.
        //
        // A regular ingest that builds the index never writes (2), so
        // the UI shows "Keyword search only / Build Index" even though
        // the vector index is on disk and live. Trust the on-disk meta
        // first; fall back to the SQLite cache for installs that
        // happened before this field was populated.
        let vector_index_ready = if let Some(info) = installed_info {
            if info.vector_index_built {
                true
            } else if let Some(ref s) = store_opt {
                s.get_vector_index_ready(&b.id).await.unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };

        entries.push(CorpusEntry {
            id: b.id.clone(),
            name: b.name.clone(),
            description: b.description.clone(),
            size_compressed_gb: b.size_compressed_gb,
            size_indexed_gb: b.size_indexed_gb,
            license: b.license.clone(),
            tiers: tiers_for(&b.id),
            status: status.to_string(),
            chunks_count: installed_info.map(|i| i.chunk_count),
            enrichment_enabled: registry_entry.map(|e| e.enrichment_enabled).unwrap_or(false),
            indexed_at: installed_info.map(|i| i.created_at),
            embedding_model: installed_info.map(|i| i.embedding_model.clone()),
            embedding_dimensions: installed_info.map(|i| i.embedding_dimensions),
            vector_index_ready,
            registry_url: registry_entry.map(|e| e.toml_url.clone()),
            schema_version: Some(1),
            parent_corpus_id: b.parent_corpus_id.clone(),
        });
    }

    Ok(entries)
}

/// Build the IVF-PQ vector index for an installed corpus in the background.
/// Emits `index-build-progress`, `index-build-complete`, or `index-build-error`
/// events to the frontend. Sets `vector_index_ready` on the store when done.
#[tauri::command]
pub async fn build_corpus_index(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let engine = {
        let guard = state.corpus_engine.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Corpus engine not ready")?
    };
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };

    let cid = corpus_id.clone();
    tokio::spawn(async move {
        let indexes = match engine.installed_indexes().await {
            Ok(v) => v,
            Err(e) => {
                let _ = app_handle.emit(
                    "index-build-error",
                    serde_json::json!({"corpus_id": cid, "error": e.to_string()}),
                );
                return;
            }
        };
        let Some(info) = indexes.iter().find(|i| i.corpus_id == cid) else {
            let _ = app_handle.emit(
                "index-build-error",
                serde_json::json!({"corpus_id": cid, "error": "Corpus not found"}),
            );
            return;
        };
        let idx = match engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                let _ = app_handle.emit(
                    "index-build-error",
                    serde_json::json!({"corpus_id": cid, "error": e.to_string()}),
                );
                return;
            }
        };

        let progress_handle = app_handle.clone();
        let progress_cid = cid.clone();
        let on_progress: Box<dyn Fn(u64, u64) + Send + Sync> = Box::new(move |done, total| {
            let pct = if total > 0 { done * 100 / total } else { 0 };
            let _ = progress_handle.emit(
                "index-build-progress",
                serde_json::json!({"corpus_id": &progress_cid, "phase": "building", "pct": pct}),
            );
        });

        // Build both vector and FTS indexes. The recipe controls which
        // are enabled; passing (true, true) lets the index builder respect
        // those flags rather than hardcoding FTS off (which would corrupt
        // the metadata by marking FTS as built without building it).
        match idx.build_indexes(true, true, Some(&*on_progress)).await {
            Ok(()) => {
                let _ = store.set_vector_index_ready(&cid, true).await;
                let _ = app_handle.emit(
                    "index-build-complete",
                    serde_json::json!({"corpus_id": cid}),
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "index-build-error",
                    serde_json::json!({"corpus_id": cid, "error": e.to_string()}),
                );
            }
        }
    });

    Ok(())
}

#[derive(serde::Serialize)]
pub struct IngestDocumentResult {
    pub source: String,
    pub chunks_created: usize,
}

#[tauri::command]
pub async fn ingest_document(
    state: State<'_, Arc<AppState>>,
    file_path: String,
) -> Result<IngestDocumentResult, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("File not found: {file_path}"));
    }

    let chunks_created = sovereign_tools::rag::ingest::ingest_file(
        path,
        store.as_ref(),
        inference.as_ref().map(|i| i.as_ref()),
    )
    .await
    .map_err(|e| format!("Ingest failed: {e}"))?;

    let source = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&file_path)
        .to_string();

    eprintln!("[ingest] {} -> {} chunks", source, chunks_created);

    Ok(IngestDocumentResult {
        source,
        chunks_created,
    })
}

// ─── Document Asset commands ─────────────────────────────────

#[derive(Serialize)]
pub struct DocumentAssetResponse {
    pub asset: sovereign_core::types::DocumentAsset,
}

#[derive(Serialize)]
pub struct DocumentAskResponse {
    pub response: String,
    /// The document operation used to answer, when the document was involved.
    /// `None` when the question was off-topic and the runtime's normal
    /// conversation pipeline answered it instead (no operation badge shown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<sovereign_core::types::DocumentAssetOperation>,
    pub sources: Vec<String>,
}

/// Upload and ingest a document. The command returns immediately with
/// a Pending asset. The full ingest pipeline (embed + skeleton) runs
/// in a background task and emits `document:progress` events. The
/// frontend shows these via the IngestBanner / DocOpProgress indicator.
#[tauri::command]
pub async fn upload_document_asset(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    file_path: String,
) -> Result<DocumentAssetResponse, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or("Inference not ready")?
    };

    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("File not found: {file_path}"));
    }

    // Parse and chunk synchronously (fast — no inference needed).
    let parsed = sovereign_tools::rag::parse::parse_file(path)
        .map_err(|e| format!("Parse failed: {e}"))?;
    let text_chunks = sovereign_tools::rag::chunk::chunk_text(&parsed.content);
    let word_count = parsed.content.split_whitespace().count();
    let chunk_count = text_chunks.len();
    let file_size_mb = std::fs::metadata(path)
        .map(|m| m.len() as f32 / (1024.0 * 1024.0))
        .unwrap_or(0.0);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&filename)
        .replace('_', " ")
        .replace('-', " ");

    // Create the asset record immediately so the UI shows it.
    let asset = sovereign_core::types::DocumentAsset {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        filename,
        file_size_mb,
        word_count,
        chunk_count,
        document_type: sovereign_core::types::DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("doc-pending"),
        skeleton: None,
        state: sovereign_core::types::AssetState::Pending,
    };
    store
        .save_document_asset(&asset)
        .await
        .map_err(|e| format!("Save failed: {e}"))?;

    let response_asset = asset.clone();

    // Spawn the full ingest in the background. Progress events will
    // update the UI in real time; the asset state transitions from
    // Pending → Indexing → PartiallyReady → BuildingSkeleton → Ready.
    let file_path_owned = file_path.to_string();
    let handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let manager =
            sovereign_tools::document_asset::DocumentAssetManager::new(inference, store);

        let path = std::path::Path::new(&file_path_owned);
        match manager
            .ingest(path, move |progress| {
                let _ = handle.emit("document:progress", &progress);
            })
            .await
        {
            Ok(completed) => {
                eprintln!(
                    "[document_asset] Ingest complete: {} ({} chunks, {} entities)",
                    completed.filename,
                    completed.chunk_count,
                    completed
                        .skeleton
                        .as_ref()
                        .map(|s| s.main_entities.len())
                        .unwrap_or(0),
                );
            }
            Err(e) => {
                eprintln!("[document_asset] Ingest failed: {e}");
            }
        }
    });

    Ok(DocumentAssetResponse {
        asset: response_asset,
    })
}

#[tauri::command]
pub async fn ask_document(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    asset_id: String,
    question: String,
    conversation_id: String,
) -> Result<DocumentAskResponse, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or("Inference not ready")?
    };

    let asset = store
        .get_document_asset(&asset_id)
        .await
        .map_err(|e| format!("Load failed: {e}"))?
        .ok_or("Document not found")?;

    if !asset.state.is_queryable() {
        return Err(format!(
            "Document is not ready for queries (state: {})",
            asset.state.label()
        ));
    }

    // Self-heal: if the skeleton never persisted (common when ingest was
    // interrupted — app quit mid-build, backend crash, etc.), kick off a
    // rebuild in the background. The current turn still proceeds with the
    // skeleton-less asset (routing will be slightly less accurate); every
    // subsequent turn benefits from the rebuilt skeleton.
    if asset.skeleton.is_none() {
        tracing::info!(
            asset_id = %asset_id,
            "ask_document: skeleton missing — spawning background rebuild"
        );
        let inf = Arc::clone(&inference);
        let s = store.clone();
        let aid = asset_id.clone();
        let app = app_handle.clone();
        tokio::spawn(async move {
            let manager = sovereign_tools::document_asset::DocumentAssetManager::new(inf, s);
            match manager.rebuild_skeleton(&aid).await {
                Ok(skeleton) => {
                    tracing::info!(
                        asset_id = %aid,
                        entities = skeleton.main_entities.len(),
                        sections = skeleton.sections.len(),
                        "auto-heal: skeleton rebuilt"
                    );
                    let _ = app.emit("document:skeleton_rebuilt", &aid);
                }
                Err(e) => {
                    tracing::warn!(
                        asset_id = %aid,
                        error = %e,
                        "auto-heal: skeleton rebuild failed"
                    );
                }
            }
        });
    }

    // Persist the user's question first. This also upserts the conversations
    // row so the conversation survives navigation and restart, and lets the
    // runtime pipeline (below) see the question when it builds context.
    let user_msg = sovereign_core::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: sovereign_core::types::Role::User,
        content: question.clone(),
        created_at: now_epoch(),
        metadata: Some(serde_json::json!({
            "attached_asset_id": asset_id,
        })),
        version: now_epoch(),
    };
    store
        .save_message(&user_msg)
        .await
        .map_err(|e| format!("Failed to save user message: {e}"))?;

    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(
        Arc::clone(&inference),
        store.clone(),
    );

    // Route first — a Fast-slot call that decides whether this question is
    // about the document at all.
    let operation = manager
        .route(&asset, &question)
        .await
        .map_err(|e| format!("Routing failed: {e}"))?;

    tracing::info!(
        asset_id = %asset_id,
        operation = %operation.label(),
        "ask_document: routed"
    );

    // When the question isn't about the document, hand it off to the normal
    // conversation pipeline. The runtime will route, search installed corpora,
    // synthesise with layered confidence, and save the assistant message. The
    // user message is already in the conversation (tagged with the asset id,
    // preserving "this turn had a document attached" context).
    if matches!(
        operation,
        sovereign_core::types::DocumentAssetOperation::OffTopic { .. }
    ) {
        return run_turn_via_runtime(&app_handle, &state, &question, &conversation_id).await;
    }

    // Document operation path.
    let handle = app_handle.clone();
    let start = std::time::Instant::now();
    let output = manager
        .execute_operation(&asset, &question, &operation, &move |progress| {
            let _ = handle.emit("document:operation", &progress);
        })
        .await
        .map_err(|e| format!("Query failed: {e}"))?;

    // RAG safety net: if retrieval returned zero matching chunks, the router
    // mis-classified. Fall through to the runtime pipeline the same way
    // OffTopic does. `execute_rag` signals this by returning an empty
    // ExecutionOutput.
    if matches!(
        operation,
        sovereign_core::types::DocumentAssetOperation::Rag { .. }
    ) && output.citations.is_empty()
        && output.text.is_empty()
    {
        tracing::info!(
            asset_id = %asset_id,
            "ask_document: RAG found no relevant passages — falling back to runtime"
        );
        return run_turn_via_runtime(&app_handle, &state, &question, &conversation_id).await;
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let assistant_message_id = uuid::Uuid::new_v4().to_string();

    // Build the `retrieved_chunks` + `provenance` shape the frontend expects
    // for the routing-meta bar and rich citation popovers. The frontend
    // matches `[Source: <label>]` spans in the prose against each chunk's
    // `title`, so we use the citation label as the title here.
    let retrieved_chunks: Vec<serde_json::Value> = output
        .citations
        .iter()
        .map(|c| {
            serde_json::json!({
                "title": c.label,
                "corpus_id": asset.title,
                "url": serde_json::Value::Null,
                "snippet": c.snippet,
                "provenance_tier": "document",
            })
        })
        .collect();

    let provenance = sovereign_core::types::ResponseProvenance {
        intent: format!("DocumentAsk:{}", operation.label()),
        search_method: Some("document".to_string()),
        sources: vec![sovereign_core::types::SourceSummary {
            origin: asset.title.clone(),
            count: output.citations.len(),
            from_peer: None,
            display_name: None,
        }],
        inference_backend: if output.model_id.is_empty() {
            "local".to_string()
        } else {
            output.model_id.clone()
        },
        oicp_match: None,
        total_latency_ms: duration_ms,
        tokens_used: output.tokens_used,
        coarse_intent: None,
        self_assessment: None,
        coverage: None,
    };

    let sources_content: Vec<String> = output
        .citations
        .iter()
        .map(|c| c.content.clone())
        .collect();

    // Epistemic-humility hook: the runtime audits the document-op answer
    // against its citations and may surface an InformationRequestCard so
    // the user can paste additional context. Evidence is the concatenated
    // citation content already shown to the model.
    let final_content = {
        let runtime_guard = state.runtime.read().await;
        if let Some(runtime) = runtime_guard.as_ref() {
            // Make sure the approval channel stamps this conversation id
            // onto any info-request so the frontend can route the response
            // back to the right pending oneshot.
            state.approval.set_task_id(&conversation_id).await;
            let evidence = sources_content.join("\n\n");
            runtime
                .maybe_collaborate(&conversation_id, &question, &output.text, &evidence)
                .await
        } else {
            output.text.clone()
        }
    };

    // Persist the assistant response with document operation metadata
    // (legacy `operation` / `sources` fields) plus the new rich
    // `provenance` / `retrieved_chunks` shape the AssistantMessage
    // component reads for the routing-meta bar and citation popovers.
    let assistant_msg = sovereign_core::types::Message {
        id: assistant_message_id.clone(),
        conversation_id: conversation_id.clone(),
        role: sovereign_core::types::Role::Assistant,
        content: final_content.clone(),
        created_at: now_epoch(),
        metadata: Some(serde_json::json!({
            "attached_asset_id": asset_id,
            "operation": operation,
            "sources": sources_content,
            "duration_ms": duration_ms,
            "provenance": provenance,
            "retrieved_chunks": retrieved_chunks,
        })),
        version: now_epoch(),
    };
    store
        .save_message(&assistant_msg)
        .await
        .map_err(|e| format!("Failed to save assistant message: {e}"))?;

    // Record the operation for analytics.
    let _ = store
        .save_document_operation(&assistant_message_id, &asset_id, &operation, duration_ms)
        .await;

    // Fire auto-title in the background after the first exchange.
    {
        let inf = Arc::clone(&inference);
        let s = store.clone();
        let cid = conversation_id.clone();
        let app = app_handle.clone();
        tokio::spawn(async move {
            match sovereign_core::title::try_auto_title(inf.as_ref(), s.as_ref(), &cid).await {
                Ok(Some(_)) => {
                    let _ = app.emit("conversations:changed", ());
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        conversation_id = %cid,
                        error = %e,
                        "auto-title: generation failed (ask_document)"
                    );
                }
            }
        });
    }

    let _ = app_handle.emit("conversations:changed", ());

    Ok(DocumentAskResponse {
        response: final_content,
        operation: Some(operation),
        sources: sources_content,
    })
}

/// Refresh a single document asset by id. Used by the frontend to pick up
/// state changes (e.g. an auto-heal rebuild that just completed in the
/// background).
#[tauri::command]
pub async fn get_document_asset(
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<Option<sovereign_core::types::DocumentAsset>, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    store
        .get_document_asset(&asset_id)
        .await
        .map_err(|e| format!("Load failed: {e}"))
}

/// User-initiated skeleton rebuild. Works from stored chunks (no file
/// required) — handy for assets whose skeleton never persisted because the
/// original ingest was interrupted, and for documents opened from history.
#[tauri::command]
pub async fn rebuild_document_skeleton(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<sovereign_core::types::DocumentAsset, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or("Inference not ready")?
    };

    let manager = sovereign_tools::document_asset::DocumentAssetManager::new(
        Arc::clone(&inference),
        store.clone(),
    );

    manager
        .rebuild_skeleton(&asset_id)
        .await
        .map_err(|e| format!("Skeleton rebuild failed: {e}"))?;

    // Return the refreshed asset record so the caller can update UI state
    // in-place (skeleton now Some, document_type set, state Ready).
    let refreshed = store
        .get_document_asset(&asset_id)
        .await
        .map_err(|e| format!("Reload failed: {e}"))?
        .ok_or("Asset vanished during rebuild")?;

    let _ = app_handle.emit("document:skeleton_rebuilt", &asset_id);

    Ok(refreshed)
}

/// Helper used by `ask_document` when the routed question is off-topic
/// (or when RAG retrieval comes up empty). Delegates to the runtime's
/// normal conversation pipeline — router, corpus search, layered-confidence
/// synthesis, auto-title — and returns a `DocumentAskResponse` with no
/// `DocumentAssetOperation` attribution since the document wasn't used.
///
/// The user message has already been saved as the latest message in the
/// conversation, so we use `handle_turn` (not `handle_message`) to avoid
/// saving it twice.
async fn run_turn_via_runtime(
    app_handle: &tauri::AppHandle,
    state: &State<'_, Arc<AppState>>,
    question: &str,
    conversation_id: &str,
) -> Result<DocumentAskResponse, String> {
    let runtime = {
        let guard = state.runtime.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Runtime not ready")?
    };

    state.approval.set_task_id(conversation_id).await;

    let response = runtime
        .handle_turn(question, conversation_id)
        .await
        .map_err(|e| format!("Runtime turn failed: {e}"))?;

    // Runtime saved the assistant message itself and spawned auto-title.
    // Emit the list-refresh event the normal send_message command emits.
    let _ = app_handle.emit("conversations:changed", ());

    Ok(DocumentAskResponse {
        response: response.message.content,
        operation: None,
        sources: Vec::new(),
    })
}

#[tauri::command]
pub async fn list_document_assets(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<sovereign_core::types::DocumentAsset>, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    store
        .list_document_assets()
        .await
        .map_err(|e| format!("List failed: {e}"))
}

#[tauri::command]
pub async fn delete_document_asset(
    state: State<'_, Arc<AppState>>,
    asset_id: String,
) -> Result<(), String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };
    let inference = {
        let guard = state.inference.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or("Inference not ready")?
    };

    let manager =
        sovereign_tools::document_asset::DocumentAssetManager::new(inference, store);
    manager
        .delete(&asset_id)
        .await
        .map_err(|e| format!("Delete failed: {e}"))
}

/// A document from the legacy chunks table (uploaded via the old
/// paperclip path before DocumentAssetManager existed).
#[derive(Serialize)]
pub struct LegacyDocumentEntry {
    pub source: String,
    pub filename: String,
    pub chunk_count: usize,
    pub word_count: usize,
}

/// List documents from the legacy `documents` table that don't have
/// a corresponding DocumentAsset record. These are shown in the picker
/// so users can see and select previously uploaded files.
#[tauri::command]
pub async fn list_legacy_documents(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LegacyDocumentEntry>, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };

    let sources = store.list_sources().await.map_err(|e| format!("{e}"))?;
    let assets = store.list_document_assets().await.unwrap_or_default();

    // Filter out sources that already have a DocumentAsset (including
    // the "asset:uuid" sources created by DocumentAssetManager).
    let asset_sources: std::collections::HashSet<String> = assets
        .iter()
        .map(|a| format!("asset:{}", a.id))
        .collect();

    let mut entries = Vec::new();
    for source in &sources {
        // Skip asset-managed documents and corpus chunks.
        if source.starts_with("asset:") && asset_sources.contains(source) {
            continue;
        }
        // Skip corpus-sourced chunks (Wikipedia, SEP, etc.).
        if source.starts_with("corpus:") {
            continue;
        }

        let chunks = store
            .get_chunks_by_source(source)
            .await
            .unwrap_or_default();
        if chunks.is_empty() {
            continue;
        }

        let word_count: usize = chunks
            .iter()
            .map(|c| c.content.split_whitespace().count())
            .sum();
        let filename = source
            .rsplit('/')
            .next()
            .unwrap_or(source)
            .to_string();

        entries.push(LegacyDocumentEntry {
            source: source.clone(),
            filename,
            chunk_count: chunks.len(),
            word_count,
        });
    }

    Ok(entries)
}

/// Promote a legacy document (from the old chunks table) into a
/// DocumentAsset. This creates the asset record from existing data —
/// no re-upload, no re-embedding. The skeleton is null until built.
#[tauri::command]
pub async fn promote_legacy_document(
    state: State<'_, Arc<AppState>>,
    source: String,
) -> Result<DocumentAssetResponse, String> {
    let store = {
        let guard = state.store.read().await;
        guard.as_ref().map(Arc::clone).ok_or("Store not ready")?
    };

    let chunks = store
        .get_chunks_by_source(&source)
        .await
        .map_err(|e| format!("{e}"))?;

    if chunks.is_empty() {
        return Err(format!("No chunks found for source: {source}"));
    }

    let word_count: usize = chunks
        .iter()
        .map(|c| c.content.split_whitespace().count())
        .sum();
    let filename = source
        .rsplit('/')
        .next()
        .unwrap_or(&source)
        .to_string();
    let title = filename
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(&filename)
        .replace('_', " ")
        .replace('-', " ");

    let asset = sovereign_core::types::DocumentAsset {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        filename,
        file_size_mb: 0.0, // Unknown for legacy docs.
        word_count,
        chunk_count: chunks.len(),
        document_type: sovereign_core::types::DocumentTypeTag::Unknown,
        ingested_at: chrono::Utc::now(),
        index_id: format!("legacy:{source}"),
        skeleton: None,
        state: sovereign_core::types::AssetState::PartiallyReady,
    };

    store
        .save_document_asset(&asset)
        .await
        .map_err(|e| format!("{e}"))?;

    Ok(DocumentAssetResponse { asset })
}

#[tauri::command]
pub async fn diagnose_corpus(
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    drop(engine_guard);
    Ok(engine.diagnose_indexes().await)
}

/// Local daemon URL used by corpus install/cancel/progress commands.
/// The internal API is always bound on `127.0.0.1:9742` (see
/// `sovereign-mesh::daemon`). Keeping this constant here rather than
/// threading a config value means a stale Desktop build can never
/// point at the wrong port after a daemon update.
const DAEMON_INTERNAL_URL: &str = "http://127.0.0.1:9742";

/// Kick off a corpus install via the daemon's unified install
/// endpoint. The daemon is the single owner of ingest lifecycle —
/// Desktop is a thin client that says "start" and then watches. The
/// continuous `spawn_corpus_status_poller` (started at backend
/// bootstrap) emits `corpus-progress` events for whichever ingests
/// the daemon is running, whether Desktop initiated them or a prior
/// session's auto-collaborate loop did.
///
/// Clicking Install a second time while the daemon is already
/// ingesting this corpus is a no-op: `/internal/corpus/install` is
/// idempotent and returns `spawned: false`.
#[tauri::command]
pub async fn install_corpus(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let install_url = format!("{DAEMON_INTERNAL_URL}/internal/corpus/install");
    let resp = client
        .post(&install_url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/install: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/install returned {status}: {body}"
        ));
    }

    // Flip the UI state immediately so the Install button reacts
    // before the next status-poller tick lands. The poller (running
    // in the background) will overwrite this stub payload with real
    // progress on its very next pass.
    let initial = CorpusProgressPayload {
        corpus_id: corpus_id.clone(),
        phase: "downloading".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Starting…".into()),
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(corpus_id.clone(), initial.clone());
    }
    let _ = app_handle.emit("corpus-progress", initial);

    Ok(())
}

/// Tauri command: expand an installed corpus by relaxing its filter
/// scope (e.g. promote Wikipedia from Core to Full). Returns
/// immediately; progress streams on the existing `corpus-progress`
/// event channel — same surface as `install_corpus` so the
/// `CorpusProgressBanner` and `KnowledgeStatus` row light up
/// automatically.
#[tauri::command]
pub async fn lc_expand_corpus(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let url = format!("{DAEMON_INTERNAL_URL}/internal/corpus/expand");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/expand: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/expand returned {status}: {body}"
        ));
    }

    // Mirror install_corpus's optimistic flip: surface a "downloading"
    // stub so the UI reacts before the next status poll lands.
    let initial = CorpusProgressPayload {
        corpus_id: corpus_id.clone(),
        phase: "extracting".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Expanding scope…".into()),
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(corpus_id.clone(), initial.clone());
    }
    let _ = app_handle.emit("corpus-progress", initial);

    Ok(())
}

/// Tauri command: ask the daemon whether `corpus_id` can be expanded
/// (i.e. has an active filter scope with `expandable=true` in
/// `_corpus_meta.json`). Returns `false` if the corpus isn't installed
/// or has no filter, `true` if a relaxed scope would add documents.
///
/// Reads `_corpus_meta.json` directly from the per-corpus index dir
/// rather than going through the daemon — the file is local and
/// avoiding the round-trip keeps the Settings render snappy.
#[tauri::command]
pub async fn lc_can_expand(corpus_id: String) -> Result<bool, String> {
    let mut path = match dirs::home_dir() {
        Some(h) => h,
        None => return Ok(false),
    };
    path.push(".sovereign");
    path.push("indexes");
    path.push(format!("{corpus_id}-canonical"));
    path.push("_corpus_meta.json");
    if !path.exists() {
        // Try the partition-of-self variant.
        path.pop();
        path.pop();
        path.push(format!("{corpus_id}-local"));
        path.push("_corpus_meta.json");
        if !path.exists() {
            return Ok(false);
        }
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };
    // Probe-only deserialize — we only care about the `scope` block.
    #[derive(serde::Deserialize)]
    struct ScopeProbe {
        #[serde(default)]
        scope: Option<ScopeBody>,
    }
    #[derive(serde::Deserialize)]
    struct ScopeBody {
        #[serde(default)]
        expandable: bool,
    }
    let probe: ScopeProbe = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    Ok(probe.scope.map(|s| s.expandable).unwrap_or(false))
}

/// Tauri command: kick off the layered Wikipedia setup. Installs
/// `wikipedia-simple` (Layer 0, ~2–3 min) and `wikipedia` Core
/// (Layer 1, ~10–12 min) back-to-back. Both run via the existing
/// `/internal/corpus/install` daemon endpoint, so progress streams on
/// the unchanged `corpus-progress` event channel.
#[tauri::command]
pub async fn lc_start_layered_setup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<String>, String> {
    // Layer 0 first — small, fast, gives the user grounded responses
    // within minutes.
    install_corpus(
        app_handle.clone(),
        state.clone(),
        "wikipedia-simple".into(),
    )
    .await?;
    // Layer 1 — kicks off concurrently with Layer 0. The daemon
    // schedules both serially behind the shared embed slot, but the
    // download phase parallelises with whichever phase Layer 0 is in.
    install_corpus(app_handle, state, "wikipedia".into()).await?;
    Ok(vec!["wikipedia-simple".into(), "wikipedia".into()])
}

/// Spawn the background poller that reads
/// `/internal/corpus/status` every second and forwards every active
/// entry to the `corpus-progress` Tauri event channel.
///
/// Starts at backend bootstrap and runs for the life of the process.
/// Without this the UI only sees ingests Desktop itself kicked off —
/// a daemon-driven resume after a crash/close would run invisibly and
/// the user would still see the "Install" button for a corpus the
/// daemon is actively ingesting (the bug we're fixing).
///
/// Emits a terminal `complete` event for corpora that disappear from
/// the snapshot after a grace window, so the Svelte `installing`
/// state flips back to `installed` without waiting for the next
/// `list_corpora` refresh.
pub fn spawn_corpus_status_poller(
    app_handle: tauri::AppHandle,
    state: Arc<AppState>,
) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "corpus status poller: failed to build HTTP client — poller disabled"
                );
                return;
            }
        };
        let url = format!("{DAEMON_INTERNAL_URL}/internal/corpus/status");
        // Track what was seen last tick so we can detect terminations
        // (corpus disappeared from the snapshot → emit complete).
        let mut last_seen: std::collections::HashSet<String> = Default::default();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(_) => continue, // Daemon may be restarting; retry next tick.
            };
            let snapshot: CorpusStatusResponse = match resp.json().await {
                Ok(b) => b,
                Err(_) => continue,
            };

            let current: std::collections::HashSet<String> = snapshot
                .entries
                .iter()
                .map(|e| e.corpus_id.clone())
                .collect();

            // Emit terminal "complete" for entries that were active last
            // tick but have dropped off this tick.
            for gone in last_seen.difference(&current) {
                let last_phase = state
                    .install_progress
                    .read()
                    .await
                    .get(gone)
                    .map(|p| p.phase.clone())
                    .unwrap_or_default();
                if last_phase != "complete" && last_phase != "failed" {
                    let final_payload = CorpusProgressPayload {
                        corpus_id: gone.clone(),
                        phase: "complete".into(),
                        percent: 100.0,
                        chunks_processed: 0,
                        message: Some("Done".into()),
                    };
                    if let Ok(mut map) = state.install_progress.try_write() {
                        map.insert(gone.clone(), final_payload.clone());
                    }
                    let _ = app_handle.emit("corpus-progress", final_payload);
                }
            }
            last_seen = current;

            for entry in &snapshot.entries {
                let payload = status_entry_to_payload(entry);
                if let Ok(mut map) = state.install_progress.try_write() {
                    map.insert(entry.corpus_id.clone(), payload.clone());
                }
                let _ = app_handle.emit("corpus-progress", payload);
            }
        }
    });
}

/// Convert a `CorpusStatusEntry` from the daemon into the
/// frontend-shaped `CorpusProgressPayload`. Prefers the
/// daemon-computed `estimated_fraction` for the percent; falls back
/// to a sensible phase + message when no progress event is known yet.
fn status_entry_to_payload(entry: &CorpusStatusEntry) -> CorpusProgressPayload {
    use corpus_engine::IngestProgress as P;

    // Shard-scan progress is the primary signal. For filtered ingests
    // (Wikipedia Core, etc.) the iterator must scan the entire source
    // ZIP, so shards-completed/shards-total tracks wall-clock honestly
    // even when most articles are rejected. An earlier revision tried
    // `docs_processed / expected_docs` as a "filter-aware" percent —
    // wrong, because docs are *sections* (~10× the accepted article
    // count for `wikipedia_jsonl`) while expected_docs is the title
    // count, so the ratio hit 100% with hours of work still ahead.
    // The "X / Y articles" string in the message line below carries
    // the filter-scope context without conflating units in the bar.
    let percent = entry
        .estimated_fraction
        .map(|f| (f * 100.0).clamp(0.0, 100.0))
        .unwrap_or(0.0);

    let (phase, chunks_processed, message) = match entry.progress.as_ref() {
        Some(P::Downloading {
            percent: dp,
            bytes_downloaded,
            bytes_total,
        }) => {
            let msg = bytes_total
                .map(|t| format!(
                    "{:.0} / {:.0} MB ({:.0}%)",
                    *bytes_downloaded as f64 / 1_048_576.0,
                    t as f64 / 1_048_576.0,
                    dp,
                ))
                .unwrap_or_else(|| {
                    format!("{:.0} MB", *bytes_downloaded as f64 / 1_048_576.0)
                });
            ("downloading".to_string(), 0u64, Some(msg))
        }
        Some(P::Extracting { documents_processed }) => (
            "extracting".to_string(),
            0,
            Some(format!("{} articles", documents_processed)),
        ),
        Some(P::Chunking { chunks_created }) => (
            "chunking".to_string(),
            *chunks_created,
            Some(format!("{} chunks", chunks_created)),
        ),
        Some(P::Embedding {
            chunks_embedded,
            docs_processed,
            chunks_per_sec,
            expected_docs,
            ..
        }) => (
            "embedding".to_string(),
            *chunks_embedded,
            Some(format_embed_message(
                *chunks_embedded,
                *docs_processed,
                *chunks_per_sec,
                *expected_docs,
            )),
        ),
        Some(P::Indexing { chunks_indexed, .. }) => (
            "indexing".to_string(),
            *chunks_indexed,
            Some(format!("{} chunks indexed", pretty_count(*chunks_indexed))),
        ),
        Some(P::OptimizingIndex { current_chunks }) => (
            "optimizing_index".to_string(),
            *current_chunks,
            Some(format!(
                "Retraining vector index over {} chunks",
                pretty_count(*current_chunks)
            )),
        ),
        Some(P::Complete {
            total_chunks,
            duration_secs,
        }) => (
            "complete".to_string(),
            *total_chunks,
            Some(format!("Done in {duration_secs}s")),
        ),
        None => {
            // No IngestProgress event yet this session — this is the
            // classic "daemon resumed after Desktop close" state. Use
            // on-disk counters so the user still sees "something is
            // happening" instead of a stuck spinner.
            let phase = if entry.canonical_in_progress || entry.partition_in_progress {
                "embedding"
            } else {
                "downloading"
            };
            let msg = if entry.committed_iter_pos > 0 {
                // When the sampler has published a total estimate we
                // prefer `M/N sections` over a raw running count —
                // it's the same info the progress bar encodes but
                // more legible at a glance on the details line.
                match entry.estimated_total_sections {
                    Some(total) if total > 0 => Some(format!(
                        "Resuming · {}/{} sections",
                        pretty_count(entry.committed_iter_pos),
                        pretty_count(total),
                    )),
                    _ => Some(format!(
                        "Resuming · {} sections committed",
                        pretty_count(entry.committed_iter_pos),
                    )),
                }
            } else {
                Some("Starting…".into())
            };
            (phase.to_string(), entry.committed_iter_pos, msg)
        }
    };

    CorpusProgressPayload {
        corpus_id: entry.corpus_id.clone(),
        phase,
        percent,
        chunks_processed,
        message,
    }
}

/// Format the embed-phase message line that both the live-event and
/// polling paths emit. Centralises the format so the two paths can't
/// drift, and threads the filter-derived denominator into the line
/// when known.
///
/// Unit nuance: for `wikipedia_jsonl` `docs_processed` counts emitted
/// `ExtractedDoc`s — i.e. sections, ~2.5× the article count — while
/// `expected_docs` from a `title_list` filter is the title count.
/// We clamp the displayed numerator to the expected count so the
/// "X / Y articles" reading matches the percent (also clamped) rather
/// than overshooting into "128k / 51k articles" near the end. The
/// displayed number is approximate at the per-article level but
/// communicates the right scale, which is what the operator needs.
fn format_embed_message(
    chunks_embedded: u64,
    docs_processed: u64,
    chunks_per_sec: f32,
    expected_docs: Option<u64>,
) -> String {
    match expected_docs {
        Some(total) if total > 0 => {
            let displayed = docs_processed.min(total);
            format!(
                "{} chunks · {} / {} articles · {:.0}/s",
                pretty_count(chunks_embedded),
                pretty_count(displayed),
                pretty_count(total),
                chunks_per_sec,
            )
        }
        _ => format!(
            "{} chunks · {} docs · {:.0}/s",
            pretty_count(chunks_embedded),
            pretty_count(docs_processed),
            chunks_per_sec,
        ),
    }
}

/// Compact count formatter for UI messages: 7_265_216 → "7.3M".
fn pretty_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Wire-level DTO for the daemon's `/internal/corpus/status`
/// response. Mirrors `commonwealth_api::routes_internal::CorpusStatusEntry`.
#[derive(Debug, serde::Deserialize)]
struct CorpusStatusResponse {
    entries: Vec<CorpusStatusEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct CorpusStatusEntry {
    corpus_id: String,
    #[allow(dead_code)]
    active: bool,
    progress: Option<corpus_engine::IngestProgress>,
    #[allow(dead_code)]
    shards_completed: usize,
    #[allow(dead_code)]
    shards_total: usize,
    committed_iter_pos: u64,
    #[allow(dead_code)]
    canonical_present: bool,
    #[allow(dead_code)]
    partition_present: bool,
    canonical_in_progress: bool,
    partition_in_progress: bool,
    estimated_fraction: Option<f32>,
    #[serde(default)]
    estimated_total_sections: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    estimated_total_articles: Option<u64>,
}

/// Remove (wipe) a corpus on this node via the daemon. Destructive —
/// deletes canonical + every partition-* sibling dir for `corpus_id`.
///
/// Replaces the old direct `engine.remove_index` call — that path
/// ignored in-flight ingest tasks and left `<corpus>-partition-*/`
/// dirs on disk. The daemon route handles both (signal cancel, await
/// task exit, wipe canonical + every partition sibling). The
/// `confirm_wipe: true` body field is the daemon-side guardrail
/// against accidental wipes; this command is the explicit "remove"
/// surface so it always passes it.
#[tauri::command]
pub async fn remove_corpus(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let url = format!("{DAEMON_INTERNAL_URL}/internal/corpus/cancel");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "corpus_id": corpus_id,
            "confirm_wipe": true,
        }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/cancel: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/cancel returned {status}: {body}"
        ));
    }

    // Clear any stale progress entry so the UI returns to "not_installed".
    if let Ok(mut map) = state.install_progress.try_write() {
        map.remove(&corpus_id);
    }
    Ok(())
}

/// Pause an in-progress corpus ingest on this node via the daemon.
/// Non-destructive — committed chunks and `_corpus_meta.json` are kept
/// so a subsequent `install_corpus` call resumes from the last flush.
///
/// This is what the UI's in-progress "Cancel" button calls. The
/// destructive variant lives behind the `Remove` action on installed
/// corpora and goes through `remove_corpus` above.
#[tauri::command]
pub async fn pause_corpus(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;

    let url = format!("{DAEMON_INTERNAL_URL}/internal/corpus/pause");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/pause: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/corpus/pause returned {status}: {body}"
        ));
    }

    // Clear the in-memory progress entry so the UI immediately reflects
    // "stopped". On-disk state is intact — `install_corpus` resumes.
    if let Ok(mut map) = state.install_progress.try_write() {
        map.remove(&corpus_id);
    }
    Ok(())
}

#[tauri::command]
pub async fn get_corpus_progress(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<CorpusProgressPayload>, String> {
    let map = state.install_progress.read().await;
    Ok(map.get(&corpus_id).cloned())
}

// ─── Ingest budget + mesh quiesce ──────────────────────────────
//
// Both controls live behind `/internal/*` daemon endpoints; the
// Settings panel pokes them to share the machine over long ingests
// without forcing a restart.
//
//   - `throttle_factor` ∈ (0.0, 1.0]: 1.0 = full speed (default),
//     0.5 ≈ duty-cycle 50% (sleep equal to embed wall time after
//     each batch). Use the corpus pause route to fully stop a
//     corpus — 0.0 is rejected by the daemon.
//   - `mesh_quiesced` bool: when true, this node neither pulls
//     peer-assigned work nor dispatches its own queue. The
//     SOVEREIGN_DISABLE_AUTO_COLLAB env var seeds the same atomic
//     at boot, so flipping at runtime via this command is reversible
//     without a daemon restart.

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct IngestBudgetState {
    pub throttle_factor: f32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct MeshQuiesceState {
    pub quiesced: bool,
}

#[tauri::command]
pub async fn get_ingest_budget() -> Result<IngestBudgetState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{DAEMON_INTERNAL_URL}/internal/ingest/budget");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /internal/ingest/budget: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/ingest/budget returned {status}: {body}"
        ));
    }
    resp.json::<IngestBudgetState>()
        .await
        .map_err(|e| format!("decode /internal/ingest/budget: {e}"))
}

#[tauri::command]
pub async fn set_ingest_budget(throttle_factor: f32) -> Result<IngestBudgetState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{DAEMON_INTERNAL_URL}/internal/ingest/budget");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "throttle_factor": throttle_factor }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/ingest/budget: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/ingest/budget returned {status}: {body}"
        ));
    }
    resp.json::<IngestBudgetState>()
        .await
        .map_err(|e| format!("decode /internal/ingest/budget: {e}"))
}

#[tauri::command]
pub async fn get_mesh_quiesced() -> Result<MeshQuiesceState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{DAEMON_INTERNAL_URL}/internal/mesh/quiesce");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET /internal/mesh/quiesce: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/mesh/quiesce returned {status}: {body}"
        ));
    }
    resp.json::<MeshQuiesceState>()
        .await
        .map_err(|e| format!("decode /internal/mesh/quiesce: {e}"))
}

#[tauri::command]
pub async fn set_mesh_quiesced(quiesced: bool) -> Result<MeshQuiesceState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{DAEMON_INTERNAL_URL}/internal/mesh/quiesce");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "quiesced": quiesced }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/mesh/quiesce: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/mesh/quiesce returned {status}: {body}"
        ));
    }
    resp.json::<MeshQuiesceState>()
        .await
        .map_err(|e| format!("decode /internal/mesh/quiesce: {e}"))
}

// ── Storage budget ───────────────────────────────────────────
//
// Mirror of `commonwealth_api::routes_internal::mesh_admin::
// StorageBudgetState`. Defined here as a flat serde struct so the
// desktop crate doesn't depend on commonwealth-api types just for
// this round-trip — keeps the TypeScript bridge simple. The wire
// shape must stay byte-compatible with the daemon's response.

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct StorageBudgetState {
    /// `None` means "no budget configured — gossiped free_storage_gb
    /// reports raw free disk and nothing is clamped".
    pub budget_bytes: Option<u64>,
    /// Sum of `index_size_bytes` across installed corpora as of the
    /// last gossip tick.
    pub used_bytes: u64,
    /// Free disk across all mounted volumes, in bytes. Same number
    /// the gossip path reports (modulo budget clamp).
    pub free_disk_bytes: u64,
    /// Suggested baseline the desktop's "Use recommended" affordance
    /// applies. Computed server-side from current free disk so a
    /// user with 250 GiB free sees a 100 GiB recommendation while a
    /// user with 60 GiB free sees a 30 GiB one.
    pub recommended_bytes: u64,
}

/// Read the daemon's current budget snapshot. Also seeds two
/// stateful defaults so the user never sees a blank "no budget"
/// state without an explicit choice:
///
///  1. If the persisted `desktop.toml` has a budget, push it to the
///     daemon (covers daemon restart while desktop kept running, or
///     first read after the desktop survived a launchd-restarted
///     daemon).
///  2. If neither the config nor the daemon has a budget, apply the
///     daemon's recommended baseline AND persist it. The user can
///     still override after — this just ensures Sovereign starts
///     out as a respectful tenant of the disk on first launch
///     instead of silently having no ceiling.
///
/// Returns whatever the daemon reports after these reconciliations.
#[tauri::command]
pub async fn get_storage_budget(
    state: State<'_, Arc<AppState>>,
) -> Result<StorageBudgetState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{DAEMON_INTERNAL_URL}/internal/storage/budget");

    let fetch = || async {
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("GET /internal/storage/budget: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "daemon /internal/storage/budget returned {status}: {body}"
            ));
        }
        resp.json::<StorageBudgetState>()
            .await
            .map_err(|e| format!("decode /internal/storage/budget: {e}"))
    };

    let snapshot = fetch().await?;
    let persisted = state.config.read().await.storage_budget_bytes;

    // Reconciliation case 1: config has a value, daemon doesn't.
    // Push the persisted value forward.
    if let (Some(persisted_bytes), None) = (persisted, snapshot.budget_bytes) {
        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "budget_bytes": persisted_bytes }))
            .send()
            .await
            .map_err(|e| format!("rehydrate storage budget: {e}"))?;
        if !resp.status().is_success() {
            tracing::warn!(
                status = %resp.status(),
                "get_storage_budget: rehydrate POST failed; the daemon's atomic stays at no-budget"
            );
        } else {
            return resp
                .json::<StorageBudgetState>()
                .await
                .map_err(|e| format!("decode rehydrate response: {e}"));
        }
    }

    // Reconciliation case 2: nobody has a value. Adopt the
    // recommended baseline AND persist it so the choice survives
    // restart. If the disk is too small for the recommendation to
    // be meaningful (under the AppState 1 GiB floor), skip — the
    // user will see the "Use recommended" affordance in Settings
    // and can apply it explicitly.
    if persisted.is_none() && snapshot.budget_bytes.is_none() {
        const MIN_BUDGET: u64 = 1_073_741_824;
        if snapshot.recommended_bytes >= MIN_BUDGET {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "budget_bytes": snapshot.recommended_bytes
                }))
                .send()
                .await
                .map_err(|e| format!("seed recommended storage budget: {e}"))?;
            if resp.status().is_success() {
                let applied: StorageBudgetState = resp
                    .json()
                    .await
                    .map_err(|e| format!("decode seed response: {e}"))?;
                let mut cfg = state.config.write().await;
                cfg.storage_budget_bytes = applied.budget_bytes;
                if let Err(e) = cfg.save() {
                    tracing::warn!(
                        "get_storage_budget: seed persist failed: {e}"
                    );
                }
                tracing::info!(
                    budget_bytes = ?applied.budget_bytes,
                    free_disk_bytes = applied.free_disk_bytes,
                    "storage_budget: seeded recommended baseline on first launch"
                );
                return Ok(applied);
            }
            tracing::warn!(
                status = %resp.status(),
                "get_storage_budget: seed POST failed; user will see no-budget state"
            );
        }
    }

    Ok(snapshot)
}

/// Push a new budget to the daemon. `budget_bytes = None` clears the
/// budget. Also rewrites the persisted `desktop.toml` so the choice
/// survives a restart — the daemon's atomic is runtime state, the
/// config file is the source of truth on next boot.
#[tauri::command]
pub async fn set_storage_budget(
    state: State<'_, Arc<AppState>>,
    budget_bytes: Option<u64>,
) -> Result<StorageBudgetState, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let url = format!("{DAEMON_INTERNAL_URL}/internal/storage/budget");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "budget_bytes": budget_bytes }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/storage/budget: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon /internal/storage/budget returned {status}: {body}"
        ));
    }
    let applied: StorageBudgetState = resp
        .json()
        .await
        .map_err(|e| format!("decode /internal/storage/budget: {e}"))?;

    // Persist into desktop.toml. Best-effort: if the disk write
    // fails the daemon already has the new value in its atomic, so
    // the runtime experience is correct; only the next-boot default
    // would revert. Log and surface the error so the UI can show it.
    {
        let mut cfg = state.config.write().await;
        cfg.storage_budget_bytes = applied.budget_bytes;
        if let Err(e) = cfg.save() {
            tracing::warn!("set_storage_budget: config save failed: {e}");
            return Err(format!("daemon updated but config save failed: {e}"));
        }
    }

    Ok(applied)
}

/// Return health details for a single installed corpus (claim/relationship
/// counts, article profiles flag). Loaded on demand so `list_corpora` stays
/// fast — the frontend calls this only when the user expands the detail panel.
#[tauri::command]
pub async fn get_corpus_health(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<CorpusHealthDetail>, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Ok(None),
    };
    drop(engine_guard);

    let index = match engine.open_index_for_corpus(&corpus_id).await {
        Ok(idx) => idx,
        Err(_) => return Ok(None),
    };

    // Count skeleton parse failures from the NDJSON log file.
    let failures_path = index.path().join("_skeleton_failures.ndjson");
    let parse_failure_count = if failures_path.exists() {
        std::fs::read_to_string(&failures_path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    // Check if a field skeleton exists (indicates enrichment ran).
    let has_skeleton = index.load_field_skeleton().ok().flatten().is_some();
    let skeleton_questions = if has_skeleton {
        index
            .load_field_skeleton()
            .ok()
            .flatten()
            .map(|s| s.canonical_questions.len() as u64)
            .unwrap_or(0)
    } else {
        0
    };

    Ok(Some(CorpusHealthDetail {
        corpus_id: corpus_id.clone(),
        claims_count: skeleton_questions,
        relationships_count: 0,
        has_article_profiles: has_skeleton,
        parse_failure_count,
    }))
}

/// Re-parse stored skeleton extraction failures using the improved repair
/// parser (unquoted-string fix, truncation repair, quality filter).
/// Does not re-run inference — only the saved raw responses are re-processed.
/// Salvaged questions are merged into the existing field_skeleton.json.
/// Returns the number of newly recovered questions.
#[tauri::command]
pub async fn retry_enrichment_failures(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<u64, String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    drop(engine_guard);

    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("Failed to open index for '{corpus_id}': {e}"))?;

    let (salvaged, still_failed) = corpus_engine::reprocess_skeleton_failures(&index)
        .map_err(|e| format!("Reprocessing failed: {e}"))?;

    tracing::info!(
        corpus_id = %corpus_id,
        salvaged = salvaged,
        still_failed = still_failed,
        "Skeleton failure reprocessing complete"
    );

    Ok(salvaged as u64)
}

// ─── Reading Surface ─────────────────────────────────────────────────────────
//
// Backs the desktop's glass-box reading UI. Frontend calls
// `read_get_chunk_neighbors(corpus, chunkId, radius)` after the user
// clicks a citation; the response shape mirrors the HTTP routes in
// `sovereign-mesh::reading_http` so the same UI works against either
// the in-process daemon (this code path) or a remote daemon (HTTP).

#[derive(Serialize)]
pub struct ChunkRecordDto {
    pub chunk_id: u64,
    pub corpus_id: String,
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub source_doc_id: Option<String>,
    pub section_id: Option<String>,
    /// Atom mentions located in `content` — byte offsets into the
    /// chunk's text. Empty when the corpus has no atlas, when the
    /// chunk wasn't produced by a sectioned chunker, or when no
    /// atom is anchored at this section.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub atom_spans: Vec<AtomSpanDto>,
    pub metadata: serde_json::Value,
    /// Populated when `corpus_id == "conversation-history"`. The
    /// reading surface uses presence of this field to pick the
    /// conversation-shaped renderer over the default book renderer.
    /// Mirrors `ConversationChunkMeta` in the HTTP layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationChunkMetaDto>,
}

#[derive(Serialize)]
pub struct ConversationChunkMetaDto {
    pub conversation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<ConversationSegmentDto>,
}

#[derive(Serialize)]
pub struct ConversationSegmentDto {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct AtomSpanDto {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub span_start: usize,
    pub span_end: usize,
    pub surface_form: String,
}

impl From<corpus_engine::atlas_traversal::AtomSpan> for AtomSpanDto {
    fn from(s: corpus_engine::atlas_traversal::AtomSpan) -> Self {
        Self {
            atom_id: s.atom_id,
            atom_type: s.atom_type,
            span_start: s.span_start,
            span_end: s.span_end,
            surface_form: s.surface_form,
        }
    }
}

#[derive(Serialize)]
pub struct NeighborWindowDto {
    pub center: ChunkRecordDto,
    pub prev: Vec<ChunkRecordDto>,
    pub next: Vec<ChunkRecordDto>,
    pub outbound_url: Option<String>,
    pub ordering: &'static str,
}

fn chunk_record_dto_from_row(
    corpus_id: &str,
    row: &corpus_engine::EnrichmentChunkRow,
    atoms: Option<&[corpus_engine::enrichment::atlas::AtomEnvelope]>,
    conversation: Option<ConversationChunkMetaDto>,
) -> ChunkRecordDto {
    let metadata: serde_json::Value = row
        .metadata_raw
        .as_deref()
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let section_id = metadata
        .as_object()
        .and_then(|obj| obj.get("section_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let atom_spans: Vec<AtomSpanDto> = match (atoms, section_id.as_deref()) {
        (Some(atoms), Some(_)) => {
            corpus_engine::atlas_traversal::detect_atom_spans(
                &row.content,
                section_id.as_deref(),
                atoms,
            )
            .into_iter()
            .map(AtomSpanDto::from)
            .collect()
        }
        _ => Vec::new(),
    };

    ChunkRecordDto {
        chunk_id: row.id,
        corpus_id: corpus_id.to_string(),
        content: row.content.clone(),
        title: row.title.clone(),
        url: row.url.clone(),
        source_doc_id: row.source_doc_id.clone(),
        section_id,
        atom_spans,
        metadata,
        conversation,
    }
}

const CONVERSATION_HISTORY_CORPUS_ID: &str = "conversation-history";

/// Same role-marker parser as the HTTP layer. Lives here in
/// duplicate (small, no shared crate available between mesh-http
/// and src-tauri) to keep the in-process Tauri path independent of
/// the HTTP path. Both shapes are wire-compatible.
fn parse_conversation_segments_dto(content: &str) -> Vec<ConversationSegmentDto> {
    if !content.starts_with('[') {
        return Vec::new();
    }
    let mut segments: Vec<ConversationSegmentDto> = Vec::new();
    let mut idx = 0usize;
    while idx < content.len() {
        if !content[idx..].starts_with('[') {
            break;
        }
        let role_close = match content[idx + 1..].find(']') {
            Some(rel) => idx + 1 + rel,
            None => break,
        };
        let role = content[idx + 1..role_close].to_string();
        let body_start = if content[role_close + 1..].starts_with(' ') {
            role_close + 2
        } else {
            role_close + 1
        };
        let body_end = match content[body_start..].find("\n\n[") {
            Some(rel) => body_start + rel,
            None => content.len(),
        };
        let body = content[body_start..body_end].to_string();
        if !role.is_empty() {
            segments.push(ConversationSegmentDto { role, content: body });
        }
        idx = if body_end == content.len() {
            content.len()
        } else {
            body_end + 2
        };
    }
    segments
}

/// Resolve conversation metadata for a chunk via the SQLite store.
/// Returns `None` for non-conversation corpora and for conversation
/// chunks whose `source_doc_id` (= conversation_id) couldn't be
/// looked up. Errors are swallowed so the chunk still renders.
async fn maybe_resolve_conversation_meta_for_commands(
    state: &State<'_, Arc<AppState>>,
    corpus_id: &str,
    row: &corpus_engine::EnrichmentChunkRow,
) -> Option<ConversationChunkMetaDto> {
    if corpus_id != CONVERSATION_HISTORY_CORPUS_ID {
        return None;
    }
    let conversation_id = row.source_doc_id.clone()?;
    let segments = parse_conversation_segments_dto(&row.content);
    let store_arc = state.store.read().await.clone();
    let (title, updated_at) = match store_arc {
        Some(s) => match s.get_conversation(&conversation_id).await {
            Ok(c) => (c.title, Some(c.updated_at)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };
    Some(ConversationChunkMetaDto {
        conversation_id,
        title,
        updated_at,
        segments,
    })
}

/// Load atlas atoms for the corpus from `atlas/atoms.json` next to
/// the index. Returns `None` when no atlas is present (corpus
/// hasn't been enriched) or when the file is unreadable — the atom
/// layer no-ops gracefully rather than failing the chunk fetch.
async fn load_atlas_atoms_for_commands(
    engine: &Arc<corpus_engine::CorpusEngine>,
    corpus_id: &str,
) -> Option<Vec<corpus_engine::enrichment::atlas::AtomEnvelope>> {
    let installed = engine.installed_indexes().await.ok()?;
    let entry = installed.iter().find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = entry.path.join("atlas");
    if !atlas_dir.exists() {
        return None;
    }
    match corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir) {
        Ok(file) => Some(file.atoms),
        Err(e) => {
            tracing::warn!(
                corpus = %corpus_id,
                ?atlas_dir,
                error = %e,
                "read_get_chunk_neighbors: atlas read failed; atom layer disabled",
            );
            None
        }
    }
}

#[tauri::command]
pub async fn read_get_chunk(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
) -> Result<Option<ChunkRecordDto>, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open index '{corpus_id}': {e}"))?;
    let mut rows = index
        .chunks_by_ids(&[chunk_id])
        .await
        .map_err(|e| format!("chunks_by_ids: {e}"))?;
    let atoms = load_atlas_atoms_for_commands(&engine, &corpus_id).await;
    let row_opt = rows.pop();
    let dto = match row_opt {
        Some(row) => {
            let conv =
                maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, &row)
                    .await;
            Some(chunk_record_dto_from_row(
                &corpus_id,
                &row,
                atoms.as_deref(),
                conv,
            ))
        }
        None => None,
    };
    Ok(dto)
}

#[tauri::command]
pub async fn read_get_chunk_neighbors(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
    radius: Option<usize>,
) -> Result<Option<NeighborWindowDto>, String> {
    let radius = radius.unwrap_or(1).min(5);
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open index '{corpus_id}': {e}"))?;
    let window = match index
        .neighbors(chunk_id, radius)
        .await
        .map_err(|e| format!("neighbors: {e}"))?
    {
        Some(w) => w,
        None => return Ok(None),
    };

    // Load the atlas once and reuse across all three chunks in
    // the window. atoms.json is small; per-chunk re-reads would
    // multiply IO without benefit.
    let atoms = load_atlas_atoms_for_commands(&engine, &corpus_id).await;
    let atoms_ref = atoms.as_deref();

    // Conversation augmentation per chunk. Cheap (one SQLite hit
    // per neighbor), and adjacent chunks tend to share a
    // conversation_id so the get_conversation cache hits hot.
    let center_conv =
        maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, &window.center)
            .await;
    let center = chunk_record_dto_from_row(
        &corpus_id,
        &window.center,
        atoms_ref,
        center_conv,
    );
    let outbound_url = center.url.clone();
    let mut prev: Vec<ChunkRecordDto> = Vec::with_capacity(window.prev.len());
    for r in &window.prev {
        let conv =
            maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, r).await;
        prev.push(chunk_record_dto_from_row(&corpus_id, r, atoms_ref, conv));
    }
    let mut next: Vec<ChunkRecordDto> = Vec::with_capacity(window.next.len());
    for r in &window.next {
        let conv =
            maybe_resolve_conversation_meta_for_commands(&state, &corpus_id, r).await;
        next.push(chunk_record_dto_from_row(&corpus_id, r, atoms_ref, conv));
    }

    Ok(Some(NeighborWindowDto {
        center,
        prev,
        next,
        outbound_url,
        ordering: window.ordering,
    }))
}

// ─── Atom Panel ──────────────────────────────────────────────────────────────
//
// Two endpoints back the desktop's atom panel: `read_get_atom_card`
// returns the atom card (canonical_name, description, salience,
// one-hop relations, cross-corpus bridges) and
// `read_get_atom_elsewhere` returns the section list + cross-corpus
// links so the user can jump to other places the atom appears. The
// section→chunk projection happens here via
// `index.resolve_sections_to_chunks` so the desktop receives ready-
// to-click chunk_ids.

#[derive(Serialize)]
pub struct AtomCardDto {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub corpus_id: String,
    pub canonical_name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub salience: Option<f32>,
    pub enrichment_depth: String,
    pub related: Vec<RelatedAtomDto>,
    pub cross_corpus: Vec<CrossCorpusLinkDto>,
}

#[derive(Serialize)]
pub struct RelatedAtomDto {
    pub atom_id: String,
    pub atom_type: &'static str,
    pub canonical_name: String,
    pub edge_type: &'static str,
    pub role: &'static str,
    pub confidence: f32,
}

#[derive(Serialize)]
pub struct CrossCorpusLinkDto {
    pub peer_corpus_id: String,
    pub peer_atom_id: String,
    pub peer_canonical_name: String,
    pub edge_type: &'static str,
    pub signal: String,
    pub confidence: f32,
}

#[derive(Serialize)]
pub struct AtomElsewhereDto {
    pub atom_id: String,
    pub corpus_id: String,
    pub same_corpus: Vec<SectionRefDto>,
    pub cross_corpus: Vec<CrossCorpusLinkDto>,
}

#[derive(Serialize)]
pub struct SectionRefDto {
    pub section_id: String,
    pub chunk_id: Option<u64>,
    pub preview: Option<String>,
}

async fn atlas_dir_for_atom_commands(
    engine: &Arc<corpus_engine::CorpusEngine>,
    corpus_id: &str,
) -> Option<std::path::PathBuf> {
    let installed = engine.installed_indexes().await.ok()?;
    let entry = installed.iter().find(|i| i.corpus_id == corpus_id)?;
    let atlas_dir = entry.path.join("atlas");
    if atlas_dir.exists() {
        Some(atlas_dir)
    } else {
        None
    }
}

#[tauri::command]
pub async fn read_get_atom_card(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<AtomCardDto>, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let Some(atlas_dir) = atlas_dir_for_atom_commands(&engine, &corpus_id).await else {
        return Ok(None);
    };
    let atoms = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?
        .atoms;
    let target = corpus_engine::enrichment::atlas::AtomId::from_raw(atom_id.clone());
    let Some(atom) = atoms.iter().find(|a| *a.id() == target) else {
        return Ok(None);
    };
    let edges = corpus_engine::enrichment::atlas::read_atlas_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross = corpus_engine::enrichment::atlas::read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    Ok(Some(build_atom_card_dto(&corpus_id, atom, &atoms, &edges, &cross)))
}

#[tauri::command]
pub async fn read_get_atom_elsewhere(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<AtomElsewhereDto>, String> {
    let engine = match state.corpus_engine.read().await.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    let Some(atlas_dir) = atlas_dir_for_atom_commands(&engine, &corpus_id).await else {
        return Ok(None);
    };
    let atoms = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms: {e}"))?
        .atoms;
    let target = corpus_engine::enrichment::atlas::AtomId::from_raw(atom_id.clone());
    let Some(atom) = atoms.iter().find(|a| *a.id() == target) else {
        return Ok(None);
    };

    let evidence = atom_evidence_section_refs_dto(atom);
    let unique_sections: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        evidence
            .iter()
            .map(|(s, _)| s.clone())
            .filter(|s| seen.insert(s.clone()))
            .collect()
    };

    let index = engine
        .open_index_for_corpus(&corpus_id)
        .await
        .map_err(|e| format!("open index '{corpus_id}': {e}"))?;
    let section_to_chunk = index
        .resolve_sections_to_chunks(&unique_sections)
        .await
        .map_err(|e| format!("resolve_sections: {e}"))?;

    let mut same_corpus: Vec<SectionRefDto> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (section_id, preview) in &evidence {
        if !seen.insert(section_id.clone()) {
            continue;
        }
        same_corpus.push(SectionRefDto {
            section_id: section_id.clone(),
            chunk_id: section_to_chunk.get(section_id).copied(),
            preview: preview.clone(),
        });
    }
    same_corpus.sort_by(|a, b| a.section_id.cmp(&b.section_id));

    let cross = corpus_engine::enrichment::atlas::read_atlas_cross_corpus_edges(&atlas_dir)
        .map(|f| f.edges)
        .unwrap_or_default();
    let cross_corpus = cross_corpus_links_dto(&target, &cross);

    Ok(Some(AtomElsewhereDto {
        atom_id: target.as_str().to_string(),
        corpus_id,
        same_corpus,
        cross_corpus,
    }))
}

fn build_atom_card_dto(
    corpus_id: &str,
    atom: &corpus_engine::enrichment::atlas::AtomEnvelope,
    all_atoms: &[corpus_engine::enrichment::atlas::AtomEnvelope],
    edges: &[corpus_engine::enrichment::atlas::Edge],
    cross_edges: &[corpus_engine::enrichment::atlas::CrossCorpusEdge],
) -> AtomCardDto {
    let (atom_type, canonical_name, aliases, description, salience) = atom_surface_dto(atom);
    let target_id = atom.id();
    let related: Vec<RelatedAtomDto> = edges
        .iter()
        .filter(|e| e.source == *target_id || e.target == *target_id)
        .filter_map(|e| {
            let (other_id, role) = if e.source == *target_id {
                (&e.target, "source")
            } else {
                (&e.source, "target")
            };
            let other = all_atoms.iter().find(|a| *a.id() == *other_id)?;
            let (other_type, other_name, _, _, _) = atom_surface_dto(other);
            Some(RelatedAtomDto {
                atom_id: other_id.as_str().to_string(),
                atom_type: other_type,
                canonical_name: other_name,
                edge_type: edge_type_label_dto(e.edge_type),
                role,
                confidence: e.confidence,
            })
        })
        .collect();
    let cross_corpus = cross_corpus_links_dto(target_id, cross_edges);
    AtomCardDto {
        atom_id: target_id.as_str().to_string(),
        atom_type,
        corpus_id: corpus_id.to_string(),
        canonical_name,
        aliases,
        description,
        salience,
        enrichment_depth: format!("{:?}", atom.enrichment_depth()),
        related,
        cross_corpus,
    }
}

fn atom_surface_dto(
    atom: &corpus_engine::enrichment::atlas::AtomEnvelope,
) -> (&'static str, String, Vec<String>, String, Option<f32>) {
    use corpus_engine::enrichment::atlas::AtomEnvelope;
    match atom {
        AtomEnvelope::Entity(e) => (
            "entity",
            e.canonical_name.clone(),
            e.aliases.clone(),
            e.description.clone(),
            Some(e.salience),
        ),
        AtomEnvelope::Event(e) => (
            "event",
            truncate_dto(&e.description, 80),
            Vec::new(),
            e.description.clone(),
            None,
        ),
        AtomEnvelope::State(s) => (
            "state",
            s.label.clone(),
            Vec::new(),
            format!("State of {}: {}", s.entity_id.as_str(), s.label),
            s.confidence,
        ),
        AtomEnvelope::Relation(r) => (
            "relation",
            r.label.clone(),
            Vec::new(),
            r.label.clone(),
            None,
        ),
        AtomEnvelope::Claim(c) => (
            "claim",
            truncate_dto(&c.content, 80),
            Vec::new(),
            c.content.clone(),
            c.confidence,
        ),
        AtomEnvelope::Question(q) => (
            "question",
            truncate_dto(&q.content, 80),
            Vec::new(),
            q.content.clone(),
            None,
        ),
        AtomEnvelope::Configuration(c) => (
            "configuration",
            c.label.clone(),
            Vec::new(),
            c.description.clone(),
            Some(c.confidence),
        ),
        AtomEnvelope::ArgumentReconstruction(a) => (
            "argument_reconstruction",
            a.name.clone(),
            Vec::new(),
            a.conclusion.clone(),
            None,
        ),
        AtomEnvelope::Position(p) => (
            "position",
            p.canonical_name.clone(),
            Vec::new(),
            p.content.clone(),
            Some(p.salience),
        ),
        AtomEnvelope::Opposition(o) => (
            "opposition",
            o.canonical_label.clone(),
            Vec::new(),
            if o.framing.is_empty() {
                format!("{} vs {}", o.left_label, o.right_label)
            } else {
                o.framing.clone()
            },
            Some(o.salience),
        ),
    }
}

fn edge_type_label_dto(t: corpus_engine::enrichment::atlas::EdgeType) -> &'static str {
    use corpus_engine::enrichment::atlas::EdgeType;
    match t {
        EdgeType::Transition => "transition",
        EdgeType::Causes => "causes",
        EdgeType::Grounds => "grounds",
        EdgeType::Tension => "tension",
        EdgeType::Involves => "involves",
        EdgeType::Composes => "composes",
        EdgeType::Configures => "configures",
        EdgeType::Grounding => "grounding",
        EdgeType::Framing => "framing",
        EdgeType::Provenance => "provenance",
        EdgeType::EvidenceFor | EdgeType::Concedes | EdgeType::OpposesIn => unreachable!("typed edges wired in Gap B Stage 4"),
    }
}

fn truncate_dto(s: &str, max_chars: usize) -> String {
    let trimmed: String = s.chars().take(max_chars).collect();
    if trimmed.chars().count() < s.chars().count() {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

fn atom_evidence_section_refs_dto(
    atom: &corpus_engine::enrichment::atlas::AtomEnvelope,
) -> Vec<(String, Option<String>)> {
    use corpus_engine::enrichment::atlas::AtomEnvelope;
    match atom {
        AtomEnvelope::Entity(e) => vec![(
            e.first_appearance.chunk_id.clone(),
            e.first_appearance.passage_preview.clone(),
        )],
        AtomEnvelope::Event(e) => {
            let mut out = vec![(e.section_position.section_id.clone(), None)];
            for c in &e.evidence {
                out.push((c.chunk_id.clone(), c.passage_preview.clone()));
            }
            out
        }
        AtomEnvelope::State(s) => s
            .evidence
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Relation(r) => r
            .evidence
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Claim(c) => c
            .evidence
            .iter()
            .map(|cr| (cr.chunk_id.clone(), cr.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Question(q) => q
            .raised_at
            .iter()
            .map(|c| (c.chunk_id.clone(), c.passage_preview.clone()))
            .collect(),
        AtomEnvelope::Configuration(c) => c
            .evidence
            .iter()
            .map(|cr| (cr.chunk_id.clone(), cr.passage_preview.clone()))
            .collect(),
        AtomEnvelope::ArgumentReconstruction(a) => {
            let mut out = vec![(a.section_position.section_id.clone(), None)];
            for c in &a.evidence {
                out.push((c.chunk_id.clone(), c.passage_preview.clone()));
            }
            out
        }
        AtomEnvelope::Position(_) | AtomEnvelope::Opposition(_) => unreachable!("typed atoms wired in Gap B Stage 4"),
    }
}

fn cross_corpus_links_dto(
    atom_id: &corpus_engine::enrichment::atlas::AtomId,
    edges: &[corpus_engine::enrichment::atlas::CrossCorpusEdge],
) -> Vec<CrossCorpusLinkDto> {
    edges
        .iter()
        .filter(|e| e.edge.source == *atom_id || e.edge.target == *atom_id)
        .map(|e| CrossCorpusLinkDto {
            peer_corpus_id: e.peer.corpus_id.clone(),
            peer_atom_id: e.peer.atom_id.as_str().to_string(),
            peer_canonical_name: e.peer.canonical_name.clone(),
            edge_type: edge_type_label_dto(e.edge.edge_type),
            signal: e.trace.signal.clone(),
            confidence: e.trace.confidence,
        })
        .collect()
}

// ─── Recipe Testing ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct RecipeValidateResult {
    pub passed: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub corpus_id: String,
    pub corpus_name: String,
    pub source_reachable: Option<bool>,
}

#[derive(Serialize)]
pub struct RecipeTestResult {
    pub passed: bool,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub recipe_id: String,
    pub recipe_name: String,
    pub records_attempted: usize,
    pub records_succeeded: usize,
    pub extraction_rate: f32,
    pub total_chunks: usize,
    pub avg_chars: f32,
    pub report_path: String,
    pub report_markdown: String,
}

/// Validate a recipe's fields without downloading any data.
///
/// Returns immediately — performs only static checks and an optional
/// HTTP HEAD request to the source URL.
#[tauri::command]
pub async fn recipe_validate(
    recipe_path: String,
    offline: bool,
) -> Result<RecipeValidateResult, String> {
    let path = PathBuf::from(&recipe_path);
    let engine = recipe_stub_engine();
    let options = corpus_engine::TestOptions {
        sample_size: 0,
        embed: false,
        offline,
        ..Default::default()
    };

    let report = engine
        .test_recipe(&path, &options)
        .await
        .map_err(|e| e.to_string())?;

    Ok(RecipeValidateResult {
        passed: report.validation.errors.is_empty(),
        errors: report.validation.errors.clone(),
        warnings: report.warnings(),
        corpus_id: report.recipe_id.clone(),
        corpus_name: report.recipe_name.clone(),
        source_reachable: report.validation.source_reachable,
    })
}

/// Run the full recipe test harness: validate → acquire sample →
/// extract → chunk → write TEST_REPORT.md.
///
/// Embedding is not available in this code path — the embed phase is
/// always skipped. The report is written to `<recipe_dir>/TEST_REPORT.md`.
#[tauri::command]
pub async fn recipe_test(
    recipe_path: String,
    sample_size: usize,
    offline: bool,
) -> Result<RecipeTestResult, String> {
    let path = PathBuf::from(&recipe_path);
    let output_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("TEST_REPORT.md");

    let engine = recipe_stub_engine();
    let options = corpus_engine::TestOptions {
        sample_size,
        embed: false,
        offline,
        output: Some(output_path.clone()),
        ..Default::default()
    };

    let report = engine
        .test_recipe(&path, &options)
        .await
        .map_err(|e| e.to_string())?;

    let markdown = report.to_markdown();

    if let Err(e) = std::fs::write(&output_path, &markdown) {
        tracing::warn!("Failed to write TEST_REPORT.md to {}: {e}", output_path.display());
    }

    let (records_attempted, records_succeeded, extraction_rate) = report
        .extraction
        .as_ref()
        .map(|e| (e.records_attempted, e.records_succeeded, e.extraction_rate))
        .unwrap_or((0, 0, 0.0));

    let (total_chunks, avg_chars) = report
        .chunking
        .as_ref()
        .map(|c| (c.total_chunks, c.avg_chars))
        .unwrap_or((0, 0.0));

    Ok(RecipeTestResult {
        passed: report.passed(),
        warnings: report.warnings(),
        errors: report.validation.errors.clone(),
        recipe_id: report.recipe_id.clone(),
        recipe_name: report.recipe_name.clone(),
        records_attempted,
        records_succeeded,
        extraction_rate,
        total_chunks,
        avg_chars,
        report_path: output_path.to_string_lossy().into_owned(),
        report_markdown: markdown,
    })
}

/// Build a `CorpusEngine` with a stub embed function for recipe testing.
/// The stub is never called because the embed phase is always disabled.
fn recipe_stub_engine() -> corpus_engine::CorpusEngine {
    let stub: corpus_engine::EmbedFn = std::sync::Arc::new(|_| {
        Box::pin(async { Ok(vec![0f32; 768]) })
    });
    let tmp = std::env::temp_dir().join("sovereign-recipe-test");
    corpus_engine::CorpusEngine::new(tmp.clone(), tmp, stub)
}

/// Kick off background installs for every corpus in the given tier.
/// Used by the setup wizard's "install tier" affordance.
async fn start_tier_installs(
    app_handle: &tauri::AppHandle,
    state: &Arc<AppState>,
    tier: &str,
) {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => {
            tracing::warn!("start_tier_installs: corpus engine not initialized");
            return;
        }
    };
    drop(engine_guard);

    let builtins = engine.builtin_corpora();
    for b in &builtins {
        if !tiers_for(&b.id).iter().any(|t| t == tier) {
            continue;
        }
        tracing::info!("Queuing corpus install for tier '{tier}': {}", b.id);
        // Reuse the install command's spawn-and-emit logic by calling it
        // directly. Each install runs in its own task; they don't block
        // each other but compete for download bandwidth.
        let app = app_handle.clone();
        let state_clone = Arc::clone(state);
        let cid = b.id.clone();
        // Synthesize a State<'_, Arc<AppState>> isn't possible here; just
        // duplicate the spawn pattern inline.
        tokio::spawn(async move {
            let engine_guard = state_clone.corpus_engine.read().await;
            let engine = match engine_guard.as_ref() {
                Some(e) => Arc::clone(e),
                None => return,
            };
            drop(engine_guard);

            let progress_cid = cid.clone();
            let progress_handle = app.clone();
            let progress_state = Arc::clone(&state_clone);
            let progress_cb: corpus_engine::ProgressCallback = Box::new(move |p| {
                let payload = ingest_progress_to_payload(&progress_cid, &p);
                if let Ok(mut map) = progress_state.install_progress.try_write() {
                    map.insert(payload.corpus_id.clone(), payload.clone());
                }
                let _ = progress_handle.emit("corpus-progress", payload);
            });

            let spec = corpus_engine::CorpusSpec::Builtin(cid.clone());
            if let Err(e) = engine.ingest(&spec, Some(progress_cb)).await {
                tracing::warn!("Tier install for '{cid}' failed: {e}");
            }
        });
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::IngestProgress;

    // ── tiers_for ───────────────────────────────────────────

    #[test]
    fn tiers_for_wikipedia_includes_essential_and_full() {
        let tiers = tiers_for("wikipedia");
        assert!(tiers.contains(&"essential".to_string()));
        assert!(tiers.contains(&"research".to_string()));
        assert!(tiers.contains(&"technical".to_string()));
        assert!(tiers.contains(&"full".to_string()));
    }

    #[test]
    fn tiers_for_sep_is_research_only() {
        let tiers = tiers_for("sep");
        assert!(tiers.contains(&"research".to_string()));
        assert!(tiers.contains(&"full".to_string()));
        // SEP is research-grade and not part of the essential
        // tier — installing it pulls multiple GB.
        assert!(!tiers.contains(&"essential".to_string()));
    }

    #[test]
    fn tiers_for_stackexchange_is_technical() {
        let tiers = tiers_for("stackexchange");
        assert!(tiers.contains(&"technical".to_string()));
        assert!(tiers.contains(&"full".to_string()));
        assert!(!tiers.contains(&"essential".to_string()));
    }

    #[test]
    fn tiers_for_unknown_corpus_falls_back_to_full() {
        let tiers = tiers_for("some_user_corpus");
        assert_eq!(tiers, vec!["full".to_string()]);
    }

    // ── ingest_progress_to_payload ──────────────────────────

    #[test]
    fn payload_for_downloading_carries_percent_and_size_message() {
        let payload = ingest_progress_to_payload(
            "wikipedia",
            &IngestProgress::Downloading {
                percent: 42.5,
                bytes_downloaded: 5_242_880, // 5 MB
                bytes_total: Some(10_485_760),
            },
        );
        assert_eq!(payload.corpus_id, "wikipedia");
        assert_eq!(payload.phase, "downloading");
        assert!((payload.percent - 42.5).abs() < 1e-3);
        // The message should describe the download size in MB so the
        // UI can show "5.0 MB" while progress is below 100%.
        let message = payload.message.expect("downloading payload should have a message");
        assert!(message.contains("MB"), "expected MB in message, got '{message}'");
    }

    #[test]
    fn payload_for_embedding_computes_percent_from_total() {
        let payload = ingest_progress_to_payload(
            "sep",
            &IngestProgress::Embedding {
                chunks_embedded: 250,
                total: 1000,
                docs_processed: 10,
                chunks_per_sec: 50.0,
                expected_docs: None,
            },
        );
        assert_eq!(payload.phase, "embedding");
        assert!((payload.percent - 25.0).abs() < 1e-3);
        assert_eq!(payload.chunks_processed, 250);
    }

    #[test]
    fn payload_for_embedding_handles_zero_total() {
        // The pipeline reports `total: 0` early, before it knows the
        // chunk count. The mapping must not divide-by-zero.
        let payload = ingest_progress_to_payload(
            "sep",
            &IngestProgress::Embedding {
                chunks_embedded: 0,
                total: 0,
                docs_processed: 0,
                chunks_per_sec: 0.0,
                expected_docs: None,
            },
        );
        assert_eq!(payload.percent, 0.0);
    }

    #[test]
    fn payload_for_embedding_does_not_overshoot_on_per_section_emit() {
        // Wikipedia JSONL emits one ExtractedDoc per section; for a
        // typical curated set that's ~10× the accepted-article count.
        // Confirm the live-event percent does NOT compute
        // `docs_processed / expected_docs` — that was an earlier
        // (wrong) attempt at filter-aware progress that hit 100%
        // within minutes of an embed run with hours left. Polling-side
        // shard-scan progress is the honest signal; the live-event
        // path falls back to the chunk-total ratio (0 until known).
        let payload = ingest_progress_to_payload(
            "wikipedia",
            &IngestProgress::Embedding {
                chunks_embedded: 339_200,
                total: 0, // unknown (streaming) → 0% live-event percent
                docs_processed: 592_253, // 11× over the title cap
                chunks_per_sec: 34.0,
                expected_docs: Some(51_222),
            },
        );
        assert_eq!(payload.phase, "embedding");
        assert_eq!(
            payload.percent, 0.0,
            "live-event path must defer to polling shard-scan progress, not lie about completion"
        );
        // The "/ Y articles" context still appears in the message.
        let msg = payload.message.as_deref().unwrap_or_default();
        assert!(msg.contains("articles"), "{msg}");
    }

    #[test]
    fn embed_message_omits_articles_when_no_expected_docs() {
        let m = format_embed_message(339_200, 128_000, 32.0, None);
        assert!(m.contains("128.0k docs"), "{m}");
        assert!(!m.contains("articles"), "{m}");
    }

    #[test]
    fn embed_message_includes_filter_scope_when_known() {
        // Wikipedia Core mid-run: filter expects 51,286 titles, the
        // pipeline has emitted 25,643 sections so far. The display
        // unit ("articles") is approximate but communicates the
        // operator-relevant scale.
        let m = format_embed_message(339_200, 25_643, 32.0, Some(51_286));
        assert!(m.contains("/ 51.3k articles"), "{m}");
        assert!(m.contains("339.2k chunks"), "{m}");
        assert!(!m.contains("docs"), "should swap in 'articles' wording: {m}");
    }

    #[test]
    fn embed_message_clamps_overshoot_to_expected() {
        // docs_processed > expected (sections-per-article > 1 for
        // wikipedia_jsonl). Clamp the displayed numerator so the
        // ratio reads sensibly instead of "128.0k / 51.3k".
        let m = format_embed_message(339_200, 128_000, 32.0, Some(51_286));
        assert!(m.contains("51.3k / 51.3k articles"), "{m}");
    }

    #[test]
    fn payload_for_complete_marks_full_progress() {
        let payload = ingest_progress_to_payload(
            "sep",
            &IngestProgress::Complete {
                total_chunks: 5000,
                duration_secs: 1234,
            },
        );
        assert_eq!(payload.phase, "complete");
        assert_eq!(payload.percent, 100.0);
        assert_eq!(payload.chunks_processed, 5000);
        let message = payload.message.expect("should include duration");
        assert!(message.contains("1234"));
    }

    /// Cover every variant of `IngestProgress` to catch the case where
    /// a future variant is added to corpus-engine but the desktop's
    /// mapping table is not updated. Without this, a new variant
    /// would silently fall through to whatever default behavior the
    /// match arm produces.
    #[test]
    fn payload_phase_is_set_for_every_progress_variant() {
        let cases = [
            IngestProgress::Downloading {
                percent: 0.0,
                bytes_downloaded: 0,
                bytes_total: None,
            },
            IngestProgress::Extracting {
                documents_processed: 1,
            },
            IngestProgress::Chunking { chunks_created: 1 },
            IngestProgress::Embedding {
                chunks_embedded: 1,
                total: 1,
                docs_processed: 1,
                chunks_per_sec: 1.0,
                expected_docs: None,
            },
            IngestProgress::Indexing {
                chunks_indexed: 1,
                total: 1,
            },
            IngestProgress::Complete {
                total_chunks: 1,
                duration_secs: 1,
            },
        ];
        for case in cases {
            let payload = ingest_progress_to_payload("test", &case);
            assert!(
                !payload.phase.is_empty(),
                "every IngestProgress variant must map to a non-empty phase string"
            );
            assert_eq!(payload.corpus_id, "test");
        }
    }
}
