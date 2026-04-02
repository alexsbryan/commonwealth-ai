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
}

#[derive(Deserialize)]
pub struct SetupConfig {
    pub model_path: String,
    #[serde(default)]
    pub primary_model_path: Option<String>,
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

// ─── Commands ────────────────────────────────────────────────

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
        message_id: response.message.id,
        role,
        content: response.message.content,
        task: task_summary,
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
    *state.config.write().await = config;
    state::rebuild_runtime(&state).await
}

#[tauri::command]
pub async fn is_setup_complete(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.config.read().await.setup_complete)
}

#[tauri::command]
pub async fn complete_setup(
    state: State<'_, Arc<AppState>>,
    setup: SetupConfig,
) -> Result<(), String> {
    let mut config = state.config.write().await;
    config.model_path = setup.model_path.into();
    config.primary_model_path = setup.primary_model_path.map(|p| p.into());
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
    config.setup_complete = true;

    config.save()?;
    drop(config);

    state::bootstrap(&state).await
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
    };
    runtime
        .store
        .save_message(&user_msg)
        .await
        .map_err(|e| e.to_string())?;

    // Execute web_search tool directly.
    let tool = runtime
        .tools
        .get("web_search")
        .map_err(|_| "Web search tool is not enabled.".to_string())?;

    let params = serde_json::json!({ "query": query });
    let ctx = sovereign_core::types::ToolContext {
        conversation_id: conversation_id.clone(),
        task_id: None,
        working_directory: None,
    };

    let output = tool
        .execute(&params, &ctx)
        .await
        .map_err(|e| format!("Web search failed: {e}"))?;

    let content = match output {
        sovereign_core::types::StepOutput::Text(t) => t,
        sovereign_core::types::StepOutput::Json(v) => v.to_string(),
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
