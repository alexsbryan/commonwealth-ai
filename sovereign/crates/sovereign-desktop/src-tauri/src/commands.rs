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
        "wikipedia" => vec!["essential".into(), "research".into(), "technical".into(), "full".into()],
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
#[tauri::command]
pub async fn send_message_stream(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
) -> Result<StreamStartedResponse, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap().clone();
    drop(guard);

    state.approval.set_task_id(&conversation_id).await;

    // Try streaming path first.
    match runtime
        .handle_message_stream(&message, &conversation_id)
        .await
    {
        Ok(handle) => {
            let message_id = handle.message_id.clone();
            let conversation_id_owned = conversation_id.clone();
            let app = app_handle.clone();
            let mut stream = handle.stream;

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
                let _ = app.emit(
                    "message-complete",
                    MessageCompletePayload {
                        conversation_id: conversation_id_owned,
                        message_id,
                        full_text,
                    },
                );
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
                        let _ = app.emit(
                            "message-complete",
                            MessageCompletePayload {
                                conversation_id: conversation_id_owned,
                                message_id: response.message.id,
                                full_text: response.message.content,
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
                    }
                }
                drop(pending_clone);
            });

            Ok(StreamStartedResponse {
                message_id: pending_id,
                streaming: false,
            })
        }
    }
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    message: String,
    conversation_id: String,
) -> Result<MessageResponse, String> {
    let guard = require_runtime!(state);
    let runtime = guard.as_ref().unwrap();

    state.approval.set_task_id(&conversation_id).await;

    let response = runtime
        .handle_message(&message, &conversation_id)
        .await
        .map_err(|e| e.to_string())?;

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
    let old_embed = state.config.read().await.embed_model_path.clone();
    let new_embed = config.embed_model_path.clone();
    *state.config.write().await = config;
    // If the embedding model changed, drop the cached inference so bootstrap
    // reloads it with the new embed model path.
    if old_embed != new_embed {
        *state.inference.write().await = None;
    }
    state::rebuild_runtime(&state).await
}

#[tauri::command]
pub async fn is_setup_complete(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.config.read().await.setup_complete)
}

#[tauri::command]
pub async fn complete_setup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    setup: SetupConfig,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    config.model_path = setup.model_path.into();
    config.primary_model_path = setup.primary_model_path.map(|p| p.into());
    config.embed_model_path = setup.embed_model_path.map(|p| p.into());
    if let Some(dir) = setup.data_dir {
        config.data_dir = dir.into();
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

fn scan_directory_flat(dir: &Path, label: &str, results: &mut Vec<DiscoveredModel>, seen: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "gguf") {
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

    // Skip if already downloaded.
    if dest.exists() {
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

    let part_path = models_dir.join(format!("{}.part", &request.file_name));

    let response = reqwest::get(&request.url)
        .await
        .map_err(|e| format!("Download request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
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
        } => CorpusProgressPayload {
            corpus_id: corpus_id.into(),
            phase: "embedding".into(),
            percent: if *total > 0 {
                (*chunks_embedded as f32 / *total as f32) * 100.0
            } else {
                0.0
            },
            chunks_processed: *chunks_embedded,
            message: Some(format!(
                "{chunks_embedded} chunks · {docs_processed} docs · {chunks_per_sec:.1} chunks/s"
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
    let builtins = engine.builtin_corpora();

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
        let installed_info = installed.iter().find(|i| i.corpus_id == b.id && !i.is_shard);
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

        let vector_index_ready = if installed_info.is_some() {
            if let Some(ref s) = store_opt {
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

        match idx.build_indexes(true, false, Some(&*on_progress)).await {
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

#[tauri::command]
pub async fn install_corpus(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    drop(engine_guard);

    let state_ref = Arc::clone(&state);
    let cid = corpus_id.clone();

    tokio::spawn(async move {
        // Mark as installing right away so the UI flips state before the
        // first IngestProgress event arrives.
        {
            let initial = CorpusProgressPayload {
                corpus_id: cid.clone(),
                phase: "downloading".into(),
                percent: 0.0,
                chunks_processed: 0,
                message: Some("Starting…".into()),
            };
            if let Ok(mut map) = state_ref.install_progress.try_write() {
                map.insert(cid.clone(), initial.clone());
            }
            let _ = app_handle.emit("corpus-progress", initial);
        }

        // The progress callback runs from inside the engine's async tasks.
        let progress_cid = cid.clone();
        let progress_handle = app_handle.clone();
        let progress_state = Arc::clone(&state_ref);
        let progress_cb: corpus_engine::ProgressCallback = Box::new(move |p| {
            let payload = ingest_progress_to_payload(&progress_cid, &p);
            if let Ok(mut map) = progress_state.install_progress.try_write() {
                map.insert(payload.corpus_id.clone(), payload.clone());
            }
            let _ = progress_handle.emit("corpus-progress", payload);
        });

        let spec = corpus_engine::CorpusSpec::Builtin(cid.clone());
        match engine.ingest(&spec, Some(progress_cb)).await {
            Ok(result) => {
                tracing::info!(
                    "Corpus '{cid}' installed: {} chunks, {:.1} MB, {}s",
                    result.chunks_created,
                    result.index_size_bytes as f64 / 1_048_576.0,
                    result.duration_secs,
                );
                let payload = CorpusProgressPayload {
                    corpus_id: cid.clone(),
                    phase: "complete".into(),
                    percent: 100.0,
                    chunks_processed: result.chunks_created,
                    message: Some(format!("Done in {}s", result.duration_secs)),
                };
                if let Ok(mut map) = state_ref.install_progress.try_write() {
                    map.insert(cid.clone(), payload.clone());
                }
                let _ = app_handle.emit("corpus-progress", payload);
            }
            Err(e) => {
                tracing::error!("Corpus '{cid}' install failed: {e}");
                let payload = CorpusProgressPayload {
                    corpus_id: cid.clone(),
                    phase: "failed".into(),
                    percent: 0.0,
                    chunks_processed: 0,
                    message: Some(e.to_string()),
                };
                if let Ok(mut map) = state_ref.install_progress.try_write() {
                    map.insert(cid.clone(), payload.clone());
                }
                let _ = app_handle.emit("corpus-progress", payload);
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn remove_corpus(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let engine_guard = state.corpus_engine.read().await;
    let engine = match engine_guard.as_ref() {
        Some(e) => Arc::clone(e),
        None => return Err("Corpus engine not initialized".into()),
    };
    drop(engine_guard);

    engine.remove_index(&corpus_id).map_err(|e| e.to_string())?;

    // Clear any stale progress entry so the UI returns to "not_installed".
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

    // Enrichment details (claims, relationships, article profiles, parse
    // failures) were removed in the field-model refactor. Return zeroed
    // defaults so the frontend still gets a valid response.
    let _ = index;
    Ok(Some(CorpusHealthDetail {
        corpus_id: corpus_id.clone(),
        claims_count: 0,
        relationships_count: 0,
        has_article_profiles: false,
        parse_failure_count: 0,
    }))
}

/// Re-parse stored enrichment failures using the truncation-repair parser.
/// Does not re-run inference — only the saved raw responses are re-processed.
/// Returns the number of newly recovered claims.
///
/// NOTE: The underlying engine method was removed in the field-model refactor.
/// This stub is kept so the Tauri command handler registration stays valid.
#[tauri::command]
pub async fn retry_enrichment_failures(
    _state: State<'_, Arc<AppState>>,
    _corpus_id: String,
) -> Result<u64, String> {
    Ok(0)
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
            },
        );
        assert_eq!(payload.percent, 0.0);
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
