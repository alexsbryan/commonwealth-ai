// SPDX-License-Identifier: AGPL-3.0-or-later
//! A standalone serve on a mesh (pb-serve-distributes-standalone): no svrn
//! daemon in its process, so it reads the mesh from cw-rails, the node's one
//! mesh endpoint, and never dials a peer itself.
//!
//! - [`RailsRoster`] is THE roster reader over cw-rails' `GET /v1/mesh/status`
//!   (the `MembershipReader` port pb-serve-ranks and pb-mesh-exit-transport
//!   reuse).
//! - [`mesh_ports`] builds the `MeshPorts` compute's distribution runs over:
//!   that roster, `mesh_reach::rails::RailsTransport` for every peer dial,
//!   serve's own listener as the model origin.
//! - [`spawn_registrations`] puts serve's origins in cw-rails' origin table
//!   (rpc, `/internal/v1/models/*`, `/internal/rpc-warm`; phase-b-19, and the
//!   member client on `cwth/client/0`, pb-serve-ranks), through the one
//!   register/renew loop, so the flip turns none of them off.
//!
//! - [`RailsVenues`] is what a standalone serve ranks: that roster's peers,
//!   reached through cw-rails' reach door (pb-serve-ranks).
//!
//! The stock binary hands serve the daemon's own mesh until the flip
//! (phase-b-33) and runs none of this.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kernel_types::NodeId;
use mesh_reach::rails::RailsTransport;
use mesh_reach::{PeerContact, PeerTransport};
use oicp_types::capabilities::{
    AnchorProfile, AvailableResources, HardwareProfile, NodeCapabilities,
};
use oicp_types::origin::{Admit, Framing, OriginRegistration};
use oicp_types::FederatedMeshDescriptor;
use serde::Deserialize;
use sovereign_contracts::daemon_wire::mesh::MemberStatus;
use sovereign_contracts::membership::{MembershipEntry, MembershipReader};
use sovereign_serving_host::rpc_discovery::{MeshNow, MeshPorts, ModelOrigin, RpcWorkerDiscovery};
use tracing::{debug, info, warn};

/// The trace target of every event here.
const TARGET: &str = "serve";

/// The roster route's ceiling: loopback coordination, slower is unreachable.
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

/// An origin claim's TTL, and how often it is renewed (the work origin's
/// cadence: three renews per TTL).
const ORIGIN_TTL_SECS: u64 = 60;
const ORIGIN_RENEW_EVERY: Duration = Duration::from_secs(ORIGIN_TTL_SECS / 3);

/// The HTTP paths serve answers for peers, registered on `cwth/http/0`.
pub const PEER_PREFIXES: [&str; 2] = ["/internal/v1/models", RPC_WARM_PATH];

/// The worker side of the shard warm (`crate::rpc_warm`).
pub const RPC_WARM_PATH: &str = "/internal/rpc-warm";

#[derive(Deserialize)]
struct StatusDoc {
    #[serde(rename = "self")]
    me: SelfDoc,
    mesh: MeshDoc,
    #[serde(default)]
    members: Vec<MemberDoc>,
}

#[derive(Deserialize)]
struct SelfDoc {
    #[serde(default)]
    node_id_hex: Option<String>,
}

#[derive(Deserialize)]
struct MeshDoc {
    name: String,
}

#[derive(Deserialize)]
struct MemberDoc {
    name: String,
    #[serde(default)]
    node_id_hex: Option<String>,
    status: MemberStatus,
    #[serde(default)]
    last_seen: u64,
    capabilities: NodeCapabilities,
    #[serde(default)]
    dial: DialDoc,
}

#[derive(Deserialize, Default)]
struct DialDoc {
    #[serde(default)]
    relay_url: Option<String>,
    #[serde(default)]
    iroh_direct_addrs: Vec<SocketAddr>,
}

/// One reading of cw-rails' roster.
#[derive(Debug, Clone)]
pub struct RosterReading {
    pub mesh_name: String,
    /// cw-rails' own node id: this node's identity on the mesh.
    pub self_id: NodeId,
    pub members: Vec<MembershipEntry<PeerContact>>,
}

