// SPDX-License-Identifier: AGPL-3.0-or-later
//! How this node reaches a peer — the wire vocabulary of the dial seam.
//!
//! Moved whole from `commonwealth-transport` (fp-40, §12 decision 3: only
//! wire FORMAT/VOCABULARY crosses into the shared leaves; a program's
//! substrate does not). The transport crate keeps the machinery —
//! `IpTransport`, `RoutedTransport`, `IrohTransport`, `peer_contact` — and
//! re-exports everything here at its historical paths, so no call site
//! outside the daemon had to change. `commonwealth-transport`'s module docs
//! remain the narrative home for the seam's design.

use kernel_types::{NodeId, NodePubkey};
use std::net::SocketAddr;

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
}

/// A MEMBER reaching this node's media origin — whatever HTTP media server its
/// operator already runs (Jellyfin's `:8096`, a plain file server, anything
/// that speaks `Range`).
///
/// Its own protocol rather than a path on the client API, because the product is
/// that clients speak the media server's OWN api: the bridge is
/// `tokio::io::copy` in both directions and never parses HTTP, so `Range`
/// passes through untouched and a player seeks as if the library were local. A
/// path on `CLIENT_ALPN` would have meant re-implementing the media server.
pub const MEDIA_ALPN: &[u8] = b"cwth/media/0";

/// A MEMBER reaching one of the HTTP apps this node publishes BY NAME
/// (`[iroh.apps]`) — a chore rotation, a print queue, a thing somebody wrote
/// at 1am and wants to show a housemate now.
///
/// One ALPN for an unbounded number of apps, demultiplexed by a leading path
/// segment (`GET /chores/tasks` → the `chores` origin, rewritten to
/// `GET /tasks`). The closed set stays closed — one variant
/// (`OriginKind::App`), one ALPN, one acceptor route — while the open set,
/// which apps, is config data that changes without a code change (ARCH §9). A
/// per-app ALPN (`cwth/app/0/<name>`) was the alternative and was rejected:
/// ALPNs are pre-registered when the endpoint is built, so publishing an app
/// would mean rebuilding the endpoint, and the whole point of the ephemeral
/// tier is that registering an app is cheaper than a restart.
pub const APP_ALPN: &[u8] = b"cwth/app/0";

/// A MEMBER reaching the HTTP origin that lists what this node's operator has
/// to SELL or LEND (`[iroh] offer_origin`) — a drill going spare, six eggs, a
/// room for a week.
///
/// The bridge parses nothing, exactly as `MEDIA_ALPN`'s does not: what an
/// offer IS stays the origin's, so a house can point this at a static JSON
/// file, a spreadsheet exporter, or a real shop. The catalogue a member sees
/// is COMPUTED by asking every publisher at once
/// (`commonwealth_media::fanout`) rather than stored, so there is no listing
/// to be excluded from and nobody positioned to rank
/// (`docs/internal/RING_APPLICATIONS.md` §Commerce).
pub const OFFER_ALPN: &[u8] = b"cwth/offer/0";

#[cfg(test)]
mod tests {
    use super::*;

    /// The ALPN strings are a WIRE contract between two independently
    /// rebuilt daemons: a rename here silently stops every peer on the old
    /// build from negotiating, with nothing red anywhere. Pinned, not
    /// assumed — the same discipline as `OriginKind`'s serde repr.
    #[test]
    fn the_origin_alpn_strings_are_pinned() {
        assert_eq!(MEDIA_ALPN, b"cwth/media/0");
        assert_eq!(APP_ALPN, b"cwth/app/0");
        assert_eq!(OFFER_ALPN, b"cwth/offer/0");
    }

    /// Three DISTINCT protocols. The failing input is a constant copied
    /// from its neighbour, which would hand a dialer admitted to one origin
    /// class the bytes of another — the separation these ALPNs exist to make
    /// real.
    #[test]
    fn no_two_origin_kinds_share_a_protocol() {
        let mut seen = vec![MEDIA_ALPN, APP_ALPN, OFFER_ALPN];
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before);
    }
}
