// SPDX-License-Identifier: AGPL-3.0-or-later
//! A standalone serve on a mesh (pb-serve-distributes-standalone): no svrn
//! daemon in its process, so it reads the mesh from cw-rails, the node's one
//! mesh endpoint, and never dials a peer itself.
//!
//! - [`RailsRoster`] is THE roster reader over cw-rails' `GET /v1/mesh/status`
//!   (the `MembershipReader` port pb-serve-ranks and pb-mesh-exit-transport
//!   reuse).
//! - [`spawn_registrations`] puts serve's origins in cw-rails' origin table
//!   (rpc, `/internal/v1/models/*`; phase-b-19, and the
//!   member client on `cwth/client/0`, pb-serve-ranks), through the one
//!   register/renew loop, so the flip turns none of them off.
//!
//! - [`RailsVenues`] is what a standalone serve ranks: that roster's peers,
//!   reached through cw-rails' reach door (pb-serve-ranks).
//!
//! The stock binary hands serve the daemon's own mesh until the flip
//! (phase-b-33) and runs none of this.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kernel_types::NodeId;
use mesh_reach::rails::RailsTransport;
use mesh_reach::{PeerContact, PeerTransport};
use oicp_types::capabilities::{
    AnchorProfile, AvailableResources, ComputeType, GpuInfo, HardwareProfile, NodeCapabilities,
};
use oicp_types::origin::{Admit, Framing, OriginRegistration};
use oicp_types::FederatedMeshDescriptor;
use serde::Deserialize;
use sovereign_contracts::daemon_wire::mesh::MemberStatus;
use sovereign_contracts::membership::{MembershipEntry, MembershipReader};
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
pub const PEER_PREFIXES: [&str; 1] = ["/internal/v1/models"];

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
    /// The member's key, full hex; `None` from a cw-rails older than the
    /// field, or for a pre-identity member.
    #[serde(default)]
    node_pubkey: Option<String>,
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
            // The key names the member cw-rails' forwards carry; one that is
            // present but unreadable is refused like a bad id, never dropped.
            let node_pubkey = match m.node_pubkey.as_deref() {
                None => None,
                Some(h) => Some(kernel_types::NodePubkey::from_hex(h).ok_or_else(|| {
                    format!(
                        "cw-rails' roster names an unreadable key for {}: {h}",
                        m.name
                    )
                })?),
            };
            Ok(MembershipEntry {
                node_id,
                name: m.name,
                status: m.status,
                active: true,
                last_seen: m.last_seen,
                dialable,
                capabilities: m.capabilities,
                // cw-rails holds every overlay decision: the overlay
                // addresses are empty (its roster fills none), and a program
                // reaches the member through `RailsTransport`. The key is the
                // member's public name, never a dial input here.
                dial: PeerContact {
                    node_id,
                    addresses: Vec::new(),
                    node_pubkey,
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
            .map_err(|e| unanswered(&url, &e))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("cw-rails refused {url}: {status}: {body}"));
        }
        let doc: StatusDoc = resp.json().await.map_err(|e| {
            if e.is_timeout() {
                unanswered(&url, &e)
            } else {
                format!("cw-rails' roster at {url} is a shape this build cannot read: {e}")
            }
        })?;
        parse(doc)
    }
}

