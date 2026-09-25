// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands for mesh operations.
//!
//! These are called from the svrnmesh frontend (the Tauri webview) when
//! users interact with the Community Mesh section of the settings UI.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use sovereign_contracts::daemon_wire::{
    JoinConfirmation, KnownMeshDto, MemberStatus, MeshMember, MeshStatus, MeshStatusSummary,
    RelayCandidate,
};

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

/// Preview an invite and return the join confirmation info.
/// Called when the user taps a `sovereign://join/...` link but before they
/// confirm — gives the UI info to render the confirmation dialog.
///
/// ONE parser, and it is the host's: `POST /v1/mesh/join/preview` runs the
/// same `parse_join_argument` that `POST /v1/mesh/join` runs, so this
/// preview accepts exactly what the join will (bare key, https URL, deep
/// link). Until svt-3 this ran `parse_deep_link` in-process — the
/// narrowest of the three forms — so a bare key previewed as "Invalid
/// join link" and then joined fine (ARCH principle 8). The host's refusal
/// arrives as its own words.
#[tauri::command]
pub async fn mesh_preview_join_link(
    state: State<'_, Arc<AppState>>,
    link: String,
) -> Result<JoinConfirmation, String> {
    mesh_client(&state)
        .mesh_join_preview(&link)
        .await
        .map_err(|e| format!("mesh_preview_join_link: {e}"))
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
    let remote: MeshStatusSummary = mesh_client(&state)
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
    let status: MeshStatusSummary = mesh_client(&state)
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
pub struct DiscoveredMemberDto {
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
    pub discovered_peers: Vec<DiscoveredMemberDto>,
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

mod state_response;
pub use state_response::MeshStateResponse;

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
pub struct VenuePreferenceDto {
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
/// client listener (`sovereign_daemon::daemon::start_daemon`), so Local
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
) -> Result<Vec<VenuePreferenceDto>, String> {
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
        .peer_preferences::<VenuePreferenceDto>()
        .await
        .map_err(|e| format!("mesh_list_peer_preferences: {e}"))
}

/// The parse shape of one `GET /v1/mesh/media` row
/// (`commonwealth_media::MediaOffer`), for the fields the Library rail
/// renders. `status` stays the roster's own word.
#[derive(Debug, Clone, Deserialize)]
struct MediaOfferRow {
    peer: String,
    node_id: String,
    status: String,
    #[serde(default)]
    offered_to: Vec<String>,
    /// What the holder's origin can serve right now; absent when the holder
    /// published no presence.
    #[serde(default)]
    media_available: Option<f32>,
}

/// The one field of `MediaReach` the rail needs.
#[derive(Debug, Clone, Deserialize)]
struct MediaReachRow {
    url: String,
}

/// One library in the Library rail's "Libraries on the mesh".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMediaOffer {
    /// The holder's member name.
    pub peer: String,
    pub node_id: String,
    pub status: String,
    /// Who the holder admits; empty = everyone here.
    pub offered_to: Vec<String>,
    /// The loopback bridge the player opens (`reach.rs` `player_url`).
    /// `None` when the host refused the reach — `unreachable` says why — and
    /// `None` when the holder is using the library themself, which
    /// [`MeshMediaOffer::media_available`] says instead.
    pub player_url: Option<String>,
    pub unreachable: Option<String>,
    /// What the holder's origin can serve right now: `0.0` the holder is
    /// watching it, `1.0` free, `None` the holder published no presence.
    ///
    /// The rail does not merely RENDER this. At `0.0` there is no
    /// `player_url` on the row at all, so "does not start a stream" is a fact
    /// about the row rather than a rule the view has to remember (ARCH
    /// principle 10).
    pub media_available: Option<f32>,
}

/// The offers `svrn mesh media` lists, each with the URL its player opens.
///
/// Both halves are the daemon's answers: the list is `GET /v1/mesh/media`
/// (the route the CLI's `list_offers` reads) and each URL is that route's
/// `?peer=` reach — never re-derived from gossip here (ARCH principle 12).
/// A refused reach is a row with its reason, not a dropped row: the
/// library exists even when it cannot be played right now.
async fn media_offers(
    client: &sovereign_turn_client::TurnClient,
) -> Result<Vec<MeshMediaOffer>, String> {
    let rows: Vec<MediaOfferRow> = client
        .mesh_media_offers()
        .await
        .map_err(|e| format!("mesh_media_offers: {e}"))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        // A library in use is not dialed at all. The holder is watching it,
        // so a bridge to it would be a stream started over their shoulder —
        // and the row that carries no URL cannot start one however the view
        // is written.
        let in_use = row.media_available.is_some_and(|v| v <= 0.0);
        let (player_url, unreachable) = if in_use {
            tracing::info!(target: "mesh_state", peer = %row.peer, "mesh_media_offers: in use by its holder — not reaching it");
            (None, None)
        } else {
            match client.mesh_media_reach::<MediaReachRow>(&row.node_id).await {
                Ok(reach) => (Some(reach.url), None),
                Err(e) => {
                    tracing::debug!(target: "mesh_state", peer = %row.peer, error = %e, "mesh_media_offers: reach refused");
                    (None, Some(e.to_string()))
                }
            }
        };
        out.push(MeshMediaOffer {
            peer: row.peer,
            node_id: row.node_id,
            status: row.status,
            offered_to: row.offered_to,
            player_url,
            unreachable,
            media_available: row.media_available,
        });
    }
    tracing::debug!(target: "mesh_state", offering = out.len(), "mesh_media_offers: fetched");
    Ok(out)
}

/// The Library rail's "Libraries on the mesh", polled by `LibraryView`.
#[tauri::command]
pub async fn mesh_media_offers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<MeshMediaOffer>, String> {
    media_offers(&mesh_client(&state)).await
}

/// `mesh_media_probe(playerUrl)` — one real GET through a media bridge
/// BEFORE a browser tab is pointed at it. This is the CLI's documented
/// pattern ("a caller does one real GET / through the bridge if it wants
/// an HTTP status rather than a port"), which the desktop skipped:
/// clicking a member's library opened the bridge URL blind, and a member
/// whose origin is down served the browser an empty reply (2026-09-22,
/// RuggedFox — dial succeeds, far side closes without a byte, nothing in
/// the log). Loopback-http only, so this can never become a general
/// fetch gadget pointed at arbitrary hosts.
#[tauri::command]
pub async fn mesh_media_probe(player_url: String) -> Result<u16, String> {
    probe_media_url(&player_url).await
}

/// The loopback trust rule `player_url` itself enforces on the daemon
/// side, restated here so the probe cannot be aimed anywhere the bridge
/// contract would not have handed out.
fn is_loopback_http(url: &str) -> bool {
    match url.strip_prefix("http://") {
        Some(rest) => matches!(
            rest.split(':').next().unwrap_or(""),
            "127.0.0.1" | "localhost" | "[::1]"
        ),
        None => false,
    }
}

async fn probe_media_url(url: &str) -> Result<u16, String> {
    if !is_loopback_http(url) {
        return Err(format!(
            "refusing to probe `{url}` — only loopback http bridge URLs are probeable"
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    match client.get(url).send().await {
        // Any HTTP status is success — the question was "does a library
        // answer", not "is it healthy"; a 401 from the origin still beats
        // a browser tab that never loads.
        Ok(resp) => Ok(resp.status().as_u16()),
        Err(e) => Err(format!(
            "the bridge connected but the library did not answer ({e}) — \
             the member's media origin may be down on their side"
        )),
    }
}

#[cfg(test)]
mod media_offers_tests;

#[cfg(test)]
mod contribution_view_tests;
