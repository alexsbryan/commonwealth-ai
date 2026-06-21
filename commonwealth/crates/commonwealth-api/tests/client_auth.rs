// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end behaviour of the `:9741` client-auth layer
//! (`commonwealth_api::client_auth`), driven through the real
//! `client_router` via `tower::oneshot`.
//!
//! `ConnectInfo<SocketAddr>` is injected directly into request
//! extensions — the layer reads it from there, so we don't need a
//! live TCP listener (which would make the loopback-vs-remote split
//! flaky on CI boxes without a routable NIC). The auth decision is
//! made entirely from the injected peer addr + the `Authorization`
//! header + the daemon's installed token.
//!
//! Target path is `/v1/models` (a gated route with a trivial handler):
//! REJECT outcomes (401/403/500) come from the layer *before* the
//! handler, so we assert them exactly; ADMIT outcomes we assert as
//! "not an auth rejection" to stay decoupled from handler internals.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use commonwealth_api::server::client_router;
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55001";
const LAN_PEER: &str = "192.168.1.50:44444";
const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

fn member(id: NodeId) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: "A".into(),
        invited_by: id,
        joined_at: 0,
        last_seen: 0,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 0,
                cpu_cores: 0,
                total_storage_gb: 0,
                free_storage_gb: 0,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: 0,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec!["192.168.1.1:9742".parse::<SocketAddr>().unwrap()],
    }
}

/// `AppState` with `token` installed (`Some` = token configured,
/// `None` = no token → remote callers fail closed).
fn state_with_token(token: Option<&str>) -> AppState {
    let node = NodeId::from_u128(1);
    let mut members = HashMap::new();
    members.insert(node, member(node));
    let mesh = Mesh {
        id: MeshId::from_u128(7),
        name: "Test".into(),
        join_key_hash: [3u8; 32],
        require_encryption: false,
        members,
        peers: vec![],
    };
    let state = AppState::new(node, mesh);
    state.install_client_token(token.map(Arc::<str>::from));
    state
}

/// Oneshot a GET through the real client_router. `peer` = injected
/// ConnectInfo (None ⇒ simulate a listener that forgot connect_info);
/// `bearer` = optional Authorization token.
async fn get_status(
    state: AppState,
    path: &str,
    peer: Option<&str>,
    bearer: Option<&str>,
) -> StatusCode {
    let mut builder = Request::get(path);
    if let Some(b) = bearer {
        builder = builder.header(axum::http::header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    if let Some(p) = peer {
        let addr: SocketAddr = p.parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    client_router(state)
        .oneshot(req)
        .await
        .unwrap()
        .status()
}

fn is_auth_rejection(s: StatusCode) -> bool {
    s == StatusCode::UNAUTHORIZED
        || s == StatusCode::FORBIDDEN
        || s == StatusCode::INTERNAL_SERVER_ERROR
}

// ── loopback is always admitted ──────────────────────────────────

#[tokio::test]
async fn loopback_admitted_without_token_even_when_none_configured() {
    // No token configured (single-user/localhost deployment) and no
    // bearer: a loopback caller must pass straight through.
    let status = get_status(state_with_token(None), "/v1/models", Some(LOOPBACK), None).await;
    assert!(
        !is_auth_rejection(status),
        "loopback caller must be admitted (got {status})"
    );
}

// ── remote requires the token ────────────────────────────────────

#[tokio::test]
async fn remote_with_correct_bearer_is_admitted() {
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        Some(TOKEN),
    )
    .await;
    assert!(
        !is_auth_rejection(status),
        "remote caller with the right token must be admitted (got {status})"
    );
}

#[tokio::test]
async fn remote_with_wrong_bearer_is_401() {
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn remote_without_bearer_is_401() {
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── fail-closed cases ────────────────────────────────────────────

#[tokio::test]
async fn remote_with_no_token_configured_fails_closed_403() {
    // Bound somewhere a remote reached us, but no token was installed.
    // Must refuse — never admit an unauthenticated remote caller.
    let status = get_status(
        state_with_token(None),
        "/v1/models",
        Some(LAN_PEER),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn missing_connect_info_fails_closed_500() {
    // Listener forgot into_make_service_with_connect_info: can't
    // identify the caller, so refuse rather than admit.
    let status = get_status(state_with_token(Some(TOKEN)), "/v1/models", None, None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

// ── exempt paths stay open to remote callers ─────────────────────

#[tokio::test]
async fn exempt_path_admits_remote_without_token() {
    // /oicp/v1/capabilities is the federation handshake — a peer must
    // read it before it could hold a token.
    let status = get_status(
        state_with_token(Some(TOKEN)),
        "/oicp/v1/capabilities",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert!(
        !is_auth_rejection(status),
        "exempt federation path must be reachable without a token (got {status})"
    );
}

#[tokio::test]
async fn gated_path_still_blocks_when_exempt_path_is_open() {
    // Sanity: the exemption is per-exact-path, not a global off switch.
    // Same remote caller, no token → /v1/models blocked, /status open.
    let blocked = get_status(
        state_with_token(Some(TOKEN)),
        "/v1/models",
        Some(LAN_PEER),
        None,
    )
    .await;
    assert_eq!(blocked, StatusCode::UNAUTHORIZED);

    let open = get_status(state_with_token(Some(TOKEN)), "/status", Some(LAN_PEER), None).await;
    assert!(!is_auth_rejection(open), "/status must stay open (got {open})");
}
