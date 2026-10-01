// SPDX-License-Identifier: AGPL-3.0-or-later
//! `tg-2-strangers-are-refused` — the internal port, driven as a stranger and
//! as a member cw-rails forwards.
//!
//! Through the REAL `internal_router` rather than a hand-rolled one: a test
//! that mounts its own routes cannot fail on a gate that was never applied in
//! `server.rs`, which is the only failure that matters here (ARCH principle 5
//! — assert on something the subject cannot author).
//!
//! Since pb-mesh-exit-transport a member reaches this port only through
//! cw-rails, which forwards over loopback with the verified key and svrn's
//! registration tie (`internal_principal`); the plain-IP mesh proof and the
//! join door this file also covered went with the daemon's mesh.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use kernel_types::NodeId;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;
use tower::ServiceExt;

use crate::common;

/// A LAN address — the stranger's.
const LAN: [u8; 4] = [10, 0, 0, 7];

const MEMBER_KEY: [u8; 32] = [7u8; 32];

fn with_peer(mut req: Request<Body>, peer: SocketAddr) -> Request<Body> {
    req.extensions_mut().insert(ConnectInfo(peer));
    req
}

fn quiesce() -> axum::http::request::Builder {
    Request::post("/internal/mesh/quiesce").header("content-type", "application/json")
}

/// ── An uncredentialed non-loopback POST to quiesce is 401, AND the flag it
/// was trying to flip did not move.
///
/// The second half is the one that makes this a refusal rather than a status
/// code: a gate that answered 401 after running the handler would look
/// identical on the wire.
#[tokio::test]
async fn a_stranger_cannot_quiesce_this_node_and_the_flag_does_not_move() {
    let state = AppState::new(NodeId::from_u128(1));
    assert!(!state.mesh_quiesced(), "the fixture starts un-quiesced");

    let resp = internal_router(state.clone())
        .oneshot(with_peer(
            quiesce().body(Body::from(r#"{"quiesced":true}"#)).unwrap(),
            SocketAddr::from((LAN, 51000)),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !state.mesh_quiesced(),
        "the refusal ran BEFORE the handler — a 401 with the flag flipped is \
         not a refusal, it is a side effect with a status code"
    );
}

/// ── A member cw-rails forwards — loopback, the verified key the roster
/// names, and the live registration tie — reaches the same route and flips
/// the flag. Read with the test above, the gate distinguishes a stranger from
/// a member by what cw-rails proved, not by where the request came from.
#[tokio::test]
async fn a_member_cw_rails_forwards_can_quiesce_this_node() {
    let self_id = NodeId::from_u128(1);
    let member = NodeId::from_u128(0x77);
    let (roster, seed) = common::roster_seed(self_id, "Gate Test");
    common::name_member_with_key(&roster, member, "LittleMac", MEMBER_KEY);
    let state =
        AppState::new_with_platform_and_engine_and_gauge_and_fabric(self_id, None, None, seed);
    let _tie = common::tie_as_cw_rails(&state, common::TIE);

    let resp = internal_router(state.clone())
        .oneshot(with_peer(
            quiesce()
                .header("X-Mesh-Member", "LittleMac")
                .header("X-Mesh-Node", member.to_string())
                .header("X-Mesh-Pubkey", hex::encode(MEMBER_KEY))
                .header(kernel_types::member::ORIGIN_TIE_HEADER, common::TIE)
                .body(Body::from(r#"{"quiesced":true}"#))
                .unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 51000)),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.mesh_quiesced(), "a member's request is served");
}