/// Why cw-rails gave no roster: slow (it holds the port and did not answer
/// inside [`STATUS_TIMEOUT`], the one bound on a roster read) or absent
/// (nothing answers). Never an empty roster (F13, principle 6).
fn unanswered(url: &str, e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("cw-rails slow: no roster within {STATUS_TIMEOUT:?} at {url}")
    } else if e.is_connect() {
        format!("cw-rails absent: nothing answers at {url}: {e}")
    } else {
        format!("cw-rails did not answer at {url}: {e}")
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

    async fn read_roster(&self) -> Result<(String, Vec<MembershipEntry<PeerContact>>), String> {
        let reading = self.read().await;
        if let Err(e) = &reading {
            debug!(target: TARGET, error = %e, "rails roster: no answer, named");
        }
        reading.map(|r| (r.mesh_name, r.members))
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

/// cw-rails' roster and reach door at `rails_base`, as the ports a program
/// that is not the mesh endpoint reads the mesh through: the stock binary
/// hands these to svrn's daemon (pb-mesh-exit-transport, phase-b-80 fork 1).
pub fn rails_ports(
    rails_base: &str,
) -> (
    Arc<dyn MembershipReader<Dial = PeerContact>>,
    Arc<dyn PeerTransport>,
) {
    let roster = RailsRoster::new(rails_base);
    let transport: Arc<dyn PeerTransport> = Arc::new(RailsTransport::new(roster.base()));
    (Arc::new(roster), transport)
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
        let members =
            sovereign_contracts::membership::inference_peers(reading.members, reading.self_id);
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

/// serve's origins as cw-rails' origin table takes them: the model-transfer
/// prefix on `cwth/http/0` at serve's listener, its member
/// client (`crate::openai_face`) whole on `cwth/client/0` at that
/// listener (`member`), where the Inference and StatusProbe classes arrive,
/// and, when
/// this node lends a GPU (`SOVEREIGN_RPC_SERVE` names a bind), its ggml rpc
/// worker on `cwth/rpc/0`, declaring the anchor record peers' discovery reads.
/// The `cwth/http/0` one declares the loader's VRAM figure ([`vram_claims`]).
pub fn registrations(listen: SocketAddr, member: Option<SocketAddr>) -> Vec<OriginRegistration> {
    registrations_for(
        listen,
        member,
        sovereign_contracts::launch::RpcServe::from_env(),
        local_vram_gb(),
    )
}

/// This node's GPU memory in whole GB from the published GPU probe
/// (`gpu_probe`, taken at [`join`]). Never spawns. `Ok(None)` is a host with
/// no GPU; `Err` is unknown, never 0 GB.
fn local_vram_gb() -> Result<Option<u32>, String> {
    match sovereign_inference::gpu_probe::published() {
        Some(seen) => seen.total_vram_gb().map_err(str::to_string),
        None => Err("no GPU probe has been published in this process".to_string()),
    }
}

fn registrations_for(
    listen: SocketAddr,
    member: Option<SocketAddr>,
    rpc: sovereign_contracts::launch::RpcServe,
    vram_gb: Result<Option<u32>, String>,
) -> Vec<OriginRegistration> {
    let mut out = vec![OriginRegistration {
        alpn: String::from_utf8_lossy(mesh_reach::alpn::ALPN).into_owned(),
        prefixes: PEER_PREFIXES.iter().map(|p| p.to_string()).collect(),
        port: listen.port(),
        admit: Admit::Members(Vec::new()),
        framing: Framing::Http,
        ttl_secs: Some(ORIGIN_TTL_SECS),
        claims: Some(vram_claims(vram_gb.clone().ok().flatten())),
        namespaces: Vec::new(),
    }];
    match member {
        Some(member) => out.push(OriginRegistration {
            alpn: String::from_utf8_lossy(mesh_reach::alpn::CLIENT_ALPN).into_owned(),
            // `cwth/client/0` is registered whole: cw-rails takes prefixes on
            // `cwth/http/0` only.
            prefixes: Vec::new(),
            port: member.port(),
            // A member reaches serve's member client; a non-member dialling the
            // same protocol goes to the guest door svrn registers on
            // `GUEST_ALPN` (pb-mesh-exit-transport), whose auth reads its bearer.
            admit: Admit::MembersElse(
                String::from_utf8_lossy(mesh_reach::alpn::GUEST_ALPN).into_owned(),
            ),
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
            claims: anchor_claims(port, rpc.binds_past_loopback(), vram_gb),
            namespaces: Vec::new(),
        }),
        _ => {
            debug!(target: TARGET, "no rpc worker bind: serve lends no GPU and registers no rpc origin")
        }
    }
    out
}

/// The VRAM figure the shipped llama-server reports ([`local_vram_gb`]),
/// declared as the one GPU entry cw-rails applies over the
/// VRAM it measured (phase-b-90 (B)): sysfs sees only the carveout on a
/// unified-memory APU. No figure, no entry; every other number is zero.
fn vram_claims(vram_gb: Option<u32>) -> NodeCapabilities {
    let mut claims = zero_claims();
    claims.hardware.gpus = vram_gb
        .map(|vram_gb| GpuInfo {
            name: "GPU".to_string(),
            vram_gb,
            compute_type: ComputeType::Vulkan,
            estimated_tflops: 0.0,
        })
        .into_iter()
        .collect();
    claims
}

/// The anchor record this node declares through its rpc registration, which
/// cw-rails merges into its gossiped capabilities (`origins::merge_declared`).
/// Every other number is zero, as cw-rails' own report is: this declaration
/// claims the anchor tier and nothing about inference. An unknown VRAM
/// figure declares no anchor rather than a CPU-only one.
fn anchor_claims(
    rpc_port: u16,
    rpc_direct: bool,
    vram_gb: Result<Option<u32>, String>,
) -> Option<NodeCapabilities> {
    let vram_gb = match vram_gb {
        // `0` for a CPU-only anchor, the field's documented reading.
        Ok(vram_gb) => vram_gb.unwrap_or(0),
        Err(why) => {
            warn!(target: TARGET, %why, "GPU memory unknown: the rpc origin declares no anchor record");
            return None;
        }
    };
    Some(NodeCapabilities {
        anchor: Some(AnchorProfile {
            can_anchor: true,
            vram_gb,
            model_resident: sovereign_contracts::launch::SharedModelFleet::from_env()
                .model_id()
                .map(str::to_string),
            rpc_port: Some(rpc_port),
            // cw-rails forwards `cwth/rpc/0` to the worker: a host bridges to
            // it when no direct address answers.
            rpc_iroh: true,
            // Only a bind past loopback (plaintext LAN, operator-allowed) is
            // reachable at this node's own address; any other worker is
            // reached over the bridge, which proves the member.
            rpc_direct,
        }),
        ..zero_claims()
    })
}

/// A declaration with every number zero and no anchor.
fn zero_claims() -> NodeCapabilities {
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
        anchor: None,
        storage_remaining_bytes: None,
    }
}

/// serve on the node's mesh, through cw-rails alone (pb-serve-distributes-
/// standalone): the member client on a loopback port of its own, and serve's
/// origins in cw-rails' origin table. The standalone serve and the stock
/// binary's hosted serve both run this (pb-mesh-exit-transport), so a stock
/// node offers members what a standalone serve does. With no cw-rails
/// answering, the registrations retry; the OpenAI wire serves this node's own
/// models.
pub async fn join(
    rails_base: &str,
    listen: SocketAddr,
    local: Arc<dyn sovereign_contracts::InferenceProvider>,
) {
    // The member client on a loopback port of its own, registered whole on
    // cw-rails' `cwth/client/0`: a member's router reaches this node's
    // models there and nothing else of serve's. Without it serve still
    // serves this host; members see no models here.
    let member_addr = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(member) => match member.local_addr() {
            Ok(addr) => {
                info!(target: TARGET, member = %addr, "member client listening for cw-rails' forwards");
                tokio::spawn(host_kit::shell::serve(
                    [member],
                    vec![crate::openai_face(local), member_client_absence()],
                    std::future::pending(),
                ));
                Some(addr)
            }
            Err(e) => {
                tracing::error!(target: TARGET, error = %e, "the member client's port is unreadable; members reach no model here");
                None
            }
        },
        Err(e) => {
            tracing::error!(target: TARGET, error = %e, "the member client could not bind; members reach no model here");
            None
        }
    };
    // The GPU probe, once at startup, before the registrations read it.
    match tokio::task::spawn_blocking(sovereign_inference::gpu_probe::observe).await {
        Ok(seen) => info!(target: TARGET, vram_gb = ?seen.total_vram_gb(), "GPU probe published"),
        Err(e) => {
            tracing::error!(target: TARGET, error = %e, "the GPU probe panicked; GPU memory reads as unknown")
        }
    }
    spawn_registrations(rails_base, listen, member_addr);
}

/// What a member reached on `cwth/client/0` until the flip
/// (pb-mesh-exit-transport), when that protocol led to svrn's whole client
/// router, and does not reach on serve's member client now. A trailing `/`
/// is a prefix.
pub const MEMBER_CLIENT_LOST: [&str; 5] = [
    "/v1/responses",
    "/v1/knowledge/search",
    "/status",
    "/api/",
    "/oicp/v1/corpus/",
];

/// The member client's answer for every path `crate::openai_face` does not
/// serve: 410 for one a member reached before the flip, 404 otherwise, both
/// naming what the member client serves, so a member reads why and not a
/// bare 404 (FIVE_PROGRAMS §4 rule 3).
fn member_client_absence() -> host_kit::shell::RouteBundle {
    host_kit::shell::RouteBundle::new("serve_member_absence").fallback(member_absence)
}

async fn member_absence(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = uri.path();
    let lost = MEMBER_CLIENT_LOST
        .iter()
        .any(|p| match p.strip_suffix('/') {
            Some(prefix) => path.starts_with(*p) || path == prefix,
            None => path == *p,
        });
    debug!(target: TARGET, %path, lost, "member client: a path it does not serve");
    let serves = "serve's member client answers a member with this node's models only: \
                  /v1/chat/completions, /v1/embeddings, /v1/completions, /v1/models and \
                  /oicp/v1/capabilities";
    let (status, error) = if lost {
        (
            axum::http::StatusCode::GONE,
            format!(
                "{path} is not served to mesh members since the flip \
                 (pb-mesh-exit-transport): {serves}. Knowledge between members travels \
                 as /internal/knowledge/search on cwth/http/0"
            ),
        )
    } else {
        (
            axum::http::StatusCode::NOT_FOUND,
            format!("{path} is not a member route: {serves}"),
        )
    };
    (status, axum::Json(serde_json::json!({ "error": error }))).into_response()
}

/// Keep every registration in cw-rails' origin table for as long as serve
/// runs, through the one register/renew loop. The `cwth/http/0` one
/// re-reads the published VRAM figure at every register and renew.
pub fn spawn_registrations(rails_base: &str, listen: SocketAddr, member: Option<SocketAddr>) {
    let http = String::from_utf8_lossy(mesh_reach::alpn::ALPN).into_owned();
    for registration in registrations(listen, member) {
        info!(target: TARGET, slot = %registration.alpn, prefixes = ?registration.prefixes,
              port = registration.port, rails = %rails_base,
              "registering an origin with cw-rails");
        let claims: Option<sovereign_turn_client::rails_origins::ClaimsSource> =
            (registration.alpn == http).then(|| {
                Arc::new(|| {
                    Box::pin(async {
                        let vram_gb = local_vram_gb();
                        debug!(target: TARGET, ?vram_gb, "declaring the shipped server's VRAM figure");
                        vram_claims(vram_gb.ok().flatten())
                    })
                        as std::pin::Pin<
                            Box<dyn std::future::Future<Output = NodeCapabilities> + Send>,
                        >
                }) as sovereign_turn_client::rails_origins::ClaimsSource
            });
        tokio::spawn(
            sovereign_turn_client::rails_origins::keep_registered_declaring(
                rails_base.to_string(),
                registration,
                ORIGIN_TTL_SECS,
                ORIGIN_RENEW_EVERY,
                None,
                claims,
            ),
        );
    }
}

#[cfg(test)]
#[path = "rails_mesh/tests.rs"]
mod tests;
