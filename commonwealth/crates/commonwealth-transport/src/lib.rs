// SPDX-License-Identifier: AGPL-3.0-or-later
//! One question, asked in one place: how does this node reach that peer?
//!
//! # What this is for
//!
//! Every part of a daemon that talks to a peer used to build its own URL —
//! `format!("http://{ip}:{port}/internal/gossip")`, and a different spelling
//! two files over. That works right up until "reach a peer" stops meaning "we
//! know its IP": a node behind a NAT with no VPN has no address anyone can
//! write into a string. This crate is the seam that makes that a
//! one-implementation change instead of a hundred call-site ones.
//!
//! A call site asks [`PeerTransport::endpoints`] for a [`PeerContact`] and a
//! [`TrafficClass`], and gets back ordered [`PeerEndpoint`]s to try in turn.
//! [`peer_contact`] is the one conversion from a
//! [`commonwealth_core::mesh::MemberRecord`] to what a transport
//! is allowed to see. Two implementations ship: [`IpTransport`], the overlay
//! path everything runs on today, and `iroh::IrohTransport` behind the `iroh`
//! feature, which dials by Ed25519 key. [`RoutedTransport`] composes them per
//! class.
//!
//! [`identity`] is the sibling half: the node's own Ed25519 keypair, persisted
//! as a 32-byte seed at `<data_dir>/node_key`. That file is byte-for-byte a
//! valid iroh secret key, which is the point — verifying a peer's identity and
//! being able to dial it become the same fact rather than two systems that
//! have to agree.
//!
//! # Three decisions worth knowing before reading the code
//!
//! **The seam resolves an address and stops.** It hands back scheme and
//! authority — `http://100.64.0.2:9742` — and nothing else. Route paths stay
//! at call sites, and so do the `reqwest` clients and their per-class timeouts
//! (gossip 3s, status probe 800ms, inference 1800s). That narrowness is what
//! let the seam land under live traffic with the IP path provably unchanged:
//! there was no behaviour left in it to change. It also sets the altitude — the
//! transport is below the route layer, and a question that needs a path or a
//! body is being asked one layer too low.
//!
//! **[`TrafficClass`] is the migration order, and it is in the type system.**
//! The seven variants partition every peer conversation in the codebase.
//! Moving to a new transport is not a config flip and not a rewrite: it is
//! mapping one class to a different transport in a [`RoutedTransport`], whose
//! candidates are concatenated ahead of the default's. Gossip goes first,
//! blob transfer next, inference streaming last. Because call sites already
//! try candidates in order and stop at the first success, per-dial fallback to
//! the IP path is free — a failed dial degrades on the *same* request.
//!
//! **Opportunistic by default, fail-closed on demand.** That free fallback is
//! wrong when the mesh has declared itself encrypted, because degrading to the
//! IP path means degrading to plaintext. So a class can be named *required* at
//! construction ([`RoutedTransport::with_required`]), and a required class
//! whose encrypted transport yields no candidates returns none — the dial
//! fails rather than quietly downgrading. `Mesh::require_encryption` is what
//! puts every class in that set.
//!
//! # What a `PeerContact` deliberately does not carry
//!
//! Addresses, the Ed25519 key, and the iroh relay and direct hints. Not
//! capabilities, not status, not anything else on the member record — the
//! struct exists so that "which fields may influence dialing" is a decision
//! made once, in [`peer_contact`], rather than at every transport that could
//! have taken a `&MemberRecord` and read whatever it liked.
//!
//! # What is out of scope, and one of them is a real hole
//!
//! The loopback self-probe and worker-pod pinned-TLS endpoints are separate
//! trust models and stay separate. The join handshake used to be out of scope
//! too — pre-membership, no `PeerContact` yet — but an encrypted mesh now
//! joins over iroh: the invite carries the founder's dial string, so the join
//! secret never crosses plaintext. A plaintext mesh still joins over the IP
//! overlay.
//!
//! [`TrafficClass::RpcTensor`] is the hole and is worth saying plainly. It is
//! the raw ggml tensor-split byte stream between spawned `llama-server` /
//! `rpc-server` processes — third-party binaries speaking TCP, not HTTP, on
//! per-worker ports that are advertised rather than uniform. [`IpTransport`]
//! returns no candidates for it, discovery does its own probing, and on an
//! otherwise encrypted mesh **this is the one remaining plaintext path**.
//! Closing it needs a tunnel-proxy sidecar, which nobody has built.

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
    /// ggml tensor-split RPC byte stream to a worker's rpc-server
    /// (`worker:50052`) — raw TCP tunneled whole, NOT HTTP. Candidates
    /// for this class are bridge-local `127.0.0.1:<port>` authorities
    /// the caller strips the scheme from and hands to ggml verbatim.
    /// The IP transport returns NO candidates for it: RPC ports are
    /// per-worker (advertised via `/status`), not the uniform mesh
    /// ports, so the raw-TCP path stays with discovery's own probing.
    RpcTensor,
}

impl TrafficClass {
    /// Every traffic class, in flip order. Callers that must apply a
    /// policy to all peer traffic — e.g. routing every class over iroh
    /// when the mesh-wide encryption policy is on — enumerate this.
    pub const ALL: [TrafficClass; 7] = [
        TrafficClass::Gossip,
        TrafficClass::ControlPlane,
        TrafficClass::KnowledgeSearch,
        TrafficClass::ModelTransfer,
        TrafficClass::Inference,
        TrafficClass::StatusProbe,
        TrafficClass::RpcTensor,
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
            TrafficClass::RpcTensor => "rpc_tensor",
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