/// Why a roster reading has no answer. Each is named, never an empty roster.
fn parse(doc: StatusDoc) -> Result<RosterReading, String> {
    let hex = |h: &Option<String>, who: &str| {
        h.as_deref().and_then(NodeId::from_hex).ok_or_else(|| {
            format!(
                "cw-rails' roster names no full id for {who} (`node_id_hex`): a cw-rails older \
                 than this serve"
            )
        })
    };
    let self_id = hex(&doc.me.node_id_hex, "itself")?;
    let members = doc
        .members
        .into_iter()
        .map(|m| {
            let node_id = hex(&m.node_id_hex, &m.name)?;
            // cw-rails lists live members only (`removed_at` is none), and a
            // member is dialable when cw-rails holds any iroh path to it: a
            // relay or a direct address (the rule `MemberRecord::is_dialable`
            // applies once the key is known, and cw-rails lists no member
            // without one).
            let dialable = m.dial.relay_url.is_some() || !m.dial.iroh_direct_addrs.is_empty();
            Ok(MembershipEntry {
                node_id,
                name: m.name,
                status: m.status,
                active: true,
                last_seen: m.last_seen,
                dialable,
                capabilities: m.capabilities,
                // cw-rails holds the key and every overlay decision: the
                // overlay addresses are empty (its roster fills none), and a
                // program reaches the member through `RailsTransport`.
                dial: PeerContact {
                    node_id,
                    addresses: Vec::new(),
                    node_pubkey: None,
                    relay_url: m.dial.relay_url,
                    iroh_direct_addrs: m.dial.iroh_direct_addrs,
                },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(RosterReading {
        mesh_name: doc.mesh.name,
        self_id,
        members,
    })
}

/// The roster as cw-rails serves it on `GET /v1/mesh/status`, read through
/// on every call: cw-rails is the owner, and a cached roster would be a
/// second one.
#[derive(Debug, Clone)]
pub struct RailsRoster {
    base: String,
    http: reqwest::Client,
}

impl RailsRoster {
    /// `base` is cw-rails' loopback API, e.g. `http://127.0.0.1:9747`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(STATUS_TIMEOUT)
                .build()
                .expect("a plain reqwest client"),
        }
    }

    /// cw-rails' API base this roster reads.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Read the roster once. `Err` names cw-rails' absence, its refusal, or a
    /// roster this build cannot read.
    pub async fn read(&self) -> Result<RosterReading, String> {
        let url = format!("{}/v1/mesh/status", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("cw-rails did not answer at {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("cw-rails refused {url}: {status}: {body}"));
        }
        let doc: StatusDoc = resp.json().await.map_err(|e| {
            format!("cw-rails' roster at {url} is a shape this build cannot read: {e}")
        })?;
        parse(doc)
    }
}

#[async_trait]
impl MembershipReader for RailsRoster {
    type Dial = PeerContact;

    async fn mesh_name(&self) -> String {
        match self.read().await {
            Ok(r) => r.mesh_name,
            Err(e) => {
                debug!(target: TARGET, error = %e, "rails roster: no mesh name");
                String::new()
            }
        }
    }

    /// cw-rails serves no federation list; serve advertises none.
    async fn federated_meshes(&self) -> Vec<FederatedMeshDescriptor> {
        Vec::new()
    }

    async fn members(&self) -> Vec<MembershipEntry<PeerContact>> {
        match self.read().await {
            Ok(r) => r.members,
            Err(e) => {
                // The port cannot carry the error; the trace names it, and the
                // mesh reader below answers `None` for the same failure, so a
                // discovery tick reads it as "scanned nothing".
                debug!(target: TARGET, error = %e, "rails roster: unreadable, no members");
                Vec::new()
            }
        }
    }
}

/// The mesh ports a standalone serve hands its distribution: cw-rails'
/// roster, `RailsTransport` over cw-rails' reach door, and serve's own
/// listener (`listen`) as the model origin. A cw-rails that does not answer
/// makes every tick "the mesh is not up" (warned once, then `debug`).
pub fn mesh_ports(roster: RailsRoster, listen: SocketAddr) -> MeshPorts {
    let transport: Arc<dyn PeerTransport> = Arc::new(RailsTransport::new(roster.base()));
    let reader = Arc::new(roster);
    let told = Arc::new(AtomicBool::new(false));
    let origin_base = format!("http://{}", origin_addr(listen));
    MeshPorts {
        mesh: Arc::new(move || {
            let reader = Arc::clone(&reader);
            let transport = Arc::clone(&transport);
            let told = Arc::clone(&told);
            Box::pin(async move {
                match reader.read().await {
                    Ok(r) => {
                        told.store(false, Ordering::Relaxed);
                        debug!(target: TARGET, mesh = %r.mesh_name, me = %r.self_id.to_hex(),
                               members = r.members.len(), "rails roster read");
                        Some(MeshNow {
                            roster: reader as Arc<dyn MembershipReader<Dial = PeerContact>>,
                            transport,
                            self_id: r.self_id,
                        })
                    }
                    Err(e) if !told.swap(true, Ordering::Relaxed) => {
                        warn!(target: TARGET, error = %e,
                              "no mesh: cw-rails' roster is unreadable, so serve distributes \
                               nothing until it answers");
                        None
                    }
                    Err(e) => {
                        debug!(target: TARGET, error = %e, "no mesh: cw-rails' roster still unreadable");
                        None
                    }
                }
            })
        }),
        on_host_role: Arc::new(|am_host| {
            info!(target: TARGET, am_host, "shared-model host role");
        }),
        discovery: Arc::new(RpcWorkerDiscovery::default()),
        // Peers fetch model files through cw-rails (`/internal/v1/models` is
        // registered on `cwth/http/0`); this raw base is the fallback a
        // worker on this host reaches directly.
        model_origin: Arc::new(move || {
            let base = origin_base.clone();
            Box::pin(async move {
                ModelOrigin {
                    internal_port: listen.port(),
                    bases: vec![base],
                }
            })
        }),
        // No mesh credential: serve holds no mesh key. A peer's request
        // reaches serve through cw-rails, which admits members only and
        // forwards the verified identity with the origin's tie.
        proof: Arc::new(|| Box::pin(async { None })),
    }
}

/// The venues a standalone serve ranks, and the host its router asks: cw-rails'
/// roster (the one reader, [`RailsRoster`]), each peer reached through
/// cw-rails' reach door on the Inference class, which lands on the peer's
/// member client (`cwth/client/0`). Which members, and each as a venue, is
/// the one decision the svrn daemon's roster applies too
/// (`membership::inference_peers`, `MembershipEntry::inference_venue`).
pub struct RailsVenues {
    roster: RailsRoster,
    transport: RailsTransport,
}

impl RailsVenues {
    pub fn new(roster: RailsRoster) -> Self {
        let transport = RailsTransport::new(roster.base());
        Self { roster, transport }
    }
}

#[async_trait]
impl sovereign_contracts::venue::VenueSource for RailsVenues {
    async fn candidates(&self) -> Vec<sovereign_contracts::venue::InferenceVenue> {
        let reading = match self.roster.read().await {
            Ok(r) => r,
            Err(e) => {
                // The port carries no error; a router reads "no peers" and
                // serves locally, and the trace names why.
                debug!(target: TARGET, error = %e, "rails venues: roster unreadable, no peer venues");
                return Vec::new();
            }
        };
        let members = sovereign_contracts::membership::inference_peers(reading.members, reading.self_id);
        let mut venues = Vec::with_capacity(members.len());
        for m in members {
            let base_urls: Vec<String> = self
                .transport
                .endpoints(&m.dial, mesh_reach::TrafficClass::Inference)
                .await
                .into_iter()
                .map(|ep| format!("{}/v1", ep.base_url))
                .collect();
            venues.push(m.inference_venue(base_urls));
        }
        debug!(target: TARGET, venues = venues.len(), "rails venues: peers from cw-rails' roster");
        venues
    }
}

#[async_trait]
impl sovereign_contracts::venue_host::VenueHost for RailsVenues {
    /// cw-rails' own id: this node's identity on the mesh.
    async fn local_node_id(&self) -> Option<NodeId> {
        match self.roster.read().await {
            Ok(r) => Some(r.self_id),
            Err(e) => {
                debug!(target: TARGET, error = %e, "rails venues: no node id, cw-rails' roster is unreadable");
                None
            }
        }
    }

    /// serve holds no contribution ledger: the fact is not recorded, and
    /// says so.
    async fn ledger_emitter(
        &self,
    ) -> Option<Arc<dyn sovereign_contracts::venue_host::LedgerEmitter>> {
        debug!(target: TARGET, "rails venues: no contribution ledger here, InferenceReceived is not recorded");
        None
    }
}

/// Where a peer on this host reaches serve's listener: an unspecified bind
/// is reached on loopback.
fn origin_addr(listen: SocketAddr) -> SocketAddr {
    if listen.ip().is_unspecified() {
        SocketAddr::from(([127, 0, 0, 1], listen.port()))
    } else {
        listen
    }
}

/// serve's origins as cw-rails' origin table takes them: the model-transfer
/// and rpc-warm prefixes on `cwth/http/0` at serve's listener, its member
/// client (`crate::openai_face`) whole on `cwth/client/0` at that
/// listener (`member`), where the Inference and StatusProbe classes arrive,
/// and, when
/// this node lends a GPU (`SOVEREIGN_RPC_SERVE` names a bind), its ggml rpc
/// worker on `cwth/rpc/0`, declaring the anchor record peers' discovery reads.
pub fn registrations(listen: SocketAddr, member: Option<SocketAddr>) -> Vec<OriginRegistration> {
    registrations_for(
        listen,
        member,
        sovereign_contracts::launch::RpcServe::from_env(),
    )
}

fn registrations_for(
    listen: SocketAddr,
    member: Option<SocketAddr>,
    rpc: sovereign_contracts::launch::RpcServe,
) -> Vec<OriginRegistration> {
    let mut out = vec![OriginRegistration {
        alpn: String::from_utf8_lossy(mesh_reach::alpn::ALPN).into_owned(),
        prefixes: PEER_PREFIXES.iter().map(|p| p.to_string()).collect(),
        port: listen.port(),
        admit: Admit::Members(Vec::new()),
        framing: Framing::Http,
        ttl_secs: Some(ORIGIN_TTL_SECS),
        claims: None,
        namespaces: Vec::new(),
    }];
    match member {
        Some(member) => out.push(OriginRegistration {
            alpn: String::from_utf8_lossy(mesh_reach::alpn::CLIENT_ALPN).into_owned(),
            // `cwth/client/0` is registered whole: cw-rails takes prefixes on
            // `cwth/http/0` only.
            prefixes: Vec::new(),
            port: member.port(),
            admit: Admit::Members(Vec::new()),
            framing: Framing::Http,
            ttl_secs: Some(ORIGIN_TTL_SECS),
            claims: None,
            namespaces: Vec::new(),
        }),
        None => warn!(target: TARGET, "no member client: serve registers nothing on cwth/client/0"),
    }
    match rpc.port() {
        Some(port) if rpc.is_serving() => out.push(OriginRegistration {
            alpn: String::from_utf8_lossy(mesh_reach::alpn::RPC_ALPN).into_owned(),
            prefixes: Vec::new(),
            port,
            admit: Admit::Members(Vec::new()),
            // ggml's rpc-server speaks raw bytes, not HTTP.
            framing: Framing::Bytes,
            ttl_secs: Some(ORIGIN_TTL_SECS),
            claims: Some(anchor_claims(port)),
            namespaces: Vec::new(),
        }),
        _ => {
            debug!(target: TARGET, "no rpc worker bind: serve lends no GPU and registers no rpc origin")
        }
    }
    out
}

/// The anchor record this node declares through its rpc registration, which
/// cw-rails merges into its gossiped capabilities (`origins::merge_declared`).
/// Every other number is zero, as cw-rails' own report is: this declaration
/// claims the anchor tier and nothing about inference.
fn anchor_claims(rpc_port: u16) -> NodeCapabilities {
    NodeCapabilities {
        hardware: HardwareProfile {
            gpus: Vec::new(),
            system_ram_gb: 0,
            cpu_cores: 0,
            total_storage_gb: 0,
            free_storage_gb: 0,
            network_bandwidth_mbps: None,
        },
        available: AvailableResources {
            free_vram_gb: 0.0,
            free_ram_gb: 0.0,
            free_storage_gb: 0.0,
            gpu_utilization: 0.0,
            cpu_utilization: 0.0,
            available_for_mesh: false,
        },
        active_processes: Vec::new(),
        hosted_corpora: Vec::new(),
        reported_at: 0,
        inference_availability: 0.0,
        inference_capable: false,
        loaded_models: Vec::new(),
        origins: Vec::new(),
        media_allow: Vec::new(),
        media_available: None,
        embed_model: None,
        benchmark: None,
        current_in_flight: None,
        anchor: Some(AnchorProfile {
            can_anchor: true,
            // `0` for a CPU-only anchor, the field's documented reading.
            vram_gb: sovereign_inference::embedded::local_gpu_total_vram_gb().unwrap_or(0),
            model_resident: sovereign_contracts::launch::SharedModelFleet::from_env()
                .model_id()
                .map(str::to_string),
            rpc_port: Some(rpc_port),
            // cw-rails forwards `cwth/rpc/0` to the worker: a host bridges to
            // it when no direct address answers.
            rpc_iroh: true,
        }),
    }
}

/// Keep every registration in cw-rails' origin table for as long as serve
/// runs, through the one register/renew loop.
pub fn spawn_registrations(rails_base: &str, listen: SocketAddr, member: Option<SocketAddr>) {
    for registration in registrations(listen, member) {
        info!(target: TARGET, slot = %registration.alpn, prefixes = ?registration.prefixes,
              port = registration.port, rails = %rails_base,
              "registering an origin with cw-rails");
        tokio::spawn(sovereign_turn_client::rails_origins::keep_registered(
            rails_base.to_string(),
            registration,
            ORIGIN_TTL_SECS,
            ORIGIN_RENEW_EVERY,
        ));
    }
}

#[cfg(test)]
#[path = "rails_mesh/tests.rs"]
mod tests;
