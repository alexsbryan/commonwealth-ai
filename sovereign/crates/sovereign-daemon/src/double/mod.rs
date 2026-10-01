// SPDX-License-Identifier: AGPL-3.0-or-later
//! The records the daemon's seeds take, built for a test that drives a
//! daemon route (pb-serve-ranks-tests-stock): roster members, a roster
//! reader, the bound router and a recording ledger. Behind `test-doubles`, so
//! a composition root's tests (sovereign-stock) build them without naming
//! commonwealth-core; the daemon's own test tree re-exports them from
//! `tests/main/common`.
//!
//! The roster is cw-rails' after the flip (pb-mesh-exit-transport), read
//! through the membership port, so a test hands the daemon a
//! [`StaticRoster`] where it used to hand it a `Mesh`.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;

use kernel_types::{NodeId, NodePubkey};
use mesh_reach::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};
use oicp_types::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use oicp_types::FederatedMeshDescriptor;
use sovereign_contracts::daemon_wire::mesh::MemberStatus;
use sovereign_contracts::membership::{MembershipEntry, MembershipReader};

pub mod ledger_double;

// ── Capabilities + member helpers ───────────────────────────────

/// A `NodeCapabilities` with every field zeroed / empty. Useful for
/// constructing test members where the hardware profile doesn't matter.
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
        storage_remaining_bytes: None,
    }
}

/// An online, active, dialable roster member with no key, as cw-rails'
/// roster lists one (`last_seen = 0`).
pub fn member(id: NodeId, name: &str) -> MembershipEntry<PeerContact> {
    MembershipEntry {
        node_id: id,
        name: name.into(),
        status: MemberStatus::Online,
        active: true,
        last_seen: 0,
        dialable: true,
        capabilities: empty_capabilities(),
        dial: PeerContact {
            node_id: id,
            addresses: Vec::new(),
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
        },
    }
}

/// [`member`], signing with `key`: the verified key cw-rails forwards as
/// `X-Mesh-Pubkey`.
pub fn keyed_member(id: NodeId, name: &str, key: [u8; 32]) -> MembershipEntry<PeerContact> {
    let mut m = member(id, name);
    m.dial.node_pubkey = Some(NodePubkey(key));
    m
}

/// A roster row for `id` in `status`, advertising `capabilities`, dialled at
/// `addresses` — [`AddressTransport`] dials them directly, as a test's bound
/// routers need.
pub fn peer_row(
    id: NodeId,
    name: &str,
    status: MemberStatus,
    capabilities: NodeCapabilities,
    addresses: Vec<SocketAddr>,
) -> MembershipEntry<PeerContact> {
    let mut row = member(id, name);
    row.status = status;
    row.capabilities = capabilities;
    row.dial.addresses = addresses;
    row
}

/// A roster reader over a list the test owns and may change mid-test.
pub struct StaticRoster {
    name: String,
    members: RwLock<Vec<MembershipEntry<PeerContact>>>,
}

impl StaticRoster {
    /// A roster named `name` holding `members`.
    pub fn new(name: &str, members: Vec<MembershipEntry<PeerContact>>) -> Self {
        Self {
            name: name.into(),
            members: RwLock::new(members),
        }
    }

    /// Replace the members, as a roster change cw-rails would report.
    pub fn set(&self, members: Vec<MembershipEntry<PeerContact>>) {
        *self.members.write().unwrap_or_else(|e| e.into_inner()) = members;
    }

    /// Add `member`, replacing the row that carries its node id.
    pub fn insert(&self, member: MembershipEntry<PeerContact>) {
        let mut rows = self.members.write().unwrap_or_else(|e| e.into_inner());
        rows.retain(|m| m.node_id != member.node_id);
        rows.push(member);
    }
}

/// Make `state` believe a request carrying `tie` in
/// `kernel_types::member::ORIGIN_TIE_HEADER` is cw-rails' forward, as the
/// register/renew loop does once cw-rails holds svrn's peer-origin
/// registration (`crate::peer_origin`). Keep the sender for the test's
/// length; a second call on one state installs nothing.
pub fn tie_as_cw_rails(
    state: &crate::state::AppState,
    tie: &str,
) -> tokio::sync::watch::Sender<Option<String>> {
    let (tx, rx) = tokio::sync::watch::channel(Some(tie.to_string()));
    let _ = state.inner.node.peer_origin_tie.install(rx);
    tx
}

#[async_trait]
impl MembershipReader for StaticRoster {
    type Dial = PeerContact;

    async fn mesh_name(&self) -> String {
        self.name.clone()
    }

    async fn federated_meshes(&self) -> Vec<FederatedMeshDescriptor> {
        Vec::new()
    }

    async fn members(&self) -> Vec<MembershipEntry<PeerContact>> {
        self.members
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A [`StaticRoster`] as the port a `FabricSeed` takes.
pub fn roster(
    name: &str,
    members: Vec<MembershipEntry<PeerContact>>,
) -> Arc<dyn MembershipReader<Dial = PeerContact>> {
    Arc::new(StaticRoster::new(name, members))
}

/// A transport that dials each roster row's `addresses` verbatim, as
/// `http://<addr>` — what a test's bound routers need. A seed's default
/// transport resolves no peer (`crate::fabric::TransportReader`'s default,
/// svrn composed with no mesh), and a production node reaches peers through
/// cw-rails' reach door, so this exists for tests only.
#[derive(Debug, Default)]
pub struct AddressTransport;

#[async_trait]
impl PeerTransport for AddressTransport {
    fn name(&self) -> &'static str {
        "addresses"
    }

    async fn endpoints(&self, peer: &PeerContact, _class: TrafficClass) -> Vec<PeerEndpoint> {
        peer.addresses
            .iter()
            .map(|addr| PeerEndpoint {
                base_url: format!("http://{addr}"),
                label: format!("addr:{addr}"),
            })
            .collect()
    }
}

/// [`AddressTransport`] as the reader a `FabricSeed` takes.
pub fn address_transport() -> crate::fabric::TransportReader {
    crate::fabric::TransportReader::new(Arc::new(AddressTransport))
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
