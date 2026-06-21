// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for mesh operations.
//!
//! These are called from the Sovereign frontend (the Tauri webview) when
//! users interact with the Community Mesh section of the settings UI.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use sovereign_mesh::mesh_discovery::RelayCandidate;
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
    /// Bearer token a remote peer/client must present — shown beside
    /// the join key on the invite screen. `None` if the daemon stayed
    /// loopback-only.
    #[serde(default)]
    pub client_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinMeshResponse {
    pub mesh_name: String,
    pub node_id: String,
    #[serde(default)]
    pub client_token: Option<String>,
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
    encrypt: Option<bool>,
) -> Result<CreateMeshResponse, String> {
    let encrypt = encrypt.unwrap_or(false);
    let node_name = {
        let config = state.config.read().await;
        resolve_node_name(&config.node_name)
    };

    if let Some(port) = attached_port(&state) {
        // Attach mode — route through the daemon's HTTP API.
        let client = http_client()?;
        let body = serde_json::json!({
            "name": mesh_name,
            "node_name": node_name,
            "encrypt": encrypt,
        });
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
    // Explicit create = opt into serving remote peers → expose the
    // client API (bind non-loopback + require a bearer token) before
    // start_daemon, so it binds wide on first start with no restart.
    mesh.expose_client_api();
    let result = mesh
        .create_mesh_with(&mesh_name, &node_name, encrypt)
        .await
        .map_err(|e| e.to_string())?;
    Ok(CreateMeshResponse {
        mesh_name: result.mesh_name,
        join_key: result.join_key,
        join_link: result.join_link,
        client_token: result.client_token,
    })
}

/// Parse a deep link and return the join confirmation info.
/// Called when the user taps a `sovereign://join/...` link but before they
/// confirm — gives the UI info to render the confirmation dialog.
#[tauri::command]
pub async fn mesh_preview_join_link(link: String) -> Result<JoinConfirmation, String> {
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
    mesh.expose_client_api();
    let result = mesh
        .join_mesh(&parsed, &node_name)
        .await
        .map_err(|e| e.to_string())?;
    Ok(JoinMeshResponse {
        mesh_name: result.mesh_name,
        node_id: result.node_id,
        client_token: result.client_token,
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
            // Glassbox: a non-2xx here is one way Members ends up
            // blank — surface it instead of silently returning None.
            tracing::warn!(
                target: "mesh_state",
                port,
                status = %resp.status(),
                "mesh_get_state(attach): /v1/mesh/status non-2xx — Members will be empty"
            );
            return Ok(None);
        }
        let remote: sovereign_mesh::mesh_http::StatusResponse = resp
            .json()
            .await
            .map_err(|e| format!("parse mesh/status: {e}"))?;
        if remote.mesh_name.is_none() {
            tracing::warn!(
                target: "mesh_state",
                port,
                "mesh_get_state(attach): daemon reports no active mesh — Members empty"
            );
            return Ok(None);
        }
        tracing::info!(
            target: "mesh_state",
            mode = "attach",
            port,
            members = remote.members.len(),
            members_online = remote.members_online,
            "mesh_get_state(attach): fetched mesh status"
        );
        return Ok(Some(MeshStateResponse::from_remote_status(remote)));
    }

    let Some(mesh) = state.mesh.as_ref() else {
        tracing::warn!(
            target: "mesh_state",
            "mesh_get_state(local): no in-process mesh daemon — Members empty"
        );
        return Ok(None);
    };
    let Some(mesh_state) = mesh.mesh_state().await else {
        tracing::warn!(
            target: "mesh_state",
            "mesh_get_state(local): in-process daemon has no active mesh — Members empty"
        );
        return Ok(None);
    };
    // Glassbox: in Local mode this desktop reads its OWN embedded
    // daemon. If that shows zero members while a separate daemon is
    // serving the mesh on :9741, the app attached to the wrong process
    // (a startup-probe race — see `bootstrap::detect`). Log the count
    // so one real run distinguishes "genuinely solo" from "wrong
    // daemon."
    tracing::info!(
        target: "mesh_state",
        mode = "local",
        members = mesh_state.members.len(),
        "mesh_get_state(local): read in-process mesh state"
    );
    let mut resp = MeshStateResponse::from(mesh_state);
    // Local-mode equivalent of the Attach-mode HTTP path: enrich the
    // status with the cached invite so the active-mesh view's share
    // card has something to render. Without this, in-process daemons
    // (the desktop's default) would never show the invite.
    if let Some((key, link)) = mesh.current_invite().await {
        resp.status.join_key = Some(key);
        resp.status.join_link = Some(link);
    }
    // Surface the client-API token beside the invite (None on a
    // loopback-only solo daemon).
    resp.client_token = mesh.running_client_token().await;
    Ok(Some(resp))
}

/// Cluster-health snapshot for the desktop "Shared model" chip + degraded
/// banner. Mirrors `mesh_get_state`'s dual reach: Attach reads the daemon's
/// `/v1/mesh/status` DTO; Local reads the in-process daemon + desktop config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SharedModelStatus {
    /// True when this node is in a shared-model fleet (a model id is set).
    pub configured: bool,
    pub model_id: Option<String>,
    /// Eligible anchors currently gossiped (online + `can_anchor`).
    pub eligible_anchors: usize,
    /// Quorum target — the host won't distribute below this many anchors.
    pub quorum_anchors: u32,
    /// Quorum met — a proxy for "the shared model is serveable". Below it the
    /// cluster is "forming" and consumers fall back to their local model.
    pub available: bool,
    /// This node currently holds the host role (assembles + distributes).
    pub is_host: bool,
}

