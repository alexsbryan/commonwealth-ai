// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ledger record **ME-10**: inference backpressure must not take down an
//! unrelated control-plane write.
//!
//! # The named failing input (ARCH §18.1)
//!
//! On 2026-08-24 (note b4b36597) a daemon answering
//! `503 host busy, ~30000ms predicted wait` blocked a session's own note
//! writes — twice. The session was not asking for inference. It was recording
//! what it had just learned, and the record was refused because the write went
//! through the same admission gate as the decode path. Backpressure meant to
//! protect a busy GPU took out the control plane with it.
//!
//! # What this pins, and why it drives the REAL router
//!
//! The fix is not in a function; it is in the mount topology. `/v1/rail/append`
//! carries no `admission()` layer while `/api/chat`, `/v1/embeddings` and
//! `/v1/edit_predictions` do — so the assertion has to be made against
//! `server::client_router`, not a hand-built test router. A minimal router
//! assembled here would prove only that this file agrees with itself; adding
//! `.layer(admission())` back onto the rail route in `server.rs` has to be what
//! reddens it.
//!
//! The daemon is put into a real shedding state (contribution paused, which is
//! `admit_peer_request`'s first refusal) rather than by racing the ceiling, so
//! the test is deterministic.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use commonwealth_api::server::client_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use tower::ServiceExt;

/// Loopback, so the client-auth layer admits and the request actually reaches
/// the admission gate. A LAN address would 401 first and every assertion below
/// would pass for the wrong reason.
const LOOPBACK: &str = "127.0.0.1:55021";

/// A well-formed 32-hex wire form — the only form `parse_x_node_id` accepts.
/// Present so `peer_admission_layer` treats the request as peer traffic; a
/// local request returns early and is never gated at all.
const PEER_NODE: &str = "0000000000000000000000000000002a";

/// A state that is SHEDDING: contribution paused an hour out, which is the
/// first refusal in `AppState::admit_peer_request` — reached before any member
/// lookup, so the mesh needs no members and the fixture stays a fixture.
fn shedding_state() -> AppState {
    let state = AppState::new(
        NodeId::from_u128(1),
        Mesh {
            mesh_secret: [0u8; 32],
            invite_expires_at: None,
            id: MeshId::from_u128(7),
            name: "Test".into(),
            invite_key_hash: [3u8; 32],
            invite_version: 0,
            require_encryption: false,
            members: Default::default(),
            peers: vec![],
        },
    );
    state.set_contribution_paused_until(chrono::Utc::now().timestamp() + 3_600);
    state
}

async fn post_as_peer(state: AppState, path: &str, body: &'static str) -> StatusCode {
    let mut req = Request::post(path)
        .header("x-node-id", PEER_NODE)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(LOOPBACK.parse::<SocketAddr>().unwrap()));
    client_router(state).oneshot(req).await.unwrap().status()
}

/// The instrument first (ARCH §18.4): if the inference path is NOT shedding,
/// the control-plane assertion below is vacuous — it would pass on a daemon
/// that gates nothing at all.
#[tokio::test]
async fn the_inference_path_really_is_shedding_in_this_fixture() {
    let status = post_as_peer(shedding_state(), "/v1/embeddings", r#"{"input":"x"}"#).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the fixture must actually shed inference, or the control-plane test \
         below proves nothing. Got {status}"
    );
}

/// ME-10. The control-plane write goes through while inference is shedding.
#[tokio::test]
async fn a_control_plane_write_survives_inference_backpressure() {
    let status = post_as_peer(
        shedding_state(),
        "/v1/rail/append",
        r#"{"kind":"note","text":"what this session just learned"}"#,
    )
    .await;

    assert_ne!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a control-plane write was shed by INFERENCE backpressure — this is \
         note b4b36597, where a 503 'host busy, ~30000ms predicted wait' \
         refused a session's own note write twice. The rail route must not \
         carry the inference admission gate."
    );
    // ...and it must be shed-free because the route is MOUNTED and ungated,
    // not because it is absent. A 404 satisfies `!= 503` for free, and that
    // false pass is exactly the shape this ledger keeps catching.
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "/v1/rail/append is not mounted on this router, so the assertion above \
         passed for the wrong reason"
    );
}
