// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP surface for mesh mutation. Lets an out-of-process client
//! (most notably the desktop app, when it detects that a CLI-started
//! daemon already owns `:9741`) drive `create / join / rotate / leave`
//! without reimplementing the state machine.
//!
//! Before this module, mesh mutations were Rust-only via
//! `EmbeddedDaemon::{create_mesh, join_mesh, stop}` — which meant a
//! second process couldn't change mesh state without attempting to
//! start its own daemon (silent port collision, no mesh parity).
//!
//! Routes mount under `/v1/mesh/*` on the same `:9741` listener as
//! `/v1/chat/completions` and `/mcp`. Localhost-only: any non-loopback
//! caller gets `403 Forbidden`, same guard as `mcp_router`. The
//! endpoints are deliberately symmetric with the `sovereign mesh …`
//! CLI subcommands.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::daemon::EmbeddedDaemon;
use crate::loopback_guard::enforce_localhost;

/// Build the mesh HTTP router. Merged into the daemon's client router
/// next to `mcp_router`. Call once at `start_daemon` time and hand the
/// same `Arc<EmbeddedDaemon>` that `start_daemon` owns internally.
pub fn mesh_router(daemon: Arc<EmbeddedDaemon>) -> Router {
    Router::new()
        .route("/v1/mesh/status", get(mesh_status))
        .route("/v1/mesh/create", post(mesh_create))
        .route("/v1/mesh/join", post(mesh_join))
        .route("/v1/mesh/rotate", post(mesh_rotate))
        .route("/v1/mesh/leave", post(mesh_leave))
        .route("/v1/mesh/relay-candidates", get(mesh_relay_candidates))
        // Router-level loopback guard — defense in depth on top of
        // the per-handler `enforce_localhost` checks. Adding a new
        // route to this module inherits the guard for free; the
        // per-handler check stays as a secondary barrier.
        .layer(axum::middleware::from_fn(
            crate::loopback_guard::loopback_only,
        ))
        .layer(Extension(daemon))
}

/// Request body for `POST /v1/mesh/create`. Both fields default so a
/// bare `POST` with empty body is valid — mirrors `sovereign mesh create`
/// with no args.
#[derive(Debug, Deserialize, Default)]
pub struct CreateRequest {
    /// Human-readable mesh name. Defaults to `"<host>'s Mesh"`.
    #[serde(default)]
    pub name: Option<String>,
    /// Node display name. Defaults to the machine's hostname.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Create an ENCRYPTED mesh (founder-set policy: all peers enforce
    /// iroh dial-by-key + encrypted join). Defaults to plaintext.
    #[serde(default)]
    pub encrypt: bool,
}

#[derive(Debug, Serialize)]
pub struct CreateResponse {
    pub mesh_name: String,
    pub join_key: String,
    pub join_link: String,
    /// Bearer token a remote peer/client must present, shown beside the
    /// join key on the invite screen. `None` if the daemon stayed
    /// loopback-only. See `client_auth`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_token: Option<String>,
}

