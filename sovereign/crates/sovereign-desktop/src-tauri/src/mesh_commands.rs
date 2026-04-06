//! Tauri commands for mesh operations.
//!
//! These are called from the Sovereign frontend (the Tauri webview) when
//! users interact with the Community Mesh section of the settings UI.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use sovereign_mesh::{parse_deep_link, JoinConfirmation, MeshState};

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMeshResponse {
    pub mesh_name: String,
    pub join_key: String,
    pub join_link: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinMeshResponse {
    pub mesh_name: String,
    pub node_id: String,
}

/// Create a new mesh and return the join link for sharing.
#[tauri::command]
pub async fn mesh_create(
    state: State<'_, Arc<AppState>>,
    mesh_name: String,
) -> Result<CreateMeshResponse, String> {
    let config = state.config.read().await;
    let node_name = hostname_or_default();
    drop(config);

    let result = state
        .mesh
        .create_mesh(&mesh_name, &node_name)
        .await
        .map_err(|e| e.to_string())?;

    Ok(CreateMeshResponse {
        mesh_name: result.mesh_name,
        join_key: result.join_key,
        join_link: result.join_link,
    })
}

/// Parse a deep link and return the join confirmation info.
/// Called when the user taps a `sovereign://join/...` link but before they
/// confirm — gives the UI info to render the confirmation dialog.
#[tauri::command]
pub async fn mesh_preview_join_link(
    link: String,
) -> Result<JoinConfirmation, String> {
    let parsed = parse_deep_link(&link).ok_or_else(|| "Invalid join link".to_string())?;
    sovereign_mesh::deep_link::join_confirmation_from_link(&parsed)
        .ok_or_else(|| "Could not build confirmation from link".to_string())
}

/// Join a mesh from a deep link.
#[tauri::command]
pub async fn mesh_join(
    state: State<'_, Arc<AppState>>,
    link: String,
) -> Result<JoinMeshResponse, String> {
    let parsed = parse_deep_link(&link).ok_or_else(|| "Invalid join link".to_string())?;
    let node_name = hostname_or_default();

    let result = state
        .mesh
        .join_mesh(&parsed, &node_name)
        .await
        .map_err(|e| e.to_string())?;

    Ok(JoinMeshResponse {
        mesh_name: result.mesh_name,
        node_id: result.node_id,
    })
}

/// Get the current mesh state for UI display (members, knowledge, contribution).
/// Returns `null` if no mesh is active.
#[tauri::command]
pub async fn mesh_get_state(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<MeshStateResponse>, String> {
    let Some(mesh_state) = state.mesh.mesh_state().await else {
        return Ok(None);
    };
    Ok(Some(MeshStateResponse::from(mesh_state)))
}

/// Check if the mesh daemon is currently running.
#[tauri::command]
pub async fn mesh_is_running(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.mesh.is_running().await)
}

/// Leave the current mesh and stop the daemon.
#[tauri::command]
pub async fn mesh_leave(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.mesh.stop().await.map_err(|e| e.to_string())
}

// ── Serializable wrappers for MeshState ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStateResponse {
    pub status: sovereign_mesh::MeshStatus,
    pub members: Vec<sovereign_mesh::MeshMember>,
    pub corpora: Vec<sovereign_mesh::MeshCorpus>,
    pub contribution: Option<sovereign_mesh::ContributionSummary>,
}

impl From<MeshState> for MeshStateResponse {
    fn from(s: MeshState) -> Self {
        Self {
            status: s.status,
            members: s.members,
            corpora: s.corpora,
            contribution: s.contribution,
        }
    }
}

// ── Helpers ──────────────────────────────────────────────

fn hostname_or_default() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "sovereign-node".to_string())
}
