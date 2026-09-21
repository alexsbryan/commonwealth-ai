// SPDX-License-Identifier: AGPL-3.0-or-later
//! `tg-2-strangers-are-refused`, the ring-sync half: the ROUTE refuses an
//! asker that proved nothing, on its own, without the layer in front.
//!
//! **Why the handler and not the router.** `internal_gate` already refuses an
//! unmarked non-loopback caller with a 401, so driving the mounted route can
//! never reach `roster_refusal`'s catch-all — and a route whose module header
//! promises "served to holders of this mesh's secret" while its own code serves
//! anyone is a guard nobody has watched fail (ARCH principle 5). Calling the
//! handler directly is what makes the route's half of the promise checkable.
//!
//! The pair is deliberate. The marked asker is the negative control: without
//! it, the refusal below could be a route that refuses everyone, which would
//! stop every file-rostered ring replicating on a plaintext mesh.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::Extension;
use commonwealth_rail::SigningKey;
use sovereign_daemon::internal_principal::ProvedMeshMember;
use sovereign_daemon::routes_internal::ring_sync;
use sovereign_peer_wire::RingSyncRequest;

use super::{state_with_rail, NS};

const LAN: [u8; 4] = [10, 0, 0, 7];

async fn ask(proved: Option<ProvedMeshMember>) -> (StatusCode, String) {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[3u8; 32]);
    let state = state_with_rail(dir.path(), &key);
    let body = serde_json::to_vec(&RingSyncRequest {
        namespace: NS.to_string(),
        digest: Default::default(),
        ops: Vec::new(),
    })
    .unwrap();

    let resp = ring_sync(
        State(state),
        // No principal: on a plaintext hop a mesh proof names nobody, so this
        // is what a plain-IP member resolves to as well.
        None,
        proved.map(Extension),
        Some(Extension(ConnectInfo(SocketAddr::from((LAN, 51000))))),
        body.into(),
    )
    .await;

    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// THE failing input: an asker on the LAN that presented nothing and proved
/// nothing is refused BY THIS ROUTE, and the refusal says what was missing.
#[tokio::test]
async fn an_unmarked_non_loopback_asker_is_refused_ring_sync_by_name() {
    let (status, body) = ask(None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body.contains("proved nothing"),
        "the refusal must name what was missing, not merely refuse: {body}"
    );
}

/// The negative control. A holder of this mesh's secret is served, roster or no
/// roster — the half this row deliberately leaves open, because refusing it
/// would stop file-rostered rings replicating on every plaintext mesh.
#[tokio::test]
async fn a_marked_asker_is_still_served_without_a_roster_check() {
    let (status, body) = ask(Some(ProvedMeshMember)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a marked asker is served as Anonymous was served before: {body}"
    );
}
