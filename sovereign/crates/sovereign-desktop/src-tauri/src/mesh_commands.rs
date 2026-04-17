//! Tauri commands for mesh operations.
//!
//! These are called from the Sovereign frontend (the Tauri webview) when
//! users interact with the Community Mesh section of the settings UI.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use sovereign_mesh::{parse_deep_link, JoinConfirmation, MeshState};

use crate::bootstrap::BootstrapMode;
use crate::state::{resolve_node_name, AppState};

/// In Attach mode, mesh mutations go over HTTP to the daemon owning
/// `:9741`. Returns the client port the CLI daemon is answering on,
/// or `None` if we're in Local mode and should use the in-process
/// daemon instead.
fn attached_port(state: &AppState) -> Option<u16> {
    match &state.bootstrap_mode {
        BootstrapMode::Attach { client_port } => Some(*client_port),
        BootstrapMode::Local { .. } => None,
    }
}

/// Shared reqwest client with a reasonable timeout — mesh HTTP calls
/// should either succeed fast or fail fast (a hanging daemon is
/// worse than a clear error).
fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())
}

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
///
/// Local mode drives the in-process `EmbeddedDaemon`. Attach mode
/// (no in-process daemon) POSTs `/v1/mesh/create` against the CLI
/// daemon owning `:9741`.
#[tauri::command]
pub async fn mesh_create(
    state: State<'_, Arc<AppState>>,
    mesh_name: String,
) -> Result<CreateMeshResponse, String> {
    let node_name = {
        let config = state.config.read().await;
        resolve_node_name(&config.node_name)
    };

    if let Some(port) = attached_port(&state) {
        // Attach mode — route through the daemon's HTTP API.
        let client = http_client()?;
        let body = serde_json::json!({ "name": mesh_name, "node_name": node_name });
        let resp = client
            .post(format!("http://localhost:{port}/v1/mesh/create"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("mesh create: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("mesh create failed ({status}): {text}"));
        }
        return resp
            .json::<CreateMeshResponse>()
            .await
            .map_err(|e| format!("parse mesh/create response: {e}"));
    }

    let Some(mesh) = state.mesh.as_ref() else {
        return Err("mesh daemon not available".into());
    };
    let result = mesh
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

/// Join a mesh from a deep link or key.
#[tauri::command]
pub async fn mesh_join(
    state: State<'_, Arc<AppState>>,
    link: String,
) -> Result<JoinMeshResponse, String> {
    let node_name = {
        let config = state.config.read().await;
        resolve_node_name(&config.node_name)
    };

    if let Some(port) = attached_port(&state) {
        // Attach mode — the daemon's `/v1/mesh/join` accepts any of
        // the three forms (bare key, https URL, sovereign:// link) so
        // we pass `link` through unchanged.
        let client = http_client()?;
        let body = serde_json::json!({ "key_or_url": link, "node_name": node_name });
        let resp = client
            .post(format!("http://localhost:{port}/v1/mesh/join"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("mesh join: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("mesh join failed ({status}): {text}"));
        }
        return resp
            .json::<JoinMeshResponse>()
            .await
            .map_err(|e| format!("parse mesh/join response: {e}"));
    }

    // Local mode keeps the deep-link-only parser for backward compat;
    // the HTTP path above accepts bare keys too.
    let Some(mesh) = state.mesh.as_ref() else {
        return Err("mesh daemon not available".into());
    };
    let parsed = parse_deep_link(&link).ok_or_else(|| "Invalid join link".to_string())?;
    let result = mesh
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
    if let Some(port) = attached_port(&state) {
        // Attach mode — read-only status over HTTP. The daemon's
        // `/v1/mesh/status` returns a flat shape; we up-convert it
        // into the `MeshStateResponse` the frontend already renders.
        let client = http_client()?;
        let resp = client
            .get(format!("http://localhost:{port}/v1/mesh/status"))
            .send()
            .await
            .map_err(|e| format!("mesh status: {e}"))?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let remote: sovereign_mesh::mesh_http::StatusResponse =
            resp.json().await.map_err(|e| format!("parse mesh/status: {e}"))?;
        if remote.mesh_name.is_none() {
            return Ok(None);
        }
        return Ok(Some(MeshStateResponse::from_remote_status(remote)));
    }

    let Some(mesh) = state.mesh.as_ref() else {
        return Ok(None);
    };
    let Some(mesh_state) = mesh.mesh_state().await else {
        return Ok(None);
    };
    Ok(Some(MeshStateResponse::from(mesh_state)))
}

/// Check if the mesh daemon is currently running. In Attach mode we
/// always report `true` — the CLI daemon is by definition running or
/// we wouldn't have detected Attach in the first place.
#[tauri::command]
pub async fn mesh_is_running(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    match state.mesh.as_ref() {
        Some(m) => Ok(m.is_running().await),
        None => Ok(true), // Attach mode: the external daemon is always running.
    }
}

/// Leave the current mesh and stop the daemon.
#[tauri::command]
pub async fn mesh_leave(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if let Some(port) = attached_port(&state) {
        let client = http_client()?;
        let resp = client
            .post(format!("http://localhost:{port}/v1/mesh/leave"))
            .send()
            .await
            .map_err(|e| format!("mesh leave: {e}"))?;
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NO_CONTENT {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("mesh leave failed ({status}): {text}"));
        }
        return Ok(());
    }
    let Some(mesh) = state.mesh.as_ref() else {
        return Err("mesh daemon not available".into());
    };
    mesh.stop().await.map_err(|e| e.to_string())
}

// ── Diagnostics ──────────────────────────────────────────
//
// Surfaces the mDNS discovery table to the UI so the user can
// visually confirm that two machines on the same LAN can see each
// other. Without this, a join failure is indistinguishable from a
// successful join with silent peer invisibility.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPeerDto {
    pub node_id: String,
    pub mesh_id_hex: String,
    /// The peer's *mesh* name (e.g. "Masonic Mesh"). Surfaced in the
    /// diagnostics panel so the user can tell which mesh each peer
    /// claims membership in — load-bearing once more than one mesh
    /// coexists on a LAN, and for debugging join-name mismatches.
    pub mesh_name: String,
    /// The peer's node/host label.
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshDiagnostics {
    pub discovered_peers: Vec<DiscoveredPeerDto>,
    pub daemon_running: bool,
}

/// Snapshot of mDNS-discovered peers and daemon health. Polled by
/// the MeshDiagnosticsPanel every few seconds.
#[tauri::command]
pub async fn mesh_diagnostics(
    state: State<'_, Arc<AppState>>,
) -> Result<MeshDiagnostics, String> {
    let (peers, daemon_running) = match state.mesh.as_ref() {
        Some(m) => {
            let peers = m
                .discovered_peers()
                .await
                .into_iter()
                .map(|p| DiscoveredPeerDto {
                    node_id: p.node_id.to_string(),
                    mesh_id_hex: p.mesh_id_hex,
                    mesh_name: p.mesh_name,
                    name: p.name,
                    address: p.address.to_string(),
                })
                .collect();
            (peers, m.is_running().await)
        }
        None => {
            // Attach mode: the CLI daemon owns mDNS discovery. Returning
            // an empty peer list today keeps the diagnostics panel happy;
            // task #37 will proxy `GET /v1/mesh/status` for the real list.
            (Vec::new(), true)
        }
    };
    Ok(MeshDiagnostics { discovered_peers: peers, daemon_running })
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

impl MeshStateResponse {
    /// Build a `MeshStateResponse` from the flat HTTP `StatusResponse`
    /// the CLI daemon returns over `/v1/mesh/status`. The UI surface
    /// (members list, online counts) is covered; rich fields that
    /// weren't surfaced over HTTP (contribution ledger, corpora shard
    /// plan) come back empty — they're populated on the daemon side
    /// and a future iteration can extend the HTTP shape to include them.
    pub fn from_remote_status(remote: sovereign_mesh::mesh_http::StatusResponse) -> Self {
        use sovereign_mesh::{MemberStatus, MeshMember, MeshStatus};
        let members: Vec<MeshMember> = remote
            .members
            .into_iter()
            .map(|m| MeshMember {
                name: m.name,
                node_id: m.node_id,
                is_self: m.is_self,
                status: match m.status.as_str() {
                    "online" => MemberStatus::Online,
                    "busy" => MemberStatus::Busy,
                    "away" => MemberStatus::Away,
                    _ => MemberStatus::Offline,
                },
                contribution_level: 0,
                contribution_label: String::new(),
            })
            .collect();
        Self {
            status: MeshStatus {
                name: remote.mesh_name.unwrap_or_default(),
                members_online: remote.members_online,
                members_total: remote.members_total,
                model_name: None,
                knowledge_corpora: Vec::new(),
                is_connected: remote.running,
            },
            members,
            corpora: Vec::new(),
            contribution: None,
        }
    }
}

