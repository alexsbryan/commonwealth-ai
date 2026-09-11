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
use sovereign_mesh::{parse_deep_link, JoinConfirmation};

use crate::bootstrap::BootstrapMode;
use crate::state::{resolve_node_name, AppState};

/// Did this boot reach a serving host?
///
/// Nothing in this file forks on the ANSWER to a mesh question any more
/// — every one of those goes over the wire (see [`mesh_client`]). The
/// two remaining callers ask whether there is anything to ask AT ALL:
/// a Local boot is one that reached no host, so its `/internal` port
/// has no listener and "empty" is a fact rather than a swallowed
/// failure.
fn attached(state: &AppState) -> bool {
    matches!(state.bootstrap_mode, BootstrapMode::Attach { .. })
}

/// The client for `/v1/mesh/*`, in BOTH boot modes.
///
/// Until svt-3 every command in this file forked: Attach hand-rolled a
/// `reqwest` call to `http://localhost:{port}/v1/mesh/…`, and Local
/// reached into an in-process `EmbeddedDaemon` for the same answer. Two
/// implementations of one question, free to drift in shape, in error
/// text and in which side-effects ran — and they had: `mesh_rotate_invite`
/// exposed the client API on the HTTP path and not on the Local one,
/// `mesh_join` accepted three invite forms over HTTP and only a deep
/// link in-process (ARCH principle 8).
///
/// There is one path now. A Local-mode daemon serves the same
/// `mesh_http` router on the same client port — `state.rs` REFUSES the
/// boot outright if that listener does not bind, so its presence is an
/// invariant here rather than a hope — and the port comes from
/// `client_base_url()`, the one accessor for it.
fn mesh_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
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
/// ONE path in both modes: `POST /v1/mesh/create`. The host is what
/// opts into serving remote peers as part of creating — the
/// `expose_client_api()` the Local arm used to perform here is the
/// route's own first statement, and a caller doing it beforehand was a
/// client deciding something about a daemon (ARCH principle 12).
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
    mesh_client(&state)
        .mesh_create(Some(&mesh_name), Some(&node_name), encrypt)
        .await
        .map_err(|e| format!("mesh_create: {e}"))
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
///
/// ONE path in both modes: `POST /v1/mesh/join`. Local mode gains the
/// two invite forms it never accepted — a bare `cwth-…` key and an
/// `https://…/join/…` URL — because the host's `parse_join_argument`
/// takes all three and the Local arm only ever tried `parse_deep_link`.
/// Two parsers for one input, and the narrower one was the default boot
/// mode's (ARCH principle 8).
#[tauri::command]
pub async fn mesh_join(
    state: State<'_, Arc<AppState>>,
    link: String,
) -> Result<JoinMeshResponse, String> {
    let node_name = {
        let config = state.config.read().await;
        resolve_node_name(&config.node_name)
    };
    mesh_client(&state)
        .mesh_join(&link, Some(&node_name))
        .await
        .map_err(|e| format!("mesh_join: {e}"))
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
    //
    // The port comes from `client_base_url()` — the ONE accessor for it
    // (ARCH principle 8). This used to spell `attached_port(…).unwrap_or(9741)`,
    // which was right only while a Local boot's port was 9741: a
    // `CliSetup` config naming any other client_port sent this read at a
    // dead port and the chip rendered empty.
    let base = state.client_base_url();
    let client = http_client()?;
    let resp = match client.get(format!("{base}/status")).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(target: "mesh_state", %base, status = %r.status(), "mesh_get_placement: /status non-2xx");
            return Ok(None);
        }
        Err(e) => {
            tracing::debug!(target: "mesh_state", %base, error = %e, "mesh_get_placement: /status unreachable");
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
    // The daemon's `/v1/mesh/status` returns a flat shape; we up-convert
    // it into the `MeshStateResponse` the frontend already renders.
    //
    // NOT A SECOND CHOICE. The Local arm this replaces read the
    // in-process `EmbeddedDaemon`, and `state.mesh` is permanently
    // `None` now that the app commissions no daemon of its own — so
    // that arm returned "Members empty" on every Local boot. The wire
    // answers. The four fields `MeshState` carried and `StatusResponse`
    // does not — `corpora`, `contribution`, `model_name`,
    // `knowledge_corpora` — were therefore ALREADY empty on that path;
    // `from_remote_status` records the gap for whoever grows the route.
    let base = state.client_base_url();
    let remote: sovereign_mesh::mesh_http::StatusResponse = mesh_client(&state)
        .mesh_status()
        .await
        .map_err(|e| format!("mesh_get_state: {e}"))?;
    if remote.mesh_name.is_none() {
        tracing::warn!(
            target: "mesh_state",
            %base,
            "mesh_get_state: daemon reports no active mesh — Members empty"
        );
        return Ok(None);
    }
    tracing::info!(
        target: "mesh_state",
        %base,
        members = remote.members.len(),
        members_online = remote.members_online,
        "mesh_get_state: fetched mesh status"
    );
    Ok(Some(MeshStateResponse::from_remote_status(remote)?))
}