/// Request body for `POST /v1/mesh/join`. Accepts any of the three
/// forms the CLI accepts: bare `cwth-…` key, `https://sovereign.dev/join/…`
/// URL, or `sovereign://join/…` deep link.
#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub key_or_url: String,
    #[serde(default)]
    pub node_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JoinResponse {
    pub mesh_name: String,
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RotateResponse {
    pub mesh_name: String,
    pub join_key: String,
}

/// Shape returned by `GET /v1/mesh/status`. Flat and JSON-friendly so
/// desktop UIs can render it without a client-side wrapper type.
/// Field names are aligned with the existing `MeshState` the desktop
/// already knows how to render — an HTTP client can round-trip through
/// this DTO without losing information.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub running: bool,
    pub mesh_name: Option<String>,
    pub members_online: usize,
    pub members_total: usize,
    pub members: Vec<MemberDto>,
    /// Current shareable invite. `None` when the daemon is solo,
    /// or when the persisted mesh predates the join_key.secret cache
    /// (a rotate recovers the link). The frontend hides the share
    /// card when these are absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub join_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub join_link: Option<String>,
    /// Client-API bearer token for remote callers. `Some` once the
    /// daemon is exposed (shared mesh); `None` for a loopback-only
    /// solo daemon. Surfaced beside the invite so the share UI can
    /// render it after a restart without re-creating the mesh.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_token: Option<String>,
    /// RPC inference workers + their eligibility state (host side; empty on a
    /// node not running RPC discovery). Lets an operator see WHY a worker isn't
    /// being distributed to — e.g. `quarantined` with a cooldown after flapping.
    /// See `crate::worker_eligibility`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub rpc_workers: Vec<crate::worker_eligibility::WorkerStatusView>,
    /// True when THIS node is the current shared-model host (it assembles +
    /// distributes the RPC layer-split). Published by the daemon's discovery
    /// loop via [`set_shared_model_host`]; lets a mesh soak assert the
    /// no-split-brain invariant — at most one host across the fleet.
    #[serde(default)]
    pub shared_model_host: bool,
    /// Cluster-health summary when this node is in a shared-model fleet
    /// (`SOVEREIGN_SHARED_MODEL_ID` set); `None` otherwise. Powers the desktop
    /// "Shared model" chip (`k/N anchors · available|forming`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shared_model: Option<SharedModelStatusDto>,
    /// Peer-admission load: current in-flight peer requests + the configured
    /// ceiling. Lets a multi-process soak assert AdmissionSafety over HTTP
    /// (inflight ≤ ceiling, → 0 at quiescence) — previously DST-only. Serde
    /// default keeps older status consumers wire-compatible.
    #[serde(default)]
    pub peer_inflight_current: usize,
    #[serde(default)]
    pub peer_inflight_ceiling: usize,
    /// Current outbound peer knowledge fan-out width (the `fanout_inflight`
    /// gauge). Lets the soak assert `BoundedFanOut` over HTTP. Serde default
    /// keeps older status consumers wire-compatible.
    #[serde(default)]
    pub fanout_inflight_current: usize,
    /// Corpora currently being ingested on this node — the soak's ingest /
    /// inference-contention signal (0 when idle).
    #[serde(default)]
    pub active_corpus_ingests: usize,
    /// Per-peer iroh connection path (H2 observability): `direct` /
    /// `relayed` / `mixed` / `idle` for each known peer. Empty when
    /// iroh isn't running (mesh on the IP path) — so it also answers
    /// "is this mesh actually on iroh, and via relay or direct?".
    /// Serde default keeps older consumers wire-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iroh_transport: Vec<crate::daemon::IrohPeerPath>,
    /// This founder's OWN iroh reachability (Track W): relay-homed?,
    /// discoverable?, plus the self-heal watchdog's recovery history. `None`
    /// when iroh isn't running. Answers "am I actually dialable right now?".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub founder_reachability: Option<crate::daemon::FounderReachability>,
}

/// Cluster-health snapshot of a shared-model fleet, surfaced on
/// `GET /v1/mesh/status` for the desktop chip + degraded banner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedModelStatusDto {
    /// The shared model this fleet runs (e.g. `glm-5.2`).
    pub model_id: String,
    /// Eligible anchors currently gossiped (online + `can_anchor`).
    pub eligible_anchors: usize,
    /// Quorum target — the host won't distribute below this many anchors.
    pub quorum_anchors: u32,
    /// `true` once the anchor quorum is met — a proxy for "the shared model is
    /// serveable" (the exact engine load state isn't HTTP-observable). Below
    /// quorum the cluster is "forming" and consumers fall back to local.
    pub available: bool,
}