#[tauri::command]
pub async fn get_shared_model_status(
    state: State<'_, Arc<AppState>>,
) -> Result<SharedModelStatus, String> {
    if let Some(port) = attached_port(&state) {
        // Attach — read the daemon's status DTO over HTTP.
        let client = http_client()?;
        let resp = client
            .get(format!("http://localhost:{port}/v1/mesh/status"))
            .send()
            .await
            .map_err(|e| format!("shared-model status: {e}"))?;
        if !resp.status().is_success() {
            return Ok(SharedModelStatus::default());
        }
        let remote: sovereign_mesh::mesh_http::StatusResponse = resp
            .json()
            .await
            .map_err(|e| format!("parse mesh/status: {e}"))?;
        return Ok(match remote.shared_model {
            Some(sm) => SharedModelStatus {
                configured: true,
                model_id: Some(sm.model_id),
                eligible_anchors: sm.eligible_anchors,
                quorum_anchors: sm.quorum_anchors,
                available: sm.available,
                is_host: remote.shared_model_host,
            },
            None => SharedModelStatus::default(),
        });
    }

    // Local — in-process daemon + desktop config. The eligible-anchor count is
    // read from gossiped membership (real regardless of who runs discovery);
    // the model id comes from this node's own config.
    let model_id = {
        let cfg = state.config.read().await;
        cfg.shared_model_id.clone().filter(|s| !s.is_empty())
    };
    let Some(model_id) = model_id else {
        return Ok(SharedModelStatus::default());
    };
    let eligible_anchors = match state.mesh.as_ref() {
        Some(mesh) => mesh.eligible_anchors().await.len(),
        None => 0,
    };
    let quorum_anchors = std::env::var("SOVEREIGN_RPC_QUORUM_ANCHORS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1);
    Ok(SharedModelStatus {
        configured: true,
        model_id: Some(model_id),
        eligible_anchors,
        quorum_anchors,
        available: eligible_anchors >= quorum_anchors as usize,
        is_host: sovereign_mesh::mesh_http::is_shared_model_host(),
    })
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateInviteResponse {
    pub mesh_name: String,
    pub join_key: String,
}

/// Rotate the active mesh's join key. Existing members stay
/// connected (they share the mesh state, not the key); only future
/// joins must use the new link. Refreshes the cached plaintext on
/// the daemon so the next status poll surfaces the new invite.
#[tauri::command]
pub async fn mesh_rotate_invite(
    state: State<'_, Arc<AppState>>,
) -> Result<RotateInviteResponse, String> {
    if let Some(port) = attached_port(&state) {
        let client = http_client()?;
        let resp = client
            .post(format!("http://localhost:{port}/v1/mesh/rotate"))
            .send()
            .await
            .map_err(|e| format!("mesh rotate: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("mesh rotate failed ({status}): {text}"));
        }
        return resp
            .json::<RotateInviteResponse>()
            .await
            .map_err(|e| format!("parse mesh/rotate response: {e}"));
    }

    // Local mode — talk to persist directly so we don't need a new
    // EmbeddedDaemon method just for this. Mirror what the HTTP
    // handler does: rotate on disk, then push the plaintext back
    // into the daemon so subsequent status polls see the new key.
    let Some(mesh) = state.mesh.as_ref() else {
        return Err("mesh daemon not available".into());
    };
    let data_dir = mesh.data_dir().to_path_buf();
    let rotated = sovereign_mesh::persist::rotate_join_key(&data_dir)
        .map_err(|e| format!("rotate failed: {e}"))?
        .ok_or_else(|| "no mesh to rotate".to_string())?;
    mesh.set_join_key(rotated.join_key.clone()).await;
    Ok(RotateInviteResponse {
        mesh_name: rotated.mesh_name,
        join_key: rotated.join_key,
    })
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
    // User clicked "Leave" — clear persisted mesh state. Distinct
    // from the daemon's graceful shutdown path which uses
    // `daemon.shutdown()` to PRESERVE state across restarts.
    mesh.leave().await.map_err(|e| e.to_string())
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

/// Snapshot of relay candidates (Tailscale / LAN / IPv6) the user
/// can append to a mesh invite as `?relay=<host:port>` for friends
/// who can't reach them via mDNS. Used by the invite-card relay
/// picker. Empty list = no detected interfaces (no network); the UI
/// hides the picker.
#[tauri::command]
pub async fn mesh_relay_candidates(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<RelayCandidate>, String> {
    if let Some(port) = attached_port(&state) {
        // Attach mode — the CLI daemon is the source of truth for
        // its own interfaces (it might be running in a container
        // or on a different binding than this desktop process).
        let client = http_client()?;
        let resp = client
            .get(format!("http://localhost:{port}/v1/mesh/relay-candidates"))
            .send()
            .await
            .map_err(|e| format!("relay-candidates: {e}"))?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        #[derive(serde::Deserialize)]
        struct Body {
            candidates: Vec<RelayCandidate>,
        }
        return Ok(resp
            .json::<Body>()
            .await
            .map(|b| b.candidates)
            .unwrap_or_default());
    }
    // Local mode — call the helper directly, no HTTP round-trip.
    Ok(sovereign_mesh::mesh_discovery::relay_candidates(9742))
}

/// Generate a fresh memorable two-word node-name suggestion (e.g.
/// "mac-peer"). Powers the 🎲 button next to the node-name input —
/// users click it to roll a new candidate, then press Save to
/// persist via the existing `save_config` flow.
///
/// This command is non-persisting on purpose: we don't want clicking
/// 🎲 to immediately mutate the user's config. The save still goes
/// through the existing audit point so DesktopConfig writes are
/// uniform.
#[tauri::command]
pub fn suggest_node_name() -> String {
    crate::friendly_names::generate(None)
}

/// Snapshot of mDNS-discovered peers and daemon health. Polled by
/// the MeshDiagnosticsPanel every few seconds.
#[tauri::command]
pub async fn mesh_diagnostics(state: State<'_, Arc<AppState>>) -> Result<MeshDiagnostics, String> {
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
    Ok(MeshDiagnostics {
        discovered_peers: peers,
        daemon_running,
    })
}

// ── Serializable wrappers for MeshState ──────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStateResponse {
    pub status: sovereign_mesh::MeshStatus,
    pub members: Vec<sovereign_mesh::MeshMember>,
    pub corpora: Vec<sovereign_mesh::MeshCorpus>,
    pub contribution: Option<sovereign_mesh::ContributionSummary>,
    /// Client-API bearer token for remote peers/clients, rendered on
    /// the invite screen beside `status.join_key`. `None` for a
    /// loopback-only (unshared) daemon. Populated from the running
    /// daemon (local mode) or `/v1/mesh/status` (attach mode).
    #[serde(default)]
    pub client_token: Option<String>,
}

impl From<MeshState> for MeshStateResponse {
    fn from(s: MeshState) -> Self {
        Self {
            status: s.status,
            members: s.members,
            corpora: s.corpora,
            contribution: s.contribution,
            // Enriched by the caller from the running daemon (the
            // `MeshState` value doesn't carry it).
            client_token: None,
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
                addresses: m.addresses,
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
                join_link: remote.join_link,
                join_key: remote.join_key,
            },
            members,
            corpora: Vec::new(),
            contribution: None,
            client_token: remote.client_token,
        }
    }
}

// ─── Mesh Health: dimensional contributions + peer preferences ──
//
// Surfaces the new contribution ledger and the operator-private
// per-peer affinity multiplier to the desktop UI. Local mode goes
// through the in-process `EmbeddedDaemon`'s AppState; Attach mode
// returns "not yet supported" — the daemon doesn't expose these
// over HTTP yet (TODO). The UI degrades to "no data" in Attach
// mode, which is honest about the gap and keeps the contract
// simple.

/// Dimensional contributions for one peer, shaped for the desktop
/// list. Mirrors `commonwealth_core::contributions::NodeContributions`
/// but flattened into a serde shape the frontend can consume
/// without depending on commonwealth-core's serde layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeContributionsDto {
    pub node_id: String,
    pub window_days: u32,
    pub inference_served_requests: u64,
    pub inference_served_tokens: u64,
    pub inference_served_wall_seconds: f64,
    pub inference_consumed_requests: u64,
    pub inference_consumed_tokens: u64,
    pub corpora_hosted: Vec<CorpusHostingDto>,
    pub bytes_served: u64,
    pub bytes_received: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusHostingDto {
    pub corpus_id: String,
    pub corpus_name: String,
    pub size_gb: f64,
    pub queries_served: u64,
    pub is_sole_host: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPreferenceDto {
    pub node_id: String,
    pub multiplier: f64,
    pub reason: Option<String>,
    pub set_at: u64,
}

/// Snapshot of every peer's dimensional contributions. In Attach
/// mode the daemon's `GET /internal/contribution/view` is the
/// source of truth (the daemon owns the MeshStore the
/// ContributionEmitter writes into). In Local mode the in-process
/// `AppState` is read directly.
#[tauri::command]
pub async fn mesh_get_contributions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<NodeContributionsDto>, String> {
    if attached_port(&state).is_some() {
        // Internal API is loopback-only on the fixed internal port —
        // matches the pinning used by every other `/internal/*` fetch
        // in this crate (see `DAEMON_INTERNAL_URL` in commands.rs).
        let client = http_client()?;
        let resp = client
            .get("http://127.0.0.1:9742/internal/contribution/view")
            .send()
            .await
            .map_err(|e| format!("GET /internal/contribution/view: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!(
                "daemon /internal/contribution/view returned {status}: {text}"
            ));
        }
        return resp
            .json::<Vec<NodeContributionsDto>>()
            .await
            .map_err(|e| format!("parse /internal/contribution/view response: {e}"));
    }
    let Some(mesh) = state.mesh.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(app_state) = mesh.app_state().await else {
        return Ok(Vec::new());
    };
    let store = app_state.inner.mesh_store.clone();
    let mesh_view = app_state.inner.mesh.read().await;
    let caps_map: std::collections::HashMap<_, _> = mesh_view
        .members
        .iter()
        .map(|(id, member)| (*id, member.capabilities.clone()))
        .collect();
    drop(mesh_view);
    let map = commonwealth_state::current_contributions(
        &store,
        &caps_map,
        commonwealth_core::contributions::DEFAULT_WINDOW_DAYS,
    )
    .map_err(|e| format!("read contributions: {e}"))?;
    let mut out: Vec<NodeContributionsDto> = map
        .into_iter()
        .map(|(node_id, c)| NodeContributionsDto {
            node_id: hex_node_id(&node_id),
            window_days: c.window_days,
            inference_served_requests: c.inference_served.requests,
            inference_served_tokens: c.inference_served.total_tokens_generated,
            inference_served_wall_seconds: c.inference_served.wall_seconds,
            inference_consumed_requests: c.inference_consumed.requests,
            inference_consumed_tokens: c.inference_consumed.total_tokens_generated,
            corpora_hosted: c
                .corpora_hosted
                .into_iter()
                .map(|h| CorpusHostingDto {
                    corpus_id: h.corpus_id,
                    corpus_name: h.corpus_name,
                    size_gb: h.size_gb,
                    queries_served: h.queries_served,
                    is_sole_host: h.is_sole_host,
                })
                .collect(),
            bytes_served: c.bytes_served,
            bytes_received: c.bytes_received,
        })
        .collect();
    out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Ok(out)
}

#[tauri::command]
pub async fn mesh_set_peer_preference(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    multiplier: f64,
    reason: Option<String>,
) -> Result<(), String> {
    if attached_port(&state).is_some() {
        return Err("peer preferences are not yet exposed over the daemon HTTP \
             API in Attach mode — set via `commonwealth peer-preference \
             set` instead"
            .into());
    }
    let Some(mesh) = state.mesh.as_ref() else {
        return Err("mesh daemon not available".into());
    };
    let Some(app_state) = mesh.app_state().await else {
        return Err("mesh daemon not running".into());
    };
    let target = parse_node_id_hex(&node_id)?;
    let pref =
        commonwealth_state::PeerPreference::new(multiplier, reason).map_err(|e| format!("{e}"))?;
    app_state
        .inner
        .peer_preferences
        .set(&target, pref)
        .map_err(|e| format!("{e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn mesh_clear_peer_preference(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<bool, String> {
    if attached_port(&state).is_some() {
        return Err("peer preferences are not yet exposed over the daemon HTTP \
             API in Attach mode — clear via `commonwealth peer-preference \
             clear` instead"
            .into());
    }
    let Some(mesh) = state.mesh.as_ref() else {
        return Err("mesh daemon not available".into());
    };
    let Some(app_state) = mesh.app_state().await else {
        return Err("mesh daemon not running".into());
    };
    let target = parse_node_id_hex(&node_id)?;
    app_state
        .inner
        .peer_preferences
        .clear(&target)
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn mesh_list_peer_preferences(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<PeerPreferenceDto>, String> {
    if attached_port(&state).is_some() {
        return Ok(Vec::new());
    }
    let Some(mesh) = state.mesh.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(app_state) = mesh.app_state().await else {
        return Ok(Vec::new());
    };
    let entries = app_state
        .inner
        .peer_preferences
        .list()
        .map_err(|e| format!("{e}"))?;
    Ok(entries
        .into_iter()
        .map(|(id, p)| PeerPreferenceDto {
            node_id: hex_node_id(&id),
            multiplier: p.multiplier(),
            reason: p.reason().map(|s| s.to_string()),
            set_at: p.set_at(),
        })
        .collect())
}

fn hex_node_id(id: &commonwealth_core::ids::NodeId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_node_id_hex(s: &str) -> Result<commonwealth_core::ids::NodeId, String> {
    if s.len() != 32 {
        return Err(format!("expected 32-hex-char node id, got '{s}'"));
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        let pair = s
            .get(i * 2..i * 2 + 2)
            .ok_or_else(|| format!("invalid hex id '{s}'"))?;
        *b = u8::from_str_radix(pair, 16).map_err(|_| format!("invalid hex id '{s}'"))?;
    }
    Ok(commonwealth_core::ids::NodeId::from_u128(
        u128::from_be_bytes(bytes),
    ))
}
