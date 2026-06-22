// SPDX-License-Identifier: AGPL-3.0-or-later
//! The PeerTransport seam — how this node reaches a mesh peer.
//!
//! Today every peer conversation is HTTP to an IP the peer gossiped
//! (the Tailscale/LAN overlay provides reachability). This crate
//! exists so that fact lives in exactly one place: call sites ask
//! "give me dialable endpoints for this peer and this kind of
//! traffic" instead of `format!("http://{ip}:{port}/…")`. A future
//! transport that dials by node identity rather than by IP (e.g.
//! iroh — QUIC by Ed25519 key with hole-punching and relays) slots
//! in behind the same trait without touching call sites.
//!
//! Deliberate altitude: the transport resolves *(peer, traffic
//! class) → ordered candidate base URLs*. It does NOT own the HTTP
//! client — call sites keep their existing `reqwest` clients and
//! per-class timeouts (gossip 3s, status probe 800ms, inference
//! 1800s), which is what makes the IP path provably unchanged by
//! this seam. Route paths (`/internal/gossip`, `/v1`, …) also stay
//! at call sites: the transport is below the route layer.
//!
//! Migration order is encoded by [`TrafficClass`], not by config:
//! when a second transport arrives, a `RoutedTransport` that maps
//! classes to transports (gossip first, blob transfer next,
//! inference streaming last) implements [`PeerTransport`] and slots
//! into the same `Arc<dyn PeerTransport>` with zero call-site churn.
//! Until there are two transports, no such registry exists.
//!
//! Out of scope, by design: the loopback self-probe, worker-pod
//! pinned-TLS endpoints (separate trust model, already seamed via
//! `PinnedTransport`), and the raw TCP that spawned
//! `llama-server`/`rpc-server` processes speak to each other
//! (third-party binaries; they stay on the IP overlay until a
//! tunnel-proxy is worth building) — that tensor-split RPC is the sole
//! residual plaintext on an ENCRYPTED mesh.
//!
//! The join handshake was historically out of scope (pre-membership —
//! no `PeerContact` yet), but an encrypted mesh now joins over iroh:
//! the invite carries the founder's dial string and
//! `sovereign-mesh::join::perform_encrypted_join` dials by key, so the
//! join secret never crosses plaintext. A plaintext mesh still joins
//! over the IP overlay.

pub mod identity;
mod ip;
#[cfg(feature = "iroh")]
pub mod iroh;
mod routed;

pub use ip::IpTransport;
pub use routed::RoutedTransport;

use std::net::SocketAddr;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::MemberRecord;

/// One class of peer traffic. The variants partition every peer
/// conversation in the codebase; a transport may apply a different
/// port/path policy per class (see [`IpTransport`]) and a future
/// router may send different classes over different transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrafficClass {
    /// Member-list anti-entropy + mesh_store/app-state push
    /// (`/internal/gossip`, `/internal/app/state`).
    Gossip,
    /// Corpus queue/collaborate/ingest-partition, pipeline pause —
    /// internal-port control traffic.
    ControlPlane,
    /// `/internal/knowledge/search` fan-out.
    KnowledgeSearch,
    /// GGUF/model/shard pulls (`/internal/v1/models/*`) and
    /// rpc-warm pushes.
    ModelTransfer,
    /// Client-port `/v1` inference (chat completions, manifest).
    Inference,
    /// Client-port `/status` and `/oicp/v1/capabilities` probes.
    StatusProbe,
}

impl TrafficClass {
    /// Every traffic class, in flip order. Callers that must apply a
    /// policy to all peer traffic — e.g. routing every class over iroh
    /// when the mesh-wide encryption policy is on — enumerate this.
    pub const ALL: [TrafficClass; 6] = [
        TrafficClass::Gossip,
        TrafficClass::ControlPlane,
        TrafficClass::KnowledgeSearch,
        TrafficClass::ModelTransfer,
        TrafficClass::Inference,
        TrafficClass::StatusProbe,
    ];

    /// Stable lowercase name for tracing fields.
    pub fn as_str(&self) -> &'static str {
        match self {
            TrafficClass::Gossip => "gossip",
            TrafficClass::ControlPlane => "control_plane",
            TrafficClass::KnowledgeSearch => "knowledge_search",
            TrafficClass::ModelTransfer => "model_transfer",
            TrafficClass::Inference => "inference",
            TrafficClass::StatusProbe => "status_probe",
        }
    }
}

/// Everything a transport may need to reach a peer, extracted from
/// a `MemberRecord` via [`peer_contact`]. Keeping this a separate
/// struct (rather than passing `&MemberRecord`) means the trait's
/// surface names exactly the fields transports are allowed to rely
/// on — capabilities, status, and the rest of the record stay out
/// of transport decisions.
#[derive(Debug, Clone)]
pub struct PeerContact {
    pub node_id: NodeId,
    /// IP-overlay addresses exactly as gossiped (internal port).
    pub addresses: Vec<SocketAddr>,
    /// Ed25519 identity key (the future iroh node id). `None` for
    /// peers running pre-identity builds.
    pub node_pubkey: Option<NodePubkey>,
    /// iroh relay URL the peer gossiped (W2). With `node_pubkey` +
    /// `iroh_direct_addrs`, this is everything `IrohTransport` needs to
    /// dial the peer by key — no out-of-band seeding. `None` when the
    /// peer isn't iroh-reachable.
    pub relay_url: Option<String>,
    /// iroh direct (hole-punch / LAN) socket hints the peer gossiped.
    pub iroh_direct_addrs: Vec<SocketAddr>,
}

/// One dialable candidate for a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEndpoint {
    /// Scheme + authority only — no path, no trailing slash:
    /// `http://100.64.0.2:9742`, `http://[fd7a::1]:9741`. Call
    /// sites append their route path.
    pub base_url: String,
    /// Glassbox label for tracing: `ip:100.64.0.2:9742`,
    /// `iroh:127.0.0.1:54321→ab3f…`.
    pub label: String,
}

/// How this node reaches mesh peers. Implementations: [`IpTransport`]
/// (today's tailnet/LAN overlay); `IrohTransport` (feature-gated,
/// dial-by-key) when it lands.
#[async_trait::async_trait]
pub trait PeerTransport: Send + Sync + std::fmt::Debug + 'static {
    /// Short transport name for tracing ("ip", "iroh").
    fn name(&self) -> &'static str;

    /// Ordered dial candidates for `peer`, best first. Callers keep
    /// the existing contract: try in order, stop at the first
    /// success. Empty when the peer has no usable contact info.
    ///
    /// Async because identity-keyed transports may need to lazily
    /// establish a local bridge before an HTTP URL exists; the IP
    /// implementation never awaits.
    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint>;

    /// Feedback that `endpoint` worked for `peer` on `class`-traffic.
    /// Transports may use it to reorder future candidates (the IP
    /// transport promotes the last-working address to the front,
    /// absorbing what used to be gossip's process-global
    /// `last_working_address_cache`). Default: ignore.
    fn note_success(&self, _peer: NodeId, _class: TrafficClass, _endpoint: &PeerEndpoint) {}
}

/// Canonical `MemberRecord` → [`PeerContact`] conversion. Every call
/// site goes through this so the "which record fields may influence
/// dialing" decision is made once.
pub fn peer_contact(member: &MemberRecord) -> PeerContact {
    PeerContact {
        node_id: member.node_id,
        addresses: member.addresses.clone(),
        node_pubkey: member.node_pubkey,
        relay_url: member.relay_url.clone(),
        iroh_direct_addrs: member.iroh_direct_addrs.clone(),
    }
}
