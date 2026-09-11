// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for mesh operations.
//!
//! These are called from the svrnmesh frontend (the Tauri webview) when
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
        BootstrapMode::Attach { client_port, .. } => Some(*client_port),
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

    let Some(mesh) = state.mesh().await else {
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
    let Some(mesh) = state.mesh().await else {
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

/// Shared-model placement for UI display (P0.6): the primary slot's
/// placement object from the daemon's `/status` — `mode`
/// (distributed/local), block split, per-worker endpoints — plus the
/// model id and whether this node serves an RPC worker. Opaque JSON on
/// purpose: the daemon's `/status` owns the schema; the panel renders
/// what's there so a daemon upgrade never breaks a stale desktop.
/// `null` when the daemon is unreachable or nothing is resident yet —
/// the panel then shows nothing rather than a broken chip.
#[tauri::command]
pub async fn mesh_get_placement(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<serde_json::Value>, String> {
    // Both modes read over HTTP: the local (embedded) daemon serves the
    // same `/status` on its client port, so one path covers both.
    let port = attached_port(&state).unwrap_or(9741);
    let client = http_client()?;
    let resp = match client
        .get(format!("http://localhost:{port}/status"))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(target: "mesh_state", port, status = %r.status(), "mesh_get_placement: /status non-2xx");
            return Ok(None);
        }
        Err(e) => {
            tracing::debug!(target: "mesh_state", port, error = %e, "mesh_get_placement: /status unreachable");
            return Ok(None);
        }
    };
    let status: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse /status: {e}"))?;
    let primary = status
        .get("inference")
        .and_then(|i| i.get("resident"))
        .and_then(|r| r.as_array())
        .and_then(|slots| {
            slots
                .iter()
                .find(|s| s.get("role").and_then(|v| v.as_str()) == Some("primary"))
        });
    let Some(primary) = primary else {
        return Ok(None);
    };
    Ok(Some(serde_json::json!({
        "model_id": primary.get("model_id"),
        "resident": primary.get("resident"),
        "transitioning": primary.get("transitioning"),
        "placement": primary.get("placement"),
        "rpc_worker": status.get("rpc_worker"),
    })))
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
        return Ok(Some(MeshStateResponse::from_remote_status(remote)?));
    }

    let Some(mesh) = state.mesh().await else {
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
    // Track W: the founder's own reachability, for the "Reachable /
    // Reconnecting" indicator (MeshState doesn't carry it).
    resp.status.self_reachability = mesh.self_reachability().await;
    Ok(Some(resp))
}

/// Check if the mesh daemon is currently running. In Attach mode we
/// always report `true` — the CLI daemon is by definition running or
/// we wouldn't have detected Attach in the first place.
#[tauri::command]
pub async fn mesh_is_running(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    match state.mesh().await {
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

    // Local mode — go through the daemon, not around it.
    //
    // This used to call `persist::rotate_join_key` directly, on the reasoning
    // that a disk write needed no daemon method. It did: rotation has to
    // change the LIVE mesh or the gossip loop re-persists the old hash over
    // the new one within a round, and this path skipped that just as the CLI
    // and HTTP paths did. Three callers, three different partial jobs — the
    // §10.6 duplicated-decider failure. There is now one implementation.
    let Some(mesh) = state.mesh().await else {
        return Err("mesh daemon not available".into());
    };
    let rotated = mesh
        .rotate_invite(false)
        .await
        .map_err(|e| format!("rotate failed: {e}"))?;
    Ok(RotateInviteResponse {
        mesh_name: rotated.mesh_name,
        join_key: rotated.join_key,
    })
}

/// One membership in the mesh switcher's list — the route's own type.
/// `mesh_list` parses `/v1/mesh/status`'s rows into it in Attach mode and
/// builds it in Local mode; both ends deserve one definition (ARCH §10.6).
pub use sovereign_mesh::mesh_http::KnownMeshDto;

/// Every mesh this node has joined — active and parked.
#[tauri::command]
pub async fn mesh_list(state: State<'_, Arc<AppState>>) -> Result<Vec<KnownMeshDto>, String> {
    if let Some(port) = attached_port(&state) {
        // Attach mode reads it off the status payload the daemon already
        // serves, rather than a second endpoint.
        let client = http_client()?;
        let resp = client
            .get(format!("http://localhost:{port}/v1/mesh/status"))
            .send()
            .await
            .map_err(|e| format!("mesh list: {e}"))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("parse mesh/status: {e}"))?;
        let rows = body.get("meshes").cloned().unwrap_or(serde_json::json!([]));
        return serde_json::from_value(rows).map_err(|e| format!("parse meshes: {e}"));
    }
    let Some(mesh) = state.mesh().await else {
        return Ok(Vec::new());
    };
    let active = sovereign_mesh::persist::active_mesh_id(mesh.data_dir());
    Ok(mesh
        .known_meshes()
        .into_iter()
        .map(|m| KnownMeshDto {
            is_active: active.as_ref() == Some(&m.mesh_id),
            mesh_id: m.mesh_id.to_hex(),
            members_total: m.members.len(),
            last_seen_unix: m.members.iter().map(|r| r.last_seen).max().unwrap_or(0),
            name: m.name,
        })
        .collect())
}

/// Park the active mesh and bring another joined mesh up.
///
/// The daemon rebinds `:9741` as part of this, so the caller must poll
/// through the bounce — `MeshSettings` reuses the same `reconnecting` banner
/// and `waitForDaemonAndRefresh` helper that Leave already uses.
#[tauri::command]
pub async fn mesh_switch(state: State<'_, Arc<AppState>>, mesh: String) -> Result<(), String> {
    if let Some(port) = attached_port(&state) {
        let client = http_client()?;
        let resp = client
            .post(format!("http://localhost:{port}/v1/mesh/switch"))
            .json(&serde_json::json!({ "mesh": mesh }))
            .send()
            .await
            .map_err(|e| format!("mesh switch: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("mesh switch failed ({status}): {text}"));
        }
        return Ok(());
    }
    let Some(daemon) = state.mesh().await else {
        return Err("mesh daemon not available".into());
    };
    daemon
        .switch_mesh(&mesh)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Drop a PARKED mesh from this node. Refuses on the active one.
#[tauri::command]
pub async fn mesh_forget(state: State<'_, Arc<AppState>>, mesh: String) -> Result<(), String> {
    if attached_port(&state).is_some() {
        return Err(
            "forgetting a mesh is not yet exposed over HTTP — run `svrn mesh forget` instead"
                .into(),
        );
    }
    let Some(daemon) = state.mesh().await else {
        return Err("mesh daemon not available".into());
    };
    daemon
        .forget_mesh(&mesh)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Leave the current mesh and return the node to a fresh solo mesh.
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
    let Some(mesh) = state.mesh().await else {
        return Err("mesh daemon not available".into());
    };
    // User clicked "Leave" — leave the current mesh AND re-create a fresh
    // solo mesh in-process (rebinding the client API) so the embedded
    // daemon stays reachable, exactly like the Attach-mode HTTP path.
    // Uses `leave_to_solo`, NOT the bare `leave()` (which only tears down
    // and is reserved for `join_mesh`'s mesh-switch auto-leave), and NOT
    // `daemon.shutdown()` (the graceful process-exit path that PRESERVES
    // state).
    mesh.leave_to_solo().await.map_err(|e| e.to_string())
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
    let (peers, daemon_running) = match state.mesh().await {
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
    ///
    /// Member status strings are parsed by the enum's OWN serde repr
    /// (`MemberStatus` is `rename_all = "lowercase"` in sovereign-mesh
    /// types) — one decider for the string set. Until 2026-09-09 this
    /// was a hand match with `_ => Offline`, so a daemon newer than the
    /// desktop (a new status variant) silently rendered its members as
    /// offline instead of surfacing the unknown string — the §18.3
    /// substitution. The parse now refuses (sv-surface rung 4).
    pub fn from_remote_status(
        remote: sovereign_mesh::mesh_http::StatusResponse,
    ) -> Result<Self, String> {
        use serde::de::IntoDeserializer;
        use sovereign_mesh::{MemberStatus, MeshMember, MeshStatus};
        let members: Vec<MeshMember> = remote
            .members
            .into_iter()
            .map(|m| {
                Ok(MeshMember {
                    name: m.name,
                    node_id: m.node_id,
                    is_self: m.is_self,
                    status: MemberStatus::deserialize(m.status.as_str().into_deserializer())
                        .map_err(|e: serde::de::value::Error| {
                            format!("mesh member status {:?}: {e}", m.status)
                        })?,
                    vram_gb: m.vram_gb,
                    can_anchor: m.can_anchor,
                    contribution_level: 0,
                    contribution_label: String::new(),
                    addresses: m.addresses,
                    origins: m.origins,
                    node_pubkey: m.node_pubkey,
                    active: m.active,
                    hw_fingerprint: m.hw_fingerprint,
                    backend: m.backend,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            status: MeshStatus {
                name: remote.mesh_name.unwrap_or_default(),
                members_online: remote.members_online,
                members_total: remote.members_total,
                model_name: None,
                knowledge_corpora: Vec::new(),
                is_connected: remote.running,
                join_link: remote.join_link,
                join_key: remote.join_key,
                self_reachability: remote.self_reachability,
            },
            members,
            corpora: Vec::new(),
            contribution: None,
            client_token: remote.client_token,
        })
    }
}

// ─── Mesh Health: dimensional contributions + peer preferences ──
//
// Surfaces the contribution ledger and the operator-private per-peer
// affinity multiplier to the desktop UI. The two halves are at
// different rungs, and the comment that used to sit here claimed one
// story for both:
//
// - CONTRIBUTIONS cross. `GET /internal/contribution/view` serves the
//   whole answer and BOTH modes go through it (svt-3). There is no
//   in-process arm left.
// - PEER PREFERENCES do not, yet. No daemon route exposes them, so
//   Attach mode REFUSES with the CLI command that does the job rather
//   than quietly doing it against the wrong process (ARCH principle
//   6). That refusal is the remaining work, not the design.

/// Dimensional contributions for one peer, shaped for the desktop
/// list — and the parse shape for `/internal/contribution/view`,
/// whose `NodeContributionsView` carries these same field names.
///
/// Flattened out of `commonwealth_core::contributions::NodeContributions`
/// so the frontend does not depend on that crate's serde layout, and
/// so this stays the ONE shape on both sides of the socket.
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

/// Snapshot of every peer's dimensional contributions.
///
/// ONE path in both modes: the daemon's `GET
/// /internal/contribution/view`, which owns the `MeshStore` the
/// `ContributionEmitter` writes into and does the aggregation
/// (`commonwealth-api/src/routes_internal/mesh_admin.rs`). Until
/// svt-3 Attach mode hand-rolled a `reqwest` call to that route while
/// Local mode ran `commonwealth_state::current_contributions`
/// in-process — two implementations of one answer, free to drift
/// apart in window, shape and order (ARCH principle 8). The
/// in-process daemon binds the SAME `/internal` router beside its
/// client listener (`sovereign_mesh::daemon::start_daemon`), so Local
/// mode reaches that one handler over loopback.
///
/// Local mode still answers an EMPTY list, not an error, when no mesh
/// daemon is running: there is no listener to ask, and "no mesh yet"
/// is a fact the Members panel renders. It is read from the daemon's
/// own state, never inferred from a refused connection — an
/// unreachable host that IS running stays an `Err` (ARCH principle 6).
#[tauri::command]
pub async fn mesh_get_contributions(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<NodeContributionsDto>, String> {
    let attached = attached_port(&state).is_some();
    if !attached {
        // Local mode: the in-process daemon binds `/internal` only
        // once it is Running (no mesh created or joined = no
        // listener). Ask only when there is something listening.
        match state.mesh().await {
            Some(mesh) if mesh.app_state().await.is_some() => {}
            _ => {
                tracing::debug!(
                    target: "mesh_state",
                    "mesh_get_contributions: local mode with no running mesh daemon \
                     — empty ledger, not a failed read"
                );
                return Ok(Vec::new());
            }
        }
    }
    tracing::debug!(
        target: "mesh_state",
        attached,
        base = %state.internal_base_url(),
        "mesh_get_contributions: asking the daemon for the contribution view"
    );
    // Internal API is loopback-only on the daemon's internal port —
    // resolved via `state.internal_base_url()`, matching every other
    // `/internal/*` fetch in this crate.
    sovereign_turn_client::TurnClient::new(state.internal_base_url())
        .contribution_view::<Vec<NodeContributionsDto>>()
        .await
        .map_err(|e| format!("mesh_get_contributions: {e}"))
}

/// Set or replace one peer's affinity multiplier.
///
/// ONE path in both modes: `POST /internal/peer-preference/set`. Until
/// svt-3 Attach mode REFUSED here, naming the CLI, because no route
/// existed — an honest refusal (ARCH principle 6) that cost two crates,
/// `commonwealth-core` and `commonwealth-state`, a place in this
/// client's manifest so the Local arm could hold a `NodeId` and a
/// `PeerPreference`. Both are the daemon's to hold: it owns the
/// `MeshStore` the preference lands in and is the only reader of it
/// (`commonwealth-api/src/routes_oicp.rs:383`).
///
/// The `(0.0, 1.0]` clamp and the 32-hex-char id precondition are BOTH
/// the host's now, not restated here (ARCH principle 8).
#[tauri::command]
pub async fn mesh_set_peer_preference(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    multiplier: f64,
    reason: Option<String>,
) -> Result<(), String> {
    tracing::debug!(
        target: "mesh_state",
        base = %state.internal_base_url(),
        multiplier,
        "mesh_set_peer_preference: asking the daemon to record the preference"
    );
    sovereign_turn_client::TurnClient::new(state.internal_base_url())
        .set_peer_preference(&node_id, multiplier, reason.as_deref())
        .await
        .map_err(|e| format!("mesh_set_peer_preference: {e}"))
}

/// Drop one peer's affinity preference, answering whether one was set.
///
/// Same one path in both modes as [`mesh_set_peer_preference`]. The bool
/// is the host's own idempotency answer, carried through rather than
/// inferred from a status code.
#[tauri::command]
pub async fn mesh_clear_peer_preference(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<bool, String> {
    tracing::debug!(
        target: "mesh_state",
        base = %state.internal_base_url(),
        "mesh_clear_peer_preference: asking the daemon to drop the preference"
    );
    sovereign_turn_client::TurnClient::new(state.internal_base_url())
        .clear_peer_preference(&node_id)
        .await
        .map_err(|e| format!("mesh_clear_peer_preference: {e}"))
}

/// Every affinity preference the operator has set, for the Mesh Health
/// panel.
///
/// ONE path in both modes: `GET /internal/peer-preference/list`. Until
/// svt-3 the Attach arm returned an EMPTY LIST — not an error — for a
/// question it had no way to ask, which is the shape ARCH principle 6
/// exists to forbid: "the operator has set no preferences" and "this
/// client cannot see them" rendered identically in the panel.
///
/// Local mode keeps the one substitution that IS honest, and it is the
/// same one `mesh_get_contributions` makes: with no mesh daemon running
/// there is no `/internal` listener to ask, and "no mesh yet" is a fact
/// the panel renders. A host that IS listening and refuses stays an
/// `Err`.
#[tauri::command]
pub async fn mesh_list_peer_preferences(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<PeerPreferenceDto>, String> {
    let attached = attached_port(&state).is_some();
    if !attached {
        match state.mesh().await {
            Some(mesh) if mesh.app_state().await.is_some() => {}
            _ => {
                tracing::debug!(
                    target: "mesh_state",
                    "mesh_list_peer_preferences: local mode with no running mesh \
                     daemon — no preferences, not a failed read"
                );
                return Ok(Vec::new());
            }
        }
    }
    tracing::debug!(
        target: "mesh_state",
        attached,
        base = %state.internal_base_url(),
        "mesh_list_peer_preferences: asking the daemon for the preference list"
    );
    sovereign_turn_client::TurnClient::new(state.internal_base_url())
        .peer_preferences::<PeerPreferenceDto>()
        .await
        .map_err(|e| format!("mesh_list_peer_preferences: {e}"))
}

#[cfg(test)]
mod contribution_view_tests {
    use super::*;
    use std::sync::Mutex;

    /// Exactly what `commonwealth_api::routes_internal::mesh_admin::
    /// contribution_view` serialises: `Vec<NodeContributionsView>`,
    /// plain field names, no serde renames, already sorted by node id
    /// (the handler's own `out.sort_by` is the last thing it does).
    ///
    /// Every one of the nine scalars and five nested fields carries a
    /// DISTINCT non-zero value, so a field wired to the wrong
    /// neighbour fails instead of matching by coincidence. Two nodes,
    /// because a one-row fixture cannot show order surviving the hop.
    const DAEMON_VIEW_JSON: &str = r#"[
      {
        "node_id": "0a0b0c0d0e0f101112131415161718aa",
        "window_days": 30,
        "inference_served_requests": 11,
        "inference_served_tokens": 22,
        "inference_served_wall_seconds": 33.5,
        "inference_consumed_requests": 44,
        "inference_consumed_tokens": 55,
        "corpora_hosted": [
          {"corpus_id":"sep","corpus_name":"Stanford Encyclopedia",
           "size_gb":6.25,"queries_served":77,"is_sole_host":true},
          {"corpus_id":"gutenberg","corpus_name":"Project Gutenberg",
           "size_gb":8.5,"queries_served":88,"is_sole_host":false}
        ],
        "bytes_served": 99,
        "bytes_received": 100
      },
      {
        "node_id": "ff0b0c0d0e0f101112131415161718bb",
        "window_days": 30,
        "inference_served_requests": 1,
        "inference_served_tokens": 2,
        "inference_served_wall_seconds": 3.0,
        "inference_consumed_requests": 4,
        "inference_consumed_tokens": 5,
        "corpora_hosted": [],
        "bytes_served": 6,
        "bytes_received": 7
      }
    ]"#;

    /// NO REGRESSION, field for field.
    ///
    /// Before svt-3 the Local arm built these DTOs in-process from
    /// `commonwealth_state::current_contributions` and the Attach arm
    /// parsed them from this route. Both arms now parse this route, so
    /// what used to be a mapping bug becomes a PARSE bug — and this is
    /// where it lands. `NodeContributionsView` has no serde renames
    /// (`mesh_admin.rs:1444-1465`), so a field renamed on either side
    /// blanks the Members ledger; the comment above that struct says
    /// exactly that, and until now nothing enforced it.
    #[test]
    fn the_daemon_view_parses_into_the_dto_field_for_field() {
        let got: Vec<NodeContributionsDto> =
            serde_json::from_str(DAEMON_VIEW_JSON).expect("the daemon's own shape parses");
        assert_eq!(got.len(), 2);

        let a = &got[0];
        assert_eq!(a.node_id, "0a0b0c0d0e0f101112131415161718aa");
        assert_eq!(a.window_days, 30, "the 30-day default window survives");
        assert_eq!(a.inference_served_requests, 11);
        assert_eq!(a.inference_served_tokens, 22);
        assert_eq!(a.inference_served_wall_seconds, 33.5, "an f64, not rounded");
        assert_eq!(a.inference_consumed_requests, 44);
        assert_eq!(a.inference_consumed_tokens, 55);
        assert_eq!(a.bytes_served, 99);
        assert_eq!(a.bytes_received, 100);

        assert_eq!(a.corpora_hosted.len(), 2, "nested rows are not flattened");
        let sep = &a.corpora_hosted[0];
        assert_eq!(sep.corpus_id, "sep");
        assert_eq!(sep.corpus_name, "Stanford Encyclopedia");
        assert_eq!(sep.size_gb, 6.25);
        assert_eq!(sep.queries_served, 77);
        assert!(sep.is_sole_host, "the sole-host flag is not defaulted");
        assert!(!a.corpora_hosted[1].is_sole_host);

        let b = &got[1];
        assert_eq!(b.node_id, "ff0b0c0d0e0f101112131415161718bb");
        assert!(
            b.corpora_hosted.is_empty(),
            "a peer hosting nothing is an empty list, not a missing key"
        );
        assert_eq!(b.inference_served_wall_seconds, 3.0);
    }

    /// THE WIRE HOP — the whole of what svt-3 changed.
    ///
    /// Drives the exact expression `mesh_get_contributions` now runs
    /// in BOTH modes: `TurnClient::new(internal_base_url)
    /// .contribution_view::<Vec<NodeContributionsDto>>()`. Asserts the
    /// host saw `/internal/contribution/view` — the same path the
    /// deleted hand-rolled `reqwest` call built by hand — and that the
    /// answer arrives with its values and its ORDER intact.
    ///
    /// Order is the host's answer, not the client's: the handler sorts
    /// by node id and the desktop's `out.sort_by` went with the local
    /// arm (ARCH principle 8). So the fixture is served in the host's
    /// order and must come back in it.
    #[tokio::test]
    async fn the_wire_hop_reaches_the_route_and_loses_nothing() {
        use axum::{routing::get, Router};

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);

        let app = Router::new()
            .route(
                "/internal/contribution/view",
                get(move || {
                    let recorder = Arc::clone(&recorder);
                    async move {
                        recorder
                            .lock()
                            .unwrap()
                            .push("/internal/contribution/view".to_string());
                        (
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            DAEMON_VIEW_JSON,
                        )
                    }
                }),
            )
            // Anything else is a 404 the client must report as an
            // error, so a client that drifts onto another path fails
            // loudly here rather than returning an empty ledger.
            .fallback(|| async { axum::http::StatusCode::NOT_FOUND });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let got: Vec<NodeContributionsDto> = sovereign_turn_client::TurnClient::new(base)
            .contribution_view()
            .await
            .expect("the daemon's contribution view is readable over the wire");

        assert_eq!(
            *seen.lock().unwrap(),
            ["/internal/contribution/view"],
            "the client asks the route the daemon actually registers \
             (commonwealth-api/src/server.rs:516)"
        );
        assert_eq!(
            got.iter().map(|c| c.node_id.as_str()).collect::<Vec<_>>(),
            [
                "0a0b0c0d0e0f101112131415161718aa",
                "ff0b0c0d0e0f101112131415161718bb"
            ],
            "the host's order arrives unchanged — the client does not re-sort"
        );
        assert_eq!(got[0].inference_served_wall_seconds, 33.5);
        assert_eq!(got[0].corpora_hosted.len(), 2);
        assert_eq!(got[0].corpora_hosted[1].corpus_id, "gutenberg");
        assert_eq!(got[1].bytes_received, 7);
    }

    /// A host that REFUSES is an error, never an empty ledger.
    ///
    /// The one way this migration could regress silently: Local mode
    /// answers `Ok(vec![])` for "no mesh daemon yet", and that arm is
    /// now a `state.mesh()` check rather than an in-process read. If a
    /// failed HTTP call could also produce an empty vec, "the mesh has
    /// served nothing" and "the daemon would not answer" would render
    /// identically and the operator would have no way to tell
    /// (ARCH principle 6). They must not collapse.
    #[tokio::test]
    async fn a_refusing_host_is_an_error_not_an_empty_ledger() {
        use axum::{routing::get, Router};

        let app = Router::new().route(
            "/internal/contribution/view",
            get(|| async {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "contribution_view: aggregate failed: store closed",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let err = sovereign_turn_client::TurnClient::new(base)
            .contribution_view::<Vec<NodeContributionsDto>>()
            .await
            .expect_err("a 500 is a refusal, not an empty ledger");
        let text = err.to_string();
        assert!(
            text.contains("store closed"),
            "the host's own words reach the operator; got {text}"
        );
    }

    /// The FRONTEND contract. `mesh_get_contributions` serialises this
    /// DTO back over the Tauri bridge, and the Members panel reads
    /// these key names. The migration changed where the values come
    /// from and must not have changed a single key.
    #[test]
    fn the_dto_reserialises_with_the_keys_the_members_panel_reads() {
        let got: Vec<NodeContributionsDto> = serde_json::from_str(DAEMON_VIEW_JSON).unwrap();
        let wire = serde_json::to_value(&got).unwrap();
        let row = &wire[0];
        for key in [
            "node_id",
            "window_days",
            "inference_served_requests",
            "inference_served_tokens",
            "inference_served_wall_seconds",
            "inference_consumed_requests",
            "inference_consumed_tokens",
            "corpora_hosted",
            "bytes_served",
            "bytes_received",
        ] {
            assert!(!row[key].is_null(), "the frontend reads `{key}`");
        }
        for key in [
            "corpus_id",
            "corpus_name",
            "size_gb",
            "queries_served",
            "is_sole_host",
        ] {
            assert!(
                !row["corpora_hosted"][0][key].is_null(),
                "the frontend reads `corpora_hosted[].{key}`"
            );
        }
        assert_eq!(
            row.as_object().unwrap().len(),
            10,
            "no key added or dropped"
        );
    }

    /// The peer-preference half of the same contract (svt-3).
    ///
    /// Exactly what `commonwealth_api::routes_internal::peer_preference::
    /// peer_preference_list` serialises: `Vec<PeerPreferenceView>`, plain
    /// field names, no serde renames. Before svt-3 the Local arm built these
    /// DTOs in-process from `commonwealth_state::PeerPreferenceStore::list`
    /// and the Attach arm returned an empty list; both arms now parse this
    /// route, so what used to be a mapping bug becomes a PARSE bug — and
    /// this is where it lands.
    ///
    /// Distinct non-zero values per field, and a second row whose `reason`
    /// is absent, because `Option<String>` is the one field a wrong serde
    /// attribute can blank without failing.
    #[test]
    fn the_daemon_preference_view_parses_into_the_dto_field_for_field() {
        const DAEMON_PREFS_JSON: &str = r#"[
          {
            "node_id": "0a0b0c0d0e0f101112131415161718aa",
            "multiplier": 0.25,
            "reason": "throttled while it backfills",
            "set_at": 1757000000
          },
          {
            "node_id": "ff0b0c0d0e0f101112131415161718bb",
            "multiplier": 1.0,
            "reason": null,
            "set_at": 1757000001
          }
        ]"#;

        let got: Vec<PeerPreferenceDto> =
            serde_json::from_str(DAEMON_PREFS_JSON).expect("the daemon's own shape parses");
        assert_eq!(got.len(), 2);

        assert_eq!(got[0].node_id, "0a0b0c0d0e0f101112131415161718aa");
        assert_eq!(got[0].multiplier, 0.25, "an f64, not rounded");
        assert_eq!(
            got[0].reason.as_deref(),
            Some("throttled while it backfills")
        );
        assert_eq!(got[0].set_at, 1_757_000_000);

        assert_eq!(got[1].node_id, "ff0b0c0d0e0f101112131415161718bb");
        assert_eq!(got[1].multiplier, 1.0, "the top of the clamp survives");
        assert_eq!(got[1].reason, None, "an absent note stays absent");
        assert_eq!(got[1].set_at, 1_757_000_001);
    }
}
