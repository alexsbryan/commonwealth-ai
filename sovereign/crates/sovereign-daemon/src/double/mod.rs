// SPDX-License-Identifier: AGPL-3.0-or-later
//! The records the daemon's seeds take, built for a test that drives a
//! daemon route (pb-serve-ranks-tests-stock): roster members, a solo mesh,
//! the bound router and a recording ledger. Behind `test-doubles`, so a
//! composition root's tests (sovereign-stock) build them without naming
//! commonwealth-core; the daemon's own test tree re-exports them from
//! `tests/main/common`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;

use oicp_types::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use kernel_types::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};

pub mod ledger_double;

// ── Capabilities + member helpers ───────────────────────────────

/// A `NodeCapabilities` with every field zeroed / empty. Useful for
/// constructing test `MemberRecord`s where the hardware profile
/// doesn't matter.
pub fn empty_capabilities() -> NodeCapabilities {
    NodeCapabilities {
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
        origins: Vec::new(),
        media_allow: Vec::new(),
        media_available: None,
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: None,
    }
}

/// Build a `MemberRecord` with a specified `last_seen`. Use when the
/// test cares about the timestamp (e.g. gossip-decay scenarios).
pub fn member_with_last_seen(
    id: NodeId,
    name: &str,
    last_seen: u64,
    addr: SocketAddr,
) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: empty_capabilities(),
        addresses: vec![addr],
    }
}

/// Build a `MemberRecord` with `last_seen = 0`. The common case in
/// tests that don't exercise decay.
pub fn member(id: NodeId, name: &str, addr: SocketAddr) -> MemberRecord {
    member_with_last_seen(id, name, 0, addr)
}

/// Build a single-member `Mesh` rooted at `self_id`. The mesh_id is
/// 1 and the invite_key_hash is `[0x77; 32]` — neither matters for
/// tests that don't exercise the gossip auth boundary; for those
/// tests, construct the mesh inline with the right values.
pub fn solo_mesh(self_id: NodeId, name: &str) -> Mesh {
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member(self_id, "self", "127.0.0.1:9742".parse().unwrap()),
    );
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: name.into(),
        invite_key_hash: [0x77u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    }
}

/// Hex-encode a `NodeId` for the `X-Node-Id` header. 32 hex chars,
/// lowercase — matches `sovereign_contracts::principal::claimed_node_id`.
pub fn id_to_hex(id: &NodeId) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

// ── Router spawning ─────────────────────────────────────────────

/// Bind `router` on `127.0.0.1:0` and return the bound address. The
/// listener is wired with `into_make_service_with_connect_info::<SocketAddr>()`
/// so the loopback guard middleware (which fail-closes on absent
/// ConnectInfo) sees the production listener shape.
pub async fn spawn_router(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    // 20ms is enough headroom on every CI box we use; the tokio
    // accept-loop is ready well before reqwest's first connect.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}