/// Build the shared-model cluster-health summary, or `None` when this node
/// isn't in a shared-model fleet. Reads the eligible-anchor count from the
/// daemon and the fleet's model id / quorum from the RPC env the role
/// translation set (`apply_shared_model_role_to_env`).
async fn shared_model_status(daemon: &EmbeddedDaemon) -> Option<SharedModelStatusDto> {
    let model_id = std::env::var("SOVEREIGN_SHARED_MODEL_ID")
        .ok()
        .filter(|s| !s.is_empty())?;
    let eligible_anchors = daemon.eligible_anchors().await.len();
    let quorum_anchors = std::env::var("SOVEREIGN_RPC_QUORUM_ANCHORS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1);
    Some(SharedModelStatusDto {
        model_id,
        eligible_anchors,
        quorum_anchors,
        available: eligible_anchors >= quorum_anchors as usize,
    })
}

/// Runtime "am I the shared-model host" flag. Published by the daemon's
/// RPC-discovery loop (`daemon_cmd::bootstrap`) each tick the elected host role
/// changes, and surfaced on `GET /v1/mesh/status` so the mesh soak can assert
/// at-most-one-host. A process-global atomic — there is exactly one daemon per
/// process and one host role per daemon.
static SHARED_MODEL_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Publish whether this node is currently the shared-model host.
pub fn set_shared_model_host(is_host: bool) {
    SHARED_MODEL_HOST.store(is_host, std::sync::atomic::Ordering::Relaxed);
}

/// Read the published shared-model host flag (for `/v1/mesh/status`).
pub fn is_shared_model_host() -> bool {
    SHARED_MODEL_HOST.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemberDto {
    pub node_id: String,
    pub name: String,
    pub is_self: bool,
    /// `"online"` | `"busy"` | `"away"` | `"offline"` — matches the
    /// `MemberStatus` serde rename in `crate::types`.
    pub status: String,
    /// Advertised total GPU VRAM (GB) summed across this member's GPUs — the
    /// live input for `svrn mesh plan --from-mesh`. `0` if the member advertises
    /// no GPU (or gossip from an older daemon that didn't carry it).
    #[serde(default)]
    pub vram_gb: u32,
    /// This member advertises itself as a shared-model anchor (an eligible
    /// tensor-split worker). `svrn mesh plan --from-mesh` places the model
    /// across the anchors + self.
    #[serde(default)]
    pub can_anchor: bool,
    /// Routable addresses (typically tailnet `host:port`) advertised
    /// by this member. Empty until the first gossip round populates
    /// them. Consumed by `sovereign mesh status` to render the per-
    /// member address row and to power `--self --addr-only` for
    /// scripting the SOVEREIGN_FOUNDER_ADDR capture pattern in
    /// pod-deployment workflows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
}

fn default_node_name(override_name: Option<String>) -> String {
    override_name.unwrap_or_else(|| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "sovereign-node".to_string())
    })
}

// ─── Handlers ────────────────────────────────────────────────────

/// `GET /v1/mesh/status` — read-only snapshot for UI polling.
async fn mesh_status(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }

    let running = daemon.is_running().await;
    // RPC worker eligibility (host side) — the same tracker the discovery loop
    // gates on, so the operator sees the live state without DEBUG logs.
    let rpc_workers = crate::worker_eligibility::global()
        .map(|e| e.status_views(std::time::Instant::now()))
        .unwrap_or_default();
    // Shared-model cluster health (None unless this node is in a shared-model
    // fleet). Powers the desktop chip + degraded banner in both UI reach modes.
    let shared_model = shared_model_status(&daemon).await;
    let (
        peer_inflight_current,
        peer_inflight_ceiling,
        fanout_inflight_current,
        active_corpus_ingests,
    ) = daemon.glassbox_signals().await;
    let Some(s) = daemon.mesh_state().await else {
        // Running but no mesh — e.g. the daemon started solo and the
        // user hasn't run `mesh create` yet. Empty but valid payload
        // keeps the UI's empty-state rendering happy.
        return (
            StatusCode::OK,
            Json(
                serde_json::to_value(StatusResponse {
                    running,
                    mesh_name: None,
                    members_online: 0,
                    members_total: 0,
                    members: vec![],
                    join_key: None,
                    join_link: None,
                    client_token: daemon.running_client_token().await,
                    rpc_workers,
                    shared_model_host: is_shared_model_host(),
                    shared_model: shared_model.clone(),
                    peer_inflight_current,
                    peer_inflight_ceiling,
                    fanout_inflight_current,
                    active_corpus_ingests,
                    iroh_transport: vec![], // no mesh → no peers
                    founder_reachability: None, // no mesh → no founder endpoint
                })
                .unwrap(),
            ),
        )
            .into_response();
    };

    // H2: per-peer iroh path (empty when iroh isn't running).
    let iroh_transport = daemon.iroh_transport_snapshot().await;
    // Track W: this founder's own reachability (relay-home + discovery health).
    let founder_reachability = daemon.founder_reachability().await;

    // Map MemberStatus to its serde-renamed variant. `MeshMember`
    // already owns its `node_id` as a String (set by `mesh_state`), so
    // we don't need to Debug-format a NodeId byte array.
    let members: Vec<MemberDto> = s
        .members
        .iter()
        .map(|m| MemberDto {
            node_id: m.node_id.clone(),
            name: m.name.clone(),
            is_self: m.is_self,
            status: match m.status {
                crate::types::MemberStatus::Online => "online",
                crate::types::MemberStatus::Busy => "busy",
                crate::types::MemberStatus::Away => "away",
                crate::types::MemberStatus::Offline => "offline",
            }
            .to_string(),
            vram_gb: m.vram_gb,
            can_anchor: m.can_anchor,
            addresses: m.addresses.clone(),
        })
        .collect();

    let (join_key, join_link) = match daemon.current_invite().await {
        Some((k, l)) => (Some(k), Some(l)),
        None => (None, None),
    };

    (
        StatusCode::OK,
        Json(
            serde_json::to_value(StatusResponse {
                running,
                mesh_name: Some(s.status.name),
                members_online: s.status.members_online,
                members_total: s.status.members_total,
                members,
                join_key,
                join_link,
                client_token: daemon.running_client_token().await,
                rpc_workers,
                shared_model_host: is_shared_model_host(),
                shared_model,
                peer_inflight_current,
                peer_inflight_ceiling,
                fanout_inflight_current,
                active_corpus_ingests,
                iroh_transport,
                founder_reachability,
            })
            .unwrap(),
        ),
    )
        .into_response()
}

