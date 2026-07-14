// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use sovereign_core::types::{InsightPosition, InsightSinkState, InsightSource};

use crate::state::AppState;

// ─── DTOs ────────────────────────────────────────────────────

/// DTO for the frontend — no raw bytes (embedding stripped).
#[derive(Serialize)]
pub struct InsightNodeDto {
    pub id: String,
    pub clipped_text: String,
    pub message_id: String,
    pub paragraph_index: usize,
    pub source: InsightSource,
    pub position: Option<InsightPosition>,
    pub adjacent: Vec<String>,
    pub created_at: String, // ISO 8601
    pub sink_state: InsightSinkState,
}

#[derive(Serialize)]
pub struct SinkStatusDto {
    pub any_connected: bool,
    pub sinks: Vec<SinkInfoDto>,
}

#[derive(Serialize)]
pub struct SinkInfoDto {
    pub id: String,
    pub display_name: String,
    pub connected: bool,
}

impl From<sovereign_core::types::InsightNode> for InsightNodeDto {
    fn from(n: sovereign_core::types::InsightNode) -> Self {
        Self {
            id: n.id.to_string(),
            clipped_text: n.clipped_text,
            message_id: n.message_id.to_string(),
            paragraph_index: n.paragraph_index,
            source: n.source,
            position: n.position,
            adjacent: n.adjacent,
            created_at: n.created_at.to_rfc3339(),
            sink_state: n.sink_state,
        }
    }
}

// ─── Helper ──────────────────────────────────────────────────

async fn get_insight_service(
    state: &AppState,
) -> Result<Arc<sovereign_core::insight::InsightService>, String> {
    state.insight_service.read().await.clone().ok_or_else(|| {
        "Insight service not initialized. Backend may still be starting.".to_string()
    })
}

use sovereign_core::time::unix_now as now;

// ─── Commands ────────────────────────────────────────────────

#[tauri::command]
pub async fn clip_insight(
    clipped_text: String,
    message_id: String,
    paragraph_index: usize,
    source_json: String,
    position_json: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<InsightNodeDto, String> {
    let service = get_insight_service(&state).await?;
    let source: InsightSource =
        serde_json::from_str(&source_json).map_err(|e| format!("Invalid source: {e}"))?;
    let position: Option<InsightPosition> = position_json
        .map(|j| serde_json::from_str(&j))
        .transpose()
        .map_err(|e| format!("Invalid position: {e}"))?;
    let message_id =
        uuid::Uuid::parse_str(&message_id).map_err(|e| format!("Invalid message_id: {e}"))?;

    let node = service
        .clip(&clipped_text, message_id, paragraph_index, source, position)
        .await
        .map_err(|e| e.to_string())?;

    Ok(InsightNodeDto::from(node))
}

#[tauri::command]
pub async fn list_insights(
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<InsightNodeDto>, String> {
    let service = get_insight_service(&state).await?;
    let nodes = service
        .store
        .list(limit.unwrap_or(50))
        .await
        .map_err(|e| e.to_string())?;
    Ok(nodes.into_iter().map(InsightNodeDto::from).collect())
}

#[tauri::command]
pub async fn search_insights(
    query: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<InsightNodeDto>, String> {
    let service = get_insight_service(&state).await?;
    let nodes = service
        .store
        .search_text(&query, 20)
        .await
        .map_err(|e| e.to_string())?;
    Ok(nodes.into_iter().map(InsightNodeDto::from).collect())
}

#[tauri::command]
pub async fn delete_insight(id: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let service = get_insight_service(&state).await?;
    let id = uuid::Uuid::parse_str(&id).map_err(|e| format!("Invalid id: {e}"))?;
    service.store.delete(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_sink_status(state: State<'_, Arc<AppState>>) -> Result<SinkStatusDto, String> {
    let service = get_insight_service(&state).await?;
    let any_connected = service.sinks.any_connected().await;
    Ok(SinkStatusDto {
        any_connected,
        sinks: vec![], // populated when Obsidian sink is added
    })
}

#[tauri::command]
pub async fn explore_insights(
    node_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let service = get_insight_service(&state).await?;
    let store = state
        .store
        .read()
        .await
        .clone()
        .ok_or_else(|| "Store not initialized".to_string())?;

    let ids: Vec<uuid::Uuid> = node_ids
        .iter()
        .map(|s| uuid::Uuid::parse_str(s))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("Invalid id: {e}"))?;

    let nodes = service
        .store
        .list_by_ids(&ids)
        .await
        .map_err(|e| e.to_string())?;

    // Build context preamble from distillations.
    let context_preamble = nodes
        .iter()
        .map(|n| {
            format!(
                "[From {} — {}]\n{}",
                n.source.article_title.as_deref().unwrap_or("unknown"),
                n.source.corpus_id.as_deref().unwrap_or(""),
                n.clipped_text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // Create a new conversation with the preamble as a system message.
    let conv_id = uuid::Uuid::new_v4().to_string();

    // Save a system message with the insight context.
    let system_msg = sovereign_core::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conv_id.clone(),
        role: sovereign_core::types::Role::System,
        content: format!(
            "The user has gathered the following insights from previous research. \
             Use them as context for the conversation.\n\n{context_preamble}"
        ),
        created_at: now(),
        metadata: None,
        version: 0,
    };

    // Save conversation first, then the system message.
    store
        .save_message(&sovereign_core::types::Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conv_id.clone(),
            role: sovereign_core::types::Role::User,
            content: String::new(), // dummy to create conversation
            created_at: now(),
            metadata: None,
            version: 0,
        })
        .await
        .map_err(|e| e.to_string())?;

    store
        .save_message(&system_msg)
        .await
        .map_err(|e| e.to_string())?;

    Ok(conv_id)
}
