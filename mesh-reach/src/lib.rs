// SPDX-License-Identifier: AGPL-3.0-or-later
//! How a program reaches a mesh peer: the dial vocabulary, and the one client
//! that asks cw-rails for it.
//!
//! A call site asks [`PeerTransport::endpoints`] for a [`PeerContact`] and a
//! [`TrafficClass`], and gets back ordered [`PeerEndpoint`]s to try in turn.
//! `commonwealth-transport` implements the port over the IP overlay and iroh
//! and re-exports every item here at its historical path. `RailsTransport`
//! (feature `rails`) implements it over cw-rails' `GET /v1/mesh/reach`, so a
//! program that is not the mesh endpoint dials peers through the one that is.
//!
//! This is a contract leaf that holds a port trait, which §12 3a rung 2 says
//! a leaf never does ("a port trait is never a leaf"). It is admitted anyway
//! (phase-b-30 Group 3) because svrn, serve and cmnwlth must share it and no
//! existing home can take it. The falsifier (phase-b-33): a workspace
//! dependency beyond kernel-types means this crate holds mechanism, not
//! vocabulary, and the leaf is wrong.

// Ask N peers the same question concurrently, one attributed row each.
// Compiled for this crate's own tests, whose dev-deps carry tokio and serde.
#[cfg(any(feature = "fanout", test))]
pub mod fanout;
// The reach door's wire, spoken by cw-rails and `RailsTransport`.
#[cfg(any(feature = "wire", test))]
pub mod door;
// `PeerTransport` over the reach door, for the programs that dial through
// cw-rails.
#[cfg(any(feature = "rails", test))]
pub mod rails;

use std::net::SocketAddr;

use kernel_types::{NodeId, NodePubkey};

/// One class of peer traffic. The variants partition every peer
/// conversation in the codebase; a transport may apply a different
/// port/path policy per class (see `IpTransport`) and a future
/// router may send different classes over different transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrafficClass {
    /// Member-list anti-entropy (`/internal/gossip`), and the ring
    /// journal's digest exchange (`/internal/ring/sync`) which shares its
    /// client and its timeout. The mesh_store snapshot push that used to ride
    /// this class was deleted at cw-lift rung 2e together with its route.
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
    /// A member's player reaching the peer's declared media origin
    /// (`[iroh] media_origin`, Jellyfin's `:8096` or any HTTP server
    /// honouring `Range`) — HTTP spliced whole through the bridge, never
    /// parsed. Candidates are bridge-local `127.0.0.1:<port>` URLs a
    /// player is pointed at verbatim. iroh-ONLY: the origin is bound to
    /// loopback on the holder and is reachable by mesh key alone, so the
    /// IP transport returns NO candidates — there is no port to guess,
    /// and a plaintext guess would be a hole rather than a fallback.
    Media,
    /// A member reaching one of a peer's PUBLISHED APPS (`[iroh.apps]`) —
    /// HTTP spliced whole, the app chosen per request by the first path
    /// segment. iroh-ONLY for the same reason `Media` is: the apps are bound
    /// to loopback on the publisher and reachable by mesh key alone, so a
    /// plaintext guess would be a hole rather than a fallback.
    App,
    /// A member reading a peer's OFFER origin (`[iroh] offer_origin`) — the
    /// HTTP listing of what that operator has to sell or lend. HTTP spliced
    /// whole, never parsed: what an offer IS stays the origin's, and the
    /// catalogue a house sees is computed by asking every publisher at once
    /// rather than stored anywhere.
    ///
    /// iroh-ONLY for the reason `Media` and `App` are: the origin is bound to
    /// loopback on the holder and admitted by mesh key, so the IP transport
    /// returns NO candidates — a plaintext guess would be somebody's shop
    /// list served to anyone on the overlay.
    Offer,
}

impl TrafficClass {
    /// Every traffic class, in flip order. Callers that must apply a
    /// policy to all peer traffic — e.g. routing every class over iroh
    /// when the mesh-wide encryption policy is on — enumerate this.
    pub const ALL: [TrafficClass; 10] = [
        TrafficClass::Gossip,
        TrafficClass::ControlPlane,
        TrafficClass::KnowledgeSearch,
        TrafficClass::ModelTransfer,
        TrafficClass::Inference,
        TrafficClass::StatusProbe,
        TrafficClass::RpcTensor,
        TrafficClass::Media,
        TrafficClass::App,
        TrafficClass::Offer,
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
            TrafficClass::Media => "media",
            TrafficClass::App => "app",
            TrafficClass::Offer => "offer",
        }
    }

    /// The class [`as_str`](Self::as_str) names, or `None`. Derived from
    /// [`ALL`](Self::ALL) and `as_str`, so the two spellings cannot drift.
    pub fn from_name(name: &str) -> Option<TrafficClass> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

/// Everything a transport may need to reach a peer, extracted from
/// a `MemberRecord` via `peer_contact`. Keeping this a separate
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
#[cfg_attr(
    any(feature = "wire", test),
    derive(serde::Serialize, serde::Deserialize)
)]
pub struct PeerEndpoint {
    /// Scheme + authority only — no path, no trailing slash:
    /// `http://100.64.0.2:9742`, `http://[fd7a::1]:9741`. Call
    /// sites append their route path.
    pub base_url: String,
    /// Glassbox label for tracing: `ip:100.64.0.2:9742`,
    /// `iroh:127.0.0.1:54321→ab3f…`.
    pub label: String,
}

/// How this node reaches mesh peers. Implementations: `IpTransport`
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

    /// Feedback that `endpoint` did NOT answer for `peer` on `class`-traffic.
    /// A transport that caches what it resolved drops the entry, so the next
    /// dial resolves again (`RailsTransport`). Default: ignore.
    fn note_failure(&self, _peer: NodeId, _class: TrafficClass, _endpoint: &PeerEndpoint) {}
}