/// `POST /v1/mesh/create` — promote the solo daemon to a joinable mesh
/// and return the shareable invite.
async fn mesh_create(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    body: Option<Json<CreateRequest>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let node_name = default_node_name(req.node_name);
    let mesh_name = req.name.unwrap_or_else(|| format!("{node_name}'s Mesh"));
    let encrypt = req.encrypt;

    // Explicit create = opt into serving remote peers. Mark exposed so
    // the daemon binds non-loopback (+ requires a bearer token). For an
    // already-running daemon this takes effect on the next restart
    // (client_bind is restart-required); the desktop reloads on the
    // config change.
    daemon.expose_client_api();

    match daemon
        .create_mesh_with(&mesh_name, &node_name, encrypt)
        .await
    {
        Ok(result) => (
            StatusCode::OK,
            Json(
                serde_json::to_value(CreateResponse {
                    mesh_name: result.mesh_name,
                    join_key: result.join_key,
                    join_link: result.join_link,
                    client_token: result.client_token,
                })
                .unwrap(),
            ),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /v1/mesh/join` — join an existing mesh by key or URL.
async fn mesh_join(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
    Json(req): Json<JoinRequest>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    // Accept bare key, https URL, or sovereign:// deep link — matches
    // what the CLI's `sovereign mesh join` takes.
    let link = match crate::deep_link::parse_join_argument(&req.key_or_url) {
        Some(l) => l,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "key_or_url must be a bare cwth-… key, an https://sovereign.dev/join/… URL, or a sovereign://join/… deep link"
                })),
            )
                .into_response();
        }
    };
    let node_name = default_node_name(req.node_name);

    // Joining a mesh = serving/relating to remote peers → expose.
    daemon.expose_client_api();

    match daemon.join_mesh(&link, &node_name).await {
        Ok(result) => (
            StatusCode::OK,
            Json(
                serde_json::to_value(JoinResponse {
                    mesh_name: result.mesh_name,
                    node_id: result.node_id,
                    client_token: result.client_token,
                })
                .unwrap(),
            ),
        )
            .into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /v1/mesh/rotate` — regenerate the join key for an existing
/// mesh. Delegates to `persist::rotate_join_key`; the daemon's
/// in-memory hash is refreshed on next restart (the handler surfaces
/// this in the response so the caller can decide whether to also hit
/// `/v1/admin/reload`).
async fn mesh_rotate(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    // Pull data_dir via a new accessor — rotate_join_key works on disk
    // state, independent of the running daemon.
    let data_dir = daemon.data_dir().to_path_buf();
    match crate::persist::rotate_join_key(&data_dir) {
        Ok(Some(rotated)) => {
            // Refresh the daemon's in-memory plaintext too so the next
            // /v1/mesh/status poll reflects the new link without a
            // restart. (The in-memory hash on the running daemon is
            // still stale until restart — same long-standing wart;
            // members already in the mesh remain connected, only new
            // joins use the new key.)
            daemon.set_join_key(rotated.join_key.clone()).await;
            // Rotation exists to SHARE the new key — arm a fresh
            // invite TTL for an encrypted mesh (create-time was the
            // only arming site before, so a rotated encrypted invite
            // carried a stale/absent expiry), and mark the daemon
            // client-exposed so a soloist rotating-to-share hands out
            // an invite for a daemon that will actually serve peers
            // (bind + token apply on next start, same as create).
            daemon.rearm_join_key_expiry().await;
            daemon.expose_client_api();
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(RotateResponse {
                        mesh_name: rotated.mesh_name,
                        join_key: rotated.join_key,
                    })
                    .unwrap(),
                ),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no mesh to rotate" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `GET /v1/mesh/relay-candidates` — enumerate the host's
/// reachable IPs so the founder can copy one into a `?relay=…`
/// query param when sharing the invite. Doesn't require a running
/// mesh — the candidates are interface-derived and a user might
/// want to look at them before deciding to create a mesh.
async fn mesh_relay_candidates(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    // Internal port is fixed at 9742 today (matches what the daemon
    // binds in start_daemon and what the gossip handshake targets).
    // Plumbing this through config is a follow-up; for now the
    // single source of truth lives next to the binder.
    let candidates = crate::mesh_discovery::relay_candidates(9742);
    (
        StatusCode::OK,
        Json(serde_json::json!({ "candidates": candidates })),
    )
        .into_response()
}

/// `POST /v1/mesh/leave` — leave the current mesh and re-create a fresh
/// solo mesh **in this same process**. Mirrors the desktop "Leave mesh"
/// button.
///
/// The node returns to being its own solo mesh with the client API
/// staying available on the same process — no restart, no model reload,
/// no dependency on a service manager to relaunch us (the old design
/// exited with code 103 and hoped launchd/systemd would bring us back,
/// which stranded `:9741` when nothing supervised the daemon).
async fn mesh_leave(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(daemon): Extension<Arc<EmbeddedDaemon>>,
) -> impl IntoResponse {
    if let Err(r) = enforce_localhost(&peer) {
        return r;
    }
    // Must be running to leave — preserve the 409 contract for callers.
    if !daemon.is_running().await {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "mesh is not running" })),
        )
            .into_response();
    }
    // Do NOT tear down synchronously: this handler is being served BY the
    // `:9741` listener that `leave()` drops, so leaving inline would cancel
    // our own response mid-flight (the historical "connection reset / :9741
    // down forever on leave" bug). Instead ACK now on the still-live
    // listener, then re-solo in a detached task after a short grace so the
    // `204` flushes first. `leave_to_solo` rebinds `:9741` in THIS process
    // within ~1s — the desktop's reconnect poll catches it.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        if let Err(e) = daemon.leave_to_solo().await {
            tracing::error!(
                error = %e,
                "mesh leave: in-process re-solo failed; :9741 may stay down \
                 until a daemon restart"
            );
        }
    });
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmbeddedDaemon;
    use sovereign_core::setup_config::{
        DaemonSection, DiscoverySection, IrohSection, ModelsSection, SetupConfig,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Hermetic daemon config for tests: ephemeral ports (`0`) so parallel
    /// `create`/`leave` tests never fight over the real `:9741`/`:9742` — a
    /// bind conflict is now a hard error (`MeshError::Network`) rather than
    /// silently swallowed — and mDNS + iroh off so no unit test touches a
    /// multicast socket or binds an iroh endpoint. Everything else defaulted.
    fn hermetic_cfg() -> SetupConfig {
        SetupConfig {
            compute: Default::default(),
            models: ModelsSection {
                primary: PathBuf::from("/models/primary.gguf"),
                fast: None,
                embed: PathBuf::from("/models/embed.gguf"),
                code: None,
                context_size: None,
                max_extras_memory_gb: None,
                extra: BTreeMap::new(),
                primary_pool: None,
            },
            daemon: DaemonSection {
                client_port: 0,
                internal_port: 0,
                ..Default::default()
            },
            data: Default::default(),
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: IrohSection {
                enabled: Some(false),
                ..Default::default()
            },
            shared_model: Default::default(),
            discovery: DiscoverySection {
                mdns: false,
                ..Default::default()
            },
            mcp_servers: Vec::new(),
        }
    }

    /// Stand up the mesh HTTP router over a no-mesh daemon bound to
    /// an ephemeral localhost port. Returns `(daemon_arc, base_url,
    /// _tmp)` — hold the tempdir so it isn't cleaned up mid-test.
    async fn spawn_test_router() -> (Arc<EmbeddedDaemon>, String, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Arc::new(EmbeddedDaemon::new(tmp.path().to_path_buf()));
        daemon.set_setup_config(hermetic_cfg()).await;
        let app = mesh_router(Arc::clone(&daemon));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .ok();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (daemon, format!("http://{addr}"), tmp)
    }

    #[tokio::test]
    async fn status_returns_empty_when_no_mesh() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["running"], false);
        assert_eq!(body["member_count"].as_u64().unwrap_or(0), 0);
    }

    #[tokio::test]
    async fn create_and_status_round_trip() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["mesh_name"], "test mesh");
        assert!(body["join_key"].as_str().unwrap().starts_with("cwth-"));
        assert!(body["join_link"].as_str().unwrap().contains("sovereign://"));

        // Status should now report running + one member.
        let resp = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["running"], true);
        assert_eq!(body["mesh_name"], "test mesh");
        assert_eq!(body["members_total"], 1);
    }

    /// The user-facing `POST /v1/mesh/leave` must return the node to a live
    /// SOLO mesh in the SAME process — `/v1/mesh/status` keeps answering, no
    /// restart. Regression guard for the bug where leaving a mesh killed
    /// `:9741` with no way back (the daemon tore down its listeners and
    /// relied on a service manager that wasn't there to relaunch it).
    #[tokio::test]
    async fn http_leave_returns_to_solo_mesh() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();

        // Create a mesh so leave() has something to leave.
        let resp = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let resp = client
            .post(format!("{base}/v1/mesh/leave"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);

        // The re-solo runs in a detached task after a short grace, so poll
        // status until the fresh solo mesh is back up (same process, same
        // test listener). We wait specifically for a mesh that is NOT the
        // old "test mesh" — the pre-teardown window still reports the old
        // one as running. A missing re-solo would never satisfy this and
        // fail the assertion after the loop.
        let mut running_solo = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let body: serde_json::Value = client
                .get(format!("{base}/v1/mesh/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if body["running"] == true
                && body["members_total"] == 1
                && body["mesh_name"].as_str() != Some("test mesh")
            {
                running_solo = true;
                break;
            }
        }
        assert!(
            running_solo,
            "POST /v1/mesh/leave should re-create a live solo mesh in-process"
        );
    }

    /// A DIRECT `leave()` — the path `join_mesh`'s auto-leave and the
    /// deprecated `stop()` take when switching meshes — must NOT re-create a
    /// solo mesh. It leaves the daemon Stopped so the caller can join the
    /// next mesh; only the user-facing `leave_to_solo` bounces back to solo.
    #[tokio::test]
    async fn direct_leave_leaves_daemon_stopped() {
        let (daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert!(daemon.is_running().await);

        // Leave via the library method, exactly as join_mesh's auto-leave does.
        daemon.leave().await.unwrap();

        assert!(
            !daemon.is_running().await,
            "direct leave() must leave the daemon Stopped (no auto re-solo — that \
             would restart the daemon mid mesh-switch)"
        );
    }

    /// Leaving and re-soloing repeatedly must rebind the SAME address cleanly
    /// every time. `stop_inner` awaits the old serve task before `create_mesh`
    /// rebinds, so the in-process rebind never races the just-dropped socket
    /// into `EADDRINUSE`. Uses a fixed (non-ephemeral) port so each iteration
    /// genuinely re-binds the same `host:port` — the exact race being guarded.
    #[tokio::test]
    async fn leave_to_solo_rebinds_same_port_repeatedly() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = EmbeddedDaemon::new(tmp.path().to_path_buf());
        let mut cfg = hermetic_cfg();
        cfg.daemon.client_port = 39411;
        cfg.daemon.internal_port = 39412;
        daemon.set_setup_config(cfg).await;

        daemon.create_mesh("test mesh", "alice").await.unwrap();
        for i in 0..5 {
            daemon
                .leave_to_solo()
                .await
                .unwrap_or_else(|e| panic!("re-solo #{i} failed (bind race?): {e}"));
            assert!(
                daemon.is_running().await,
                "daemon should be running after re-solo #{i}"
            );
        }
    }

    #[tokio::test]
    async fn create_fails_when_mesh_already_exists() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let _ = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "first" }))
            .send()
            .await
            .unwrap();
        let resp = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "second" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 409, "second create must conflict");
    }

    #[tokio::test]
    async fn rotate_after_create_changes_join_key_hash() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();

        let create: serde_json::Value = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "m" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let original_key = create["join_key"].as_str().unwrap().to_string();

        let rotate: serde_json::Value = client
            .post(format!("{base}/v1/mesh/rotate"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let rotated_key = rotate["join_key"].as_str().unwrap();
        assert!(rotated_key.starts_with("cwth-"));
        assert_ne!(original_key, rotated_key);
    }

    #[tokio::test]
    async fn rotate_without_mesh_returns_404() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/mesh/rotate"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn status_includes_invite_after_create() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let create: serde_json::Value = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "Lab Squad" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let created_key = create["join_key"].as_str().unwrap().to_string();

        let status: serde_json::Value = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            status["join_key"].as_str().unwrap(),
            created_key,
            "status must echo back the same plaintext key"
        );
        let link = status["join_link"].as_str().unwrap();
        assert!(link.starts_with("sovereign://join/"));
        assert!(link.contains(&created_key));
    }

    #[tokio::test]
    async fn status_omits_invite_when_solo() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let status: serde_json::Value = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(status.get("join_key").is_none_or(|v| v.is_null()));
        assert!(status.get("join_link").is_none_or(|v| v.is_null()));
    }

    #[tokio::test]
    async fn rotate_refreshes_status_invite_in_place() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let create: serde_json::Value = client
            .post(format!("{base}/v1/mesh/create"))
            .json(&serde_json::json!({ "name": "m" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pre_key = create["join_key"].as_str().unwrap().to_string();

        let rotate: serde_json::Value = client
            .post(format!("{base}/v1/mesh/rotate"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let new_key = rotate["join_key"].as_str().unwrap().to_string();
        assert_ne!(pre_key, new_key);

        let status: serde_json::Value = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["join_key"].as_str().unwrap(), new_key);
        assert!(status["join_link"].as_str().unwrap().contains(&new_key));
    }

    #[tokio::test]
    async fn relay_candidates_endpoint_returns_classified_array() {
        // Doesn't require a mesh — just lists local interfaces.
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{base}/v1/mesh/relay-candidates"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let arr = body["candidates"].as_array().expect("candidates is array");
        // Test runners on macOS / Linux always have at least one
        // non-loopback interface (even CI VMs). Each entry must be
        // shape-correct so the desktop's typed deserializer doesn't
        // silently drop fields.
        for c in arr {
            assert!(c["ip"].is_string());
            assert!(c["kind"].is_string());
            assert!(c["url_fragment"].is_string());
            assert!(c["recommended"].is_boolean());
        }
        // At most one should be marked recommended.
        let recommended_count = arr
            .iter()
            .filter(|c| c["recommended"].as_bool().unwrap_or(false))
            .count();
        assert!(
            recommended_count <= 1,
            "got {recommended_count} recommended"
        );
    }

    #[tokio::test]
    async fn join_rejects_unparseable_input() {
        let (_daemon, base, _tmp) = spawn_test_router().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{base}/v1/mesh/join"))
            .json(&serde_json::json!({ "key_or_url": "not a valid key" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }
}