/// Check if the mesh daemon is currently running.
///
/// Always `true`, and honestly so: this process reached a serving host
/// at startup or it would not have got here. It USED to ask an
/// in-process `EmbeddedDaemon` first and fall through to `true` — and
/// since that daemon stopped existing the fall-through was the whole
/// function, with a `state.mesh()` call in front of it saying otherwise.
#[tauri::command]
pub async fn mesh_is_running(_state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(true)
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
///
/// ONE path in both modes: `POST /v1/mesh/rotate`. The Local arm used
/// to call `rotate_invite` in-process and stop there, while the route
/// ALSO exposes the client API — rotation exists in order to share, so
/// a soloist rotating to invite someone gets a reachable node. That was
/// the third partial job in this command's history: it first wrote the
/// key straight to disk (leaving the live mesh to re-persist the old
/// hash within a gossip round), then drove the daemon but skipped the
/// exposure. One implementation now, and it is the host's.
#[tauri::command]
pub async fn mesh_rotate_invite(
    state: State<'_, Arc<AppState>>,
) -> Result<RotateInviteResponse, String> {
    mesh_client(&state)
        .mesh_rotate(false)
        .await
        .map_err(|e| format!("mesh_rotate_invite: {e}"))
}

/// One membership in the mesh switcher's list — the route's own type.
/// `mesh_list` parses `/v1/mesh/status`'s rows into it in Attach mode and
/// builds it in Local mode; both ends deserve one definition (ARCH §10.6).
pub use sovereign_mesh::mesh_http::KnownMeshDto;

/// Every mesh this node has joined — active and parked.
///
/// ONE path in both modes: the `meshes[]` the daemon already carries on
/// `/v1/mesh/status`, rather than a second endpoint. The Local arm used
/// to BUILD these rows itself out of `known_meshes()` plus a direct
/// `persist::active_mesh_id` read of the data dir — the same five fields
/// derived a second way, off a file the daemon owns and writes (ARCH
/// principle 12).
#[tauri::command]
pub async fn mesh_list(state: State<'_, Arc<AppState>>) -> Result<Vec<KnownMeshDto>, String> {
    let status: sovereign_mesh::mesh_http::StatusResponse = mesh_client(&state)
        .mesh_status()
        .await
        .map_err(|e| format!("mesh_list: {e}"))?;
    Ok(status.meshes)
}

/// Park the active mesh and bring another joined mesh up.
///
/// The daemon rebinds `:9741` as part of this, so the caller must poll
/// through the bounce — `MeshSettings` reuses the same `reconnecting` banner
/// and `waitForDaemonAndRefresh` helper that Leave already uses.
///
/// ONE path in both modes: `POST /v1/mesh/switch`. Local mode gains the
/// route's resolve-before-detach rule — an unknown name comes back a 404
/// the caller can read, instead of a teardown followed by silence.
#[tauri::command]
pub async fn mesh_switch(state: State<'_, Arc<AppState>>, mesh: String) -> Result<(), String> {
    mesh_client(&state)
        .mesh_switch(&mesh)
        .await
        .map_err(|e| format!("mesh_switch: {e}"))
}

/// Drop a PARKED mesh from this node. Refuses on the active one.
///
/// `POST /v1/mesh/forget`, WRITTEN for this rung. The button worked in
/// neither mode before it: Attach refused with "not yet exposed over
/// HTTP — run `svrn mesh forget` instead", and the Local arm behind
/// that refusal asked an in-process daemon this app no longer
/// commissions. An honest refusal that nothing could satisfy is still a
/// missing feature; the route is the fix (ARCH principle 6).
#[tauri::command]
pub async fn mesh_forget(state: State<'_, Arc<AppState>>, mesh: String) -> Result<(), String> {
    mesh_client(&state)
        .mesh_forget(&mesh)
        .await
        .map_err(|e| format!("mesh_forget: {e}"))
}

/// Leave the current mesh and return the node to a fresh solo mesh.
///
/// ONE path in both modes: `POST /v1/mesh/leave`. The host ACKs and
/// re-solos in a detached task — it is served BY the listener that
/// leaving drops, so an inline teardown would cancel its own response —
/// and the caller polls back through the bounce with the same
/// `reconnecting` banner Switch uses. Which of `leave()`,
/// `leave_to_solo()` and `shutdown()` a Leave means is the daemon's
/// choice about its own lifetime, and this client no longer holds an
/// opinion on it (ARCH principle 12).
#[tauri::command]
pub async fn mesh_leave(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    mesh_client(&state)
        .mesh_leave()
        .await
        .map_err(|e| format!("mesh_leave: {e}"))
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
///
/// ONE path in both modes: `GET /v1/mesh/relay-candidates`. The HOST
/// answers because the addresses are its own — it may be in a container
/// or bound differently from whatever asked — and the internal port it
/// advertises them on is its to know. The Local arm used to call
/// `mesh_discovery::relay_candidates(9742)` here with that port spelled
/// out beside the daemon's own copy of it (ARCH principle 8).
///
/// A failed read is now an `Err` rather than an empty list. The Attach
/// arm swallowed both a non-2xx and a parse failure into `Vec::new()`,
/// which renders identically to "this machine has no reachable
/// interface" — two facts with opposite remedies (ARCH principle 6).
/// An empty list still means no detected interface, and the picker
/// hides on it.
#[tauri::command]
pub async fn mesh_relay_candidates(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<RelayCandidate>, String> {
    mesh_client(&state)
        .mesh_relay_candidates()
        .await
        .map_err(|e| format!("mesh_relay_candidates: {e}"))
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
    // The DAEMON owns mDNS discovery, in every mode — it always did in
    // Attach, and the in-process arm that read `discovered_peers()`
    // directly is gone with the daemon it read. No route carries the
    // table yet, so this panel is EMPTY rather than wrong, and the gap
    // is named here rather than papered over: `/v1/mesh/status` grows a
    // `discovered[]` and this reads it (task #37).
    Ok(MeshDiagnostics {
        discovered_peers: Vec::new(),
        daemon_running: true,
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
    let attached = attached(&state);
    if !attached {
        // A Local boot is a boot that reached no serving host, so there
        // is no `/internal` listener to ask and the ledger is empty —
        // the same answer this gave before, reached without a
        // `state.mesh()` that is now permanently `None`. A host that IS
        // listening and refuses still arrives as an `Err`.
        tracing::debug!(
            target: "mesh_state",
            "mesh_get_contributions: no serving host for this boot \
             — empty ledger, not a failed read"
        );
        return Ok(Vec::new());
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
    let attached = attached(&state);
    if !attached {
        tracing::debug!(
            target: "mesh_state",
            "mesh_list_peer_preferences: no serving host for this boot \
             — no preferences, not a failed read"
        );
        return Ok(Vec::new());
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
    /// The one way this migration could regress silently: a boot that
    /// reached no serving host answers `Ok(vec![])`, and that arm is a
    /// `bootstrap_mode` check rather than an in-process read. If a
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
