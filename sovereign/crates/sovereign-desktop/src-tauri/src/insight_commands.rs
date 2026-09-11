// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use sovereign_contracts::types::{InsightPosition, InsightSinkState, InsightSource};

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

/// The client for the daemon's insight surface (rung 6): the SAME
/// `InsightService` in both boot modes — commissioned into the in-process
/// daemon on a Local boot, owned by the CLI daemon on an attached one — served
/// on loopback either way. The wire projection
/// (`sovereign_turn_client::InsightEntry`) maps 1:1 onto the DTO below (same
/// fields, embedding already stripped), so the frontend contract does not
/// depend on which boot answered (sv-surface D2).
fn insight_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

impl From<sovereign_turn_client::InsightEntry> for InsightNodeDto {
    fn from(e: sovereign_turn_client::InsightEntry) -> Self {
        Self {
            id: e.id,
            clipped_text: e.clipped_text,
            message_id: e.message_id,
            paragraph_index: e.paragraph_index,
            source: e.source,
            position: e.position,
            adjacent: e.adjacent,
            created_at: e.created_at,
            sink_state: e.sink_state,
        }
    }
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
    let source: InsightSource =
        serde_json::from_str(&source_json).map_err(|e| format!("Invalid source: {e}"))?;
    let position: Option<InsightPosition> = position_json
        .map(|j| serde_json::from_str(&j))
        .transpose()
        .map_err(|e| format!("Invalid position: {e}"))?;

    // The clip crosses the wire in BOTH modes — the daemon's service embeds
    // and persists. One clip decider (sv-surface D2).
    let entry = insight_client(&state)
        .clip_insight(sovereign_turn_client::ClipInsight {
            clipped_text: &clipped_text,
            message_id: &message_id,
            paragraph_index,
            source,
            position,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(InsightNodeDto::from(entry))
}

#[tauri::command]
pub async fn list_insights(
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<InsightNodeDto>, String> {
    let entries = insight_client(&state)
        .list_insights(limit)
        .await
        .map_err(|e| e.to_string())?;
    Ok(entries.into_iter().map(InsightNodeDto::from).collect())
}

#[tauri::command]
pub async fn search_insights(
    query: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<InsightNodeDto>, String> {
    let entries = insight_client(&state)
        .search_insights(&query)
        .await
        .map_err(|e| e.to_string())?;
    Ok(entries.into_iter().map(InsightNodeDto::from).collect())
}

#[tauri::command]
pub async fn delete_insight(id: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    insight_client(&state)
        .delete_insight(&id)
        .await
        .map_err(|e| e.to_string())
}

/// `GET /v1/insights/sinks` — the daemon's OWN sink registry, in both
/// boot modes (sv-surface D2's owed route, landed in 710a0326e).
///
/// This read THIS process's registry until now, which on an attached boot
/// is empty however many vaults the daemon has connected — `any_connected`
/// was false there regardless — and it hard-coded `sinks: vec![]` in every
/// mode. Both are gone: the route answers the real registry, and the DTO
/// carries whatever it holds.
#[tauri::command]
pub async fn get_sink_status(state: State<'_, Arc<AppState>>) -> Result<SinkStatusDto, String> {
    let status = insight_client(&state)
        .insight_sinks()
        .await
        .map_err(|e| e.to_string())?;
    Ok(SinkStatusDto {
        any_connected: status.any_connected,
        sinks: status
            .sinks
            .into_iter()
            .map(|s| SinkInfoDto {
                id: s.id,
                display_name: s.display_name,
                connected: s.connected,
            })
            .collect(),
    })
}

/// Start a conversation seeded with the text of the insights the user
/// gathered. The NODE FETCH crosses (sv-surface D8): `POST
/// /v1/insights/by-id` reads the daemon's own insight store — the one
/// `clip_insight` has written to since D2, and on an attached boot not the
/// one this process opened.
///
/// `missing` is reported, not dropped. `list_by_ids` silently omits an id
/// that names no live row, so a short list alone would have seeded a
/// conversation with fewer insights than the user selected and said nothing
/// (ARCH §18.3). The conversation itself is still written to this process's
/// store, which is where the chat surface reads it from.
#[tauri::command]
pub async fn explore_insights(
    node_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let store = state
        .store
        .read()
        .await
        .clone()
        .ok_or_else(|| "Store not initialized".to_string())?;

    // Parse first: an id the store could never match is the caller's error,
    // not an absence to report.
    for s in &node_ids {
        uuid::Uuid::parse_str(s).map_err(|e| format!("Invalid id: {e}"))?;
    }

    let fetched = insight_client(&state)
        .insights_by_id(&node_ids)
        .await
        .map_err(|e| format!("explore_insights: {e}"))?;
    if !fetched.missing.is_empty() {
        return Err(format!(
            "{} of the {} gathered insights no longer exist ({}). \
             Refresh the gather tray and try again.",
            fetched.missing.len(),
            node_ids.len(),
            fetched.missing.join(", ")
        ));
    }

    // Build context preamble from distillations.
    let context_preamble = fetched
        .insights
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
    let system_msg = sovereign_contracts::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conv_id.clone(),
        role: sovereign_contracts::types::Role::System,
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
        .save_message(&sovereign_contracts::types::Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conv_id.clone(),
            role: sovereign_contracts::types::Role::User,
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
