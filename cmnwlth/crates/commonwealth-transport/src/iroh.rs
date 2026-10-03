// SPDX-License-Identifier: AGPL-3.0-or-later
//! EXPERIMENTAL dial-by-key transport over iroh (QUIC by Ed25519
//! key, hole-punching, optional relays). Feature-gated behind
//! `iroh` and excluded from default workspace gates; the spike e2e
//! lives in `sovereign-mesh/tests/iroh_transport_e2e.rs` (run with
//! `cargo test -p sovereign-mesh --features iroh-experimental`).
//!
//! ## Shape: localhost byte-tunnels, not a new HTTP stack
//!
//! The [`PeerTransport`] contract resolves base URLs, and every call
//! site keeps its existing `reqwest` client — so this transport
//! terminates iroh QUIC locally and hands HTTP a plain TCP socket:
//!
//! - Client side ([`IrohTransport`]): per-peer localhost
//!   `TcpListener`; each accepted TCP connection opens one iroh
//!   bi-stream to the peer (dialed by its Ed25519 key — the
//!   `MemberRecord.node_pubkey`) and copies bytes both ways.
//! - Server side ([`IrohAcceptor`]): accepts iroh bi-streams and
//!   copies each into a fresh TCP connection to the daemon's
//!   existing localhost listener — unmodified axum router,
//!   unmodified middleware.
//!
//! Known upgrade path (deliberately NOT in the spike): serve hyper
//! directly on iroh streams (drop the double-copy), carry the
//! traffic class in the ALPN or a stream header so one acceptor can
//! route to both daemon ports, and a tunnel-proxy sidecar for the
//! raw-TCP `rpc-server` tensor traffic that this transport
//! intentionally does not cover.
//!
//! ## Spike limitations (documented, intentional)
//!
//! - The acceptor forwards to ONE local address — fine for the
//!   internal-port classes the spike exercises; the class-in-ALPN
//!   upgrade lifts this.
//! - Peer iroh socket addresses come from an explicitly seeded map
//!   ([`IrohTransport::add_known_peer`]); production would use
//!   relays/address-lookup. `MemberRecord` already carries the key,
//!   which is the part that must travel in the trust ring.

use std::collections::HashMap;
use std::net::SocketAddr;

pub use crate::iroh_identity_forward::Forward;
use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};

use crate::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

pub use mesh_reach::alpn::ALPN;
// The guest dialer and the one HTTP bridge moved to `mesh_reach::guest`
// (pb-reach-guest, phase-b-35); re-exported so every call site keeps its
// `commonwealth_transport::iroh::` spelling. The acceptor below splices
// through the same `pump`.
#[cfg(feature = "iroh-relay-only")]
pub use mesh_reach::guest::build_relay_only_endpoint;
pub use mesh_reach::guest::{
    build_relayed_endpoint, configured_proxy_redacted, parse_dial_string, relay_pin_active,
    ring_crypto_provider, HttpBridge, RelayConfig, GUEST_ALPN,
};
use mesh_reach::guest::{pump, PumpSide};

pub use mesh_reach::alpn::{CLIENT_ALPN, RPC_ALPN};

// The three origin protocols moved to `origin_alpn.rs` (2026-09-13, adding
// `OFFER_ALPN`): this file is past ARCH §3.2's ceiling and a third ALPN with
// its reasoning was where the next line would have gone, the same call
// `iroh_path.rs` made. Re-exported here so every call site keeps spelling
// them `commonwealth_transport::iroh::MEDIA_ALPN` — the extraction changed no
// import anywhere, which is what makes it behaviour-preserving.
pub use crate::origin_alpn::{APP_ALPN, MEDIA_ALPN, OFFER_ALPN};

// Re-exported so feature consumers (sovereign-server, the mobile
// core, the sovereign-mesh spike test) build endpoints without
// declaring their own iroh dependency — keeps the version pin in
// exactly one place.
pub use iroh::endpoint::presets;
pub use iroh::endpoint::Builder as EndpointBuilder;
// Per-peer connection observability (H2): `remote_info` returns these.
pub use iroh::endpoint::TransportAddrUsage;
// Founder-reachability watchdog: `Endpoint::home_relay_status()` returns a
// `Watcher<Vec<RelayStatus>>`. Re-exported here so the mesh crate consumes them
// without declaring its own `iroh` dependency.
pub use iroh::endpoint::RelayStatus;
pub use iroh::{Endpoint, EndpointAddr, PublicKey, RelayUrl, SecretKey, TransportAddr, Watcher};

// Per-peer path classification. Lives in its own module — `iroh.rs` is past
// ARCH §3.2's ceiling and this was new surface, so it went beside the file
// rather than into it. Re-exported here so every call site keeps spelling it
// `commonwealth_transport::iroh::peer_path_snapshot`, and so the "iroh symbol
// use is confined" note above stays checkable: `iroh_path.rs` is the second
// (and only other) file that names iroh types.
pub use crate::iroh_path::{peer_path_snapshot, PeerPath, PeerPathSnapshot};

/// Render an endpoint's current dial info as a pairing string
/// ([`parse_dial_string`]'s inverse). Relay URLs first (stable),
/// then direct addresses. `None` while the endpoint has no
/// reachable address yet.
pub fn format_dial_string(addr: &EndpointAddr) -> Option<String> {
    let mut targets: Vec<String> = addr.relay_urls().map(|r| r.to_string()).collect();
    targets.extend(addr.ip_addrs().map(|a| a.to_string()));
    if targets.is_empty() {
        return None;
    }
    Some(format!(
        "{}@{}",
        hex::encode(addr.id.as_bytes()),
        targets.join(",")
    ))
}

/// Client half: resolves a peer's `node_pubkey` to a localhost base
/// URL bridged over iroh.
#[derive(Debug)]
pub struct IrohTransport {
    endpoint: iroh::Endpoint,
    /// Out-of-band dial hints: pubkey → iroh UDP socket addresses.
    /// **Fallback only.** Production (W2) resolves dial info from the
    /// `PeerContact` the mesh gossiped — relay URL + direct addrs.
    /// Retained so hermetic tests can seed addresses directly; when a
    /// contact carries its own addrs, both are merged.
    known_addrs: std::sync::Mutex<HashMap<[u8; 32], Vec<SocketAddr>>>,
    /// One localhost bridge per (peer, ALPN): a peer is reached on the
    /// internal ALPN for most classes and the client ALPN for
    /// inference/status, so the two ride separate tunnels. Each entry
    /// remembers the dial info it was built with (`dial_key`) so a
    /// gossiped dial-info change REBUILDS the bridge — a frozen target
    /// kept dialing a restarted peer's dead ephemeral port forever
    /// (the 2026-07-19 dual-restart heal deadlock's transport half).
    bridges: tokio::sync::Mutex<HashMap<([u8; 32], &'static [u8]), CachedBridge>>,
}

/// A cached bridge plus the normalized contact dial-info it targets.
#[derive(Debug)]
struct CachedBridge {
    bridge: Arc<HttpBridge>,
    dial_key: DialKey,
}

/// Normalized (relay_url, sorted direct addrs) — the parts of a
/// `PeerContact` that determine where a bridge's iroh dials go.
type DialKey = (Option<String>, Vec<SocketAddr>);

fn dial_key_for(peer: &PeerContact) -> DialKey {
    let mut addrs = peer.iroh_direct_addrs.clone();
    addrs.sort();
    addrs.dedup();
    (peer.relay_url.clone(), addrs)
}

impl IrohTransport {
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self {
            endpoint,
            known_addrs: std::sync::Mutex::new(HashMap::new()),
            bridges: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Seed dial hints for a peer. Test/fallback surface — production
    /// reads them from the gossiped `PeerContact` instead.
    pub fn add_known_peer(&self, pubkey: NodePubkey, addrs: Vec<SocketAddr>) {
        if let Ok(mut map) = self.known_addrs.lock() {
            map.insert(*pubkey.as_bytes(), addrs);
        }
    }

    /// The ALPN a traffic class rides: client-port classes
    /// (Inference/StatusProbe) reach the peer's client router, every
    /// other class reaches its internal router. The *class chooses the
    /// ALPN* — the iroh analogue of `IpTransport`'s per-class port
    /// policy, and why there's no port rewrite here.
    pub fn alpn_for_class(class: TrafficClass) -> &'static [u8] {
        match class {
            TrafficClass::Inference | TrafficClass::StatusProbe => CLIENT_ALPN,
            TrafficClass::RpcTensor => RPC_ALPN,
            TrafficClass::Media => MEDIA_ALPN,
            TrafficClass::App => APP_ALPN,
            TrafficClass::Offer => OFFER_ALPN,
            _ => ALPN,
        }
    }

    /// Build the dial target from the peer's gossiped iroh info (relay
    /// URL + direct addrs), merging any test-seeded `known_addrs`.
    /// `None` when there's no usable path (no relay AND no address) — a
    /// bare key isn't dialable without one, so such a peer falls
    /// through to the IP transport in a routed composition.
    fn endpoint_addr_for(
        &self,
        pubkey: &NodePubkey,
        peer: &PeerContact,
    ) -> Option<iroh::EndpointAddr> {
        let id = iroh::PublicKey::from_bytes(pubkey.as_bytes()).ok()?;
        let mut ea = iroh::EndpointAddr::new(id);
        let mut has_path = false;
        if let Some(relay) = peer.relay_url.as_deref() {
            match relay.parse::<RelayUrl>() {
                Ok(url) => {
                    ea = ea.with_relay_url(url);
                    has_path = true;
                }
                Err(e) => tracing::warn!(
                    target: "transport",
                    transport = "iroh",
                    relay = %relay,
                    error = %e,
                    "iroh: peer relay_url did not parse — ignoring"
                ),
            }
        }
        // Relay-pin (bench posture): when this process is relay-pinned and
        // the peer has a relay, seed ONLY the relay. Seeding direct addrs
        // makes the pin a RACE the selector cannot win — a direct path that
        // validates first (same box ~1ms, warmed LAN) becomes the current
        // path, and the selector's no-relay-open fallback keeps it: the run
        // silently measures the direct path at full speed (observed
        // 2026-07-19: hairpin "relay" run at 0.9ms/16KB). With only the
        // relay seeded, relay is current from the first packet and
        // later-discovered directs stay unselected.
        if relay_pin_active() && has_path {
            return Some(ea);
        }
        let seeded = self
            .known_addrs
            .lock()
            .ok()
            .and_then(|m| m.get(pubkey.as_bytes()).cloned())
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        for a in peer.iroh_direct_addrs.iter().copied().chain(seeded) {
            if seen.insert(a) {
                ea = ea.with_ip_addr(a);
                has_path = true;
            }
        }
        has_path.then_some(ea)
    }

    /// Get or create the localhost TCP bridge for `(pubkey, alpn)`,
    /// dialing the target resolved from `peer`.
    async fn bridge_for(
        &self,
        pubkey: &NodePubkey,
        peer: &PeerContact,
        alpn: &'static [u8],
    ) -> Option<SocketAddr> {
        let key = (*pubkey.as_bytes(), alpn);
        let dial_key = dial_key_for(peer);
        let mut bridges = self.bridges.lock().await;
        if let Some(cached) = bridges.get(&key) {
            if cached.dial_key == dial_key {
                return Some(cached.bridge.local_addr());
            }
        }
        let Some(target) = self.endpoint_addr_for(pubkey, peer) else {
            // The fresh contact has no dialable path at all (no relay, no
            // direct addrs). Nothing to retarget TO — drop any stale bridge
            // rather than keep tunneling at an address the peer has left.
            bridges.remove(&key);
            return None;
        };
        if let Some(cached) = bridges.get_mut(&key) {
            // The peer's gossiped dial info changed (typical: it
            // restarted and its ephemeral iroh port moved). A frozen
            // bridge would keep dialing the dead target forever — point
            // this one at the fresh contact instead. Retargeting rather
            // than rebuilding keeps the loopback port stable, which is
            // what plain-TCP clients holding that address depend on (see
            // `HttpBridge::retarget`); in-flight tunnels to the stale
            // target were doomed either way.
            cached.bridge.retarget(target);
            cached.dial_key = dial_key;
            let local_addr = cached.bridge.local_addr();
            tracing::info!(
                target: "transport",
                peer = %hex::encode(&key.0[..4]),
                bridge = %local_addr,
                "iroh bridge: peer dial info changed — retargeted in place (port held)"
            );
            return Some(local_addr);
        }
        let bridge = HttpBridge::spawn_preferring(
            self.endpoint.clone(),
            target,
            alpn,
            Some(preferred_bridge_port(pubkey, alpn)),
        )
        .await
        .ok()?;
        let local_addr = bridge.local_addr();
        bridges.insert(
            key,
            CachedBridge {
                bridge: Arc::new(bridge),
                dial_key,
            },
        );
        Some(local_addr)
    }
}

/// Copy bytes both ways between a TCP socket and an iroh bi-stream
/// until both directions close.
/// The loopback port a peer's bridge prefers, from its key and the ALPN —
/// identity from essence (ARCH §7.5), never a counter or whatever the kernel
/// handed out last time. 20000..=32767 sits below Linux's default ephemeral
/// range (32768–60999), so a derived port is never one an unrelated socket
/// was just given; a collision between two peers is a 1-in-12768 event per
/// pair and is handled by the named fallback in
/// [`HttpBridge::spawn_preferring`].
pub fn preferred_bridge_port(pubkey: &NodePubkey, alpn: &[u8]) -> u16 {
    // FNV-1a: spread, not secrecy. The inputs are public.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pubkey.0.iter().chain(alpn.iter()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    20_000 + (h % 12_768) as u16
}

#[async_trait::async_trait]
impl PeerTransport for IrohTransport {
    fn name(&self) -> &'static str {
        "iroh"
    }

    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint> {
        // No identity key → not dialable on this transport. (A
        // routed/fallback composition would send such peers to the
        // IP transport.)
        let Some(pubkey) = peer.node_pubkey else {
            tracing::debug!(
                target: "transport",
                transport = "iroh",
                class = class.as_str(),
                peer = %peer.node_id,
                "iroh: peer has no node_pubkey — not dialable"
            );
            return Vec::new();
        };
        let alpn = Self::alpn_for_class(class);
        let Some(local) = self.bridge_for(&pubkey, peer, alpn).await else {
            tracing::debug!(
                target: "transport",
                transport = "iroh",
                class = class.as_str(),
                peer = %peer.node_id,
                "iroh: no dialable path in contact (no relay_url / iroh_direct_addrs) \
                 — not dialable (routed composition falls back to IP)"
            );
            return Vec::new();
        };
        let ep = PeerEndpoint {
            base_url: format!("http://{local}"),
            label: format!("iroh:{local}→{}", &pubkey.to_string()[..8]),
        };
        tracing::debug!(
            target: "transport",
            transport = "iroh",
            class = class.as_str(),
            peer = %peer.node_id,
            candidates = 1usize,
            first = %ep.label,
            "transport: resolved"
        );
        vec![ep]
    }

    fn note_success(&self, _peer: NodeId, _class: TrafficClass, _endpoint: &PeerEndpoint) {
        // iroh maintains and migrates paths itself; nothing to do.
    }
}

/// Server half: accept iroh bi-streams and forward each to the
/// daemon's existing localhost HTTP listener.
pub struct IrohAcceptor {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for IrohAcceptor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl IrohAcceptor {
    /// Spawn the accept loop forwarding EVERY accepted bi-stream to a
    /// single local listener, regardless of negotiated ALPN. Right for
    /// a single-ALPN endpoint (Track M: `sovereign-server` binds only
    /// `cwth/client/0` → its HTTP listener).
    ///
    /// Admits any dialer. Correct where the local listener authenticates
    /// for itself (`sovereign-server` requires a bearer of every caller);
    /// NOT correct where it trusts loopback — see
    /// [`spawn_admitting`](Self::spawn_admitting).
    pub fn spawn(endpoint: iroh::Endpoint, forward_to: SocketAddr) -> Self {
        Self::run(endpoint, move |_alpn, _dialer| async move {
            Some(Forward::Splice(forward_to))
        })
    }

    /// Spawn the accept loop routing each connection to a local
    /// listener chosen by its **negotiated ALPN** — the W1 capability
    /// that lets one daemon endpoint serve both the internal router
    /// (`cwth/http/0`) and the client router (`cwth/client/0`) without
    /// a port (the class chose the ALPN). A connection whose ALPN is
    /// not in `routes` is closed with a loud log, never misrouted.
    pub fn spawn_routed(endpoint: iroh::Endpoint, routes: HashMap<Vec<u8>, SocketAddr>) -> Self {
        Self::run(endpoint, move |alpn, _dialer| {
            let target = routes.get(&alpn).copied().map(Forward::Splice);
            async move { target }
        })
    }

    /// Spawn the accept loop routing each connection by its negotiated ALPN
    /// **and by who dialed it**.
    ///
    /// # Why the dialer has to be part of the routing decision
    ///
    /// An iroh endpoint accepts anyone. The dial string that reaches it is
    /// public by design — it rides in every mesh invite's `dial=` and is
    /// gossiped as `MemberRecord.node_pubkey` — so "holds the dial string"
    /// is not a credential and must never be treated as one. But the
    /// acceptor forwards by `TcpStream::connect`ing a loopback listener,
    /// and a listener that trusts loopback (correctly, for the local user)
    /// cannot tell that hop from a real local caller. Route on ALPN alone
    /// and every dial-string holder inherits whatever that listener grants
    /// its own machine.
    ///
    /// What the connection DOES carry is the dialer's Ed25519 public key,
    /// verified by the QUIC handshake — the same key a mesh gossips as
    /// `node_pubkey`. `resolve` receives it, so a caller can send members
    /// and strangers to different listeners (or send a stranger nowhere)
    /// on evidence rather than on transport.
    ///
    /// `resolve` returning `None` closes the connection. It is async so the
    /// decision can consult live state — membership changes between dials.
    pub fn spawn_admitting<F, Fut>(endpoint: iroh::Endpoint, resolve: F) -> Self
    where
        F: Fn(Vec<u8>, NodePubkey) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Option<SocketAddr>> + Send + 'static,
    {
        let resolve = Arc::new(resolve);
        Self::run(endpoint, move |alpn, dialer| {
            let resolve = resolve.clone();
            async move { resolve(alpn, dialer).await.map(Forward::Splice) }
        })
    }

    /// [`spawn_admitting`](Self::spawn_admitting), where the resolver also
    /// picks the KIND of forward: a byte splice, or an HTTP origin that is
    /// handed the dialer's verified identity on every request
    /// ([`Forward::Http`]). One accept loop serves both; the kind is decided
    /// once per connection beside the listener, by the same evidence.
    pub fn spawn_admitting_forward<F, Fut>(endpoint: iroh::Endpoint, resolve: F) -> Self
    where
        F: Fn(Vec<u8>, NodePubkey) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Option<Forward>> + Send + 'static,
    {
        Self::run(endpoint, resolve)
    }

    /// Shared accept loop. `resolve` maps a connection's negotiated ALPN
    /// and its verified dialer key to the local TCP target its bi-streams
    /// forward to; `None` closes the connection. Both are read once per
    /// connection, then every bi-stream on it is pumped to that target.
    fn run<F, Fut>(endpoint: iroh::Endpoint, resolve: F) -> Self
    where
        F: Fn(Vec<u8>, NodePubkey) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Option<Forward>> + Send + 'static,
    {
        let resolve = Arc::new(resolve);
        let task = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let resolve = resolve.clone();
                tokio::spawn(async move {
                    let conn = match incoming.await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::debug!(
                                target: "transport",
                                error = %e,
                                "iroh acceptor: handshake failed"
                            );
                            return;
                        }
                    };
                    // A fully-accepted connection has a negotiated ALPN and
                    // a verified dialer key (from the peer's TLS
                    // certificate). Route this connection's streams by both.
                    let alpn = conn.alpn().to_vec();
                    let dialer = NodePubkey(*conn.remote_id().as_bytes());
                    let Some(forward) = resolve(alpn.clone(), dialer).await else {
                        tracing::warn!(
                            target: "transport",
                            alpn = %String::from_utf8_lossy(&alpn),
                            dialer = %hex::encode(dialer.0),
                            "iroh acceptor: no local forward for this (ALPN, dialer) — closing connection"
                        );
                        return;
                    };
                    // `HttpByName` has no single target: it resolves per
                    // bi-stream from the request path, so it connects inside
                    // the pump rather than here.
                    enum Streams {
                        To(SocketAddr, Option<Arc<Vec<(String, String)>>>),
                        ByName(
                            Arc<std::collections::BTreeMap<String, SocketAddr>>,
                            Arc<Vec<(String, String)>>,
                        ),
                        ByPrefix(
                            Arc<
                                std::collections::BTreeMap<
                                    String,
                                    crate::iroh_routed_forward::PrefixRoute,
                                >,
                            >,
                            Arc<Vec<(String, String)>>,
                        ),
                    }
                    let streams = match forward {
                        Forward::Splice(addr) => Streams::To(addr, None),
                        Forward::Http { origin, headers } => {
                            Streams::To(origin, Some(Arc::new(headers)))
                        }
                        Forward::HttpByName { apps, headers } => {
                            Streams::ByName(apps, Arc::new(headers))
                        }
                        Forward::HttpByPrefix { routes, headers } => {
                            Streams::ByPrefix(routes, Arc::new(headers))
                        }
                    };
                    loop {
                        match conn.accept_bi().await {
                            Ok((send, recv)) => {
                                let streams = match &streams {
                                    Streams::To(a, h) => Streams::To(*a, h.clone()),
                                    Streams::ByName(m, h) => {
                                        Streams::ByName(Arc::clone(m), Arc::clone(h))
                                    }
                                    Streams::ByPrefix(m, h) => {
                                        Streams::ByPrefix(Arc::clone(m), Arc::clone(h))
                                    }
                                };
                                // The pump's zero-answer warn names the ALPN;
                                // clone it so the next accepted stream still
                                // has it.
                                let alpn = alpn.clone();
                                tokio::spawn(async move {
                                    let (forward_to, identity) = match streams {
                                        Streams::ByName(apps, headers) => {
                                            crate::iroh_identity_forward::pump_by_name(
                                                send, recv, apps, headers,
                                            )
                                            .await;
                                            return;
                                        }
                                        Streams::ByPrefix(routes, headers) => {
                                            crate::iroh_routed_forward::pump_by_prefix(
                                                send, recv, routes, headers,
                                            )
                                            .await;
                                            return;
                                        }
                                        Streams::To(a, h) => (a, h),
                                    };
                                    match tokio::net::TcpStream::connect(forward_to).await {
                                        Ok(tcp) => {
                                            // Same Nagle × delayed-ACK stall as the
                                            // bridge side — see HttpBridge::spawn.
                                            tcp.set_nodelay(true).ok();
                                            match identity {
                                        None => pump(
                                            tcp,
                                            send,
                                            recv,
                                            PumpSide::Acceptor,
                                            hex::encode(dialer.0),
                                            &alpn,
                                        )
                                        .await,
                                                Some(headers) => {
                                                    crate::iroh_identity_forward::pump_with_identity(
                                                        tcp, send, recv, headers,
                                                    )
                                                    .await
                                                }
                                            }
                                        }
                                        Err(e) => tracing::warn!(
                                            target: "transport",
                                            error = %e,
                                            forward_to = %forward_to,
                                            "iroh acceptor: local forward connect failed"
                                        ),
                                    }
                                });
                            }
                            Err(_) => break, // connection closed
                        }
                    }
                });
            }
        });
        Self { task }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Bind an empty (no-relay, deterministic) endpoint serving `alpns`.
    async fn hermetic_endpoint(seed: u8, alpns: Vec<Vec<u8>>) -> Endpoint {
        EndpointBuilder::empty()
            .crypto_provider(ring_crypto_provider())
            .secret_key(SecretKey::from_bytes(&[seed; 32]))
            .alpns(alpns)
            .bind()
            .await
            .expect("hermetic endpoint bind")
    }

    /// iroh binds the wildcard; rewrite to loopback so the address is
    /// dialable in-process (mirrors the spike e2e's `dialable_sockets`).
    fn loopback_sockets(endpoint: &Endpoint) -> Vec<SocketAddr> {
        endpoint
            .bound_sockets()
            .into_iter()
            .map(|mut a| {
                if a.ip().is_unspecified() {
                    let ip = if a.is_ipv4() { "127.0.0.1" } else { "::1" };
                    a.set_ip(ip.parse().unwrap());
                }
                a
            })
            .collect()
    }

    /// The same peer and protocol always prefer the same port, a different
    /// protocol on the same peer prefers a different one, and every derived
    /// port sits below the kernel's ephemeral range. The failing inputs are
    /// a port outside 20000..=32767 or two calls disagreeing.
    #[test]
    fn a_peers_bridge_port_is_derived_from_its_key_and_alpn() {
        let a = NodePubkey([7u8; 32]);
        let b = NodePubkey([8u8; 32]);
        let p = preferred_bridge_port(&a, MEDIA_ALPN);
        assert_eq!(p, preferred_bridge_port(&a, MEDIA_ALPN));
        assert_ne!(p, preferred_bridge_port(&a, CLIENT_ALPN));
        assert_ne!(p, preferred_bridge_port(&b, MEDIA_ALPN));
        for port in [p, preferred_bridge_port(&b, MEDIA_ALPN)] {
            assert!((20_000..=32_767).contains(&port), "{port}");
        }
    }

    /// When the derived port is already bound, the bridge still comes up —
    /// on another port, named at warn — rather than failing the dial.
    #[tokio::test]
    async fn a_taken_derived_port_falls_back_to_an_ephemeral_one_and_still_serves() {
        let server_ep = hermetic_endpoint(61, vec![CLIENT_ALPN.to_vec()]).await;
        let client_ep = hermetic_endpoint(62, vec![]).await;
        let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut target = EndpointAddr::new(server_ep.id());
        for sock in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(sock);
        }
        let bridge = HttpBridge::spawn_preferring(client_ep, target, CLIENT_ALPN, Some(port))
            .await
            .expect("a taken preferred port is not a failed bridge");
        assert_ne!(bridge.local_addr().port(), port);
        assert!(bridge.local_addr().ip().is_loopback());
    }

    /// A trivial TCP "service": every accepted connection is answered
    /// with a fixed marker, then closed. Stands in for one of the
    /// daemon's two local HTTP listeners — the marker is the witness
    /// that a stream reached THIS listener and not the other. It also
    /// drains the client's bytes so the close is a clean FIN, not a
    /// RST that could truncate the marker in flight on loopback.
    async fn spawn_marker_listener(marker: &'static [u8]) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let _ = sock.write_all(marker).await;
                    let _ = sock.shutdown().await;
                    let mut drain = Vec::new();
                    let _ = sock.read_to_end(&mut drain).await;
                });
            }
        });
        addr
    }

    /// Dial the routed acceptor on `alpn` via an `HttpBridge` and read
    /// back whatever local listener the acceptor forwarded us to. Each
    /// call uses a FRESH client endpoint (seeded distinctly) so the two
    /// ALPN dials are unambiguously separate QUIC connections — no risk
    /// of a coalesced connection carrying the prior dial's ALPN.
    async fn read_marker_over(seed: u8, server: &EndpointAddr, alpn: &'static [u8]) -> Vec<u8> {
        let client_ep = hermetic_endpoint(seed, vec![]).await;
        let bridge = HttpBridge::spawn(client_ep, server.clone(), alpn)
            .await
            .expect("bridge spawns");
        let mut tcp = tokio::net::TcpStream::connect(bridge.local_addr())
            .await
            .expect("connect to bridge");
        // Send a probe and half-close our write side. A QUIC bi-stream
        // a client merely opens isn't surfaced to the server's
        // `accept_bi()` until the client sends on it — so without this,
        // the acceptor never sees the stream and never forwards. This
        // mirrors real traffic (reqwest sends a request first); the FIN
        // also lets the marker listener drain to a clean close.
        let _ = tcp.write_all(b"ping").await;
        let _ = tcp.shutdown().await;
        let mut buf = Vec::new();
        // Generous timeout: a routing miss closes the connection, which
        // surfaces here as an empty read rather than a hang.
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tcp.read_to_end(&mut buf),
        )
        .await;
        read.expect("read did not time out").expect("read ok");
        buf
    }

    /// W1 keystone: ONE iroh endpoint, bound with both the internal and
    /// client ALPNs, dispatches each accepted connection to a different
    /// local listener purely by its negotiated ALPN — the "class chose
    /// the ALPN, not a port" property that lets the mesh daemon serve
    /// its internal (`cwth/http/0`) and client (`cwth/client/0`)
    /// routers over a single dial-by-key endpoint.
    #[tokio::test]
    async fn routed_acceptor_dispatches_by_alpn() {
        let internal_addr = spawn_marker_listener(b"INTERNAL").await;
        let client_addr = spawn_marker_listener(b"CLIENT").await;

        let server_ep = hermetic_endpoint(11, vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(ALPN.to_vec(), internal_addr);
        routes.insert(CLIENT_ALPN.to_vec(), client_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        // The dialable target: the server's key + its loopback sockets.
        let mut target = EndpointAddr::new(server_ep.id());
        for s in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(s);
        }

        // Internal ALPN must land on the internal listener…
        assert_eq!(
            read_marker_over(21, &target, ALPN).await,
            b"INTERNAL",
            "cwth/http/0 must route to the internal listener"
        );
        // …and the client ALPN on the client listener — same endpoint,
        // same key, routed solely by ALPN.
        assert_eq!(
            read_marker_over(22, &target, CLIENT_ALPN).await,
            b"CLIENT",
            "cwth/client/0 must route to the client listener"
        );
    }

    /// The guest split, on the wire. A guest and a peer dial the SAME key and
    /// must land on DIFFERENT local listeners, because the two listeners
    /// disagree about whether a loopback forward hop is a credential. Routing
    /// them together is the bug this ALPN exists to make impossible: a peer's
    /// federated inference carries no `Authorization` at all, so the listener
    /// it needs is exactly the one a guest must never reach.
    #[tokio::test]
    async fn a_guest_and_a_peer_dialing_one_key_land_on_different_listeners() {
        let client_addr = spawn_marker_listener(b"CLIENT").await;
        let guest_addr = spawn_marker_listener(b"GUEST").await;

        let server_ep =
            hermetic_endpoint(15, vec![CLIENT_ALPN.to_vec(), GUEST_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(CLIENT_ALPN.to_vec(), client_addr);
        routes.insert(GUEST_ALPN.to_vec(), guest_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        let mut target = EndpointAddr::new(server_ep.id());
        for s in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(s);
        }

        assert_eq!(
            read_marker_over(25, &target, CLIENT_ALPN).await,
            b"CLIENT",
            "a peer stays on the trusting client listener"
        );
        assert_eq!(
            read_marker_over(26, &target, GUEST_ALPN).await,
            b"GUEST",
            "a guest lands on the bearer-only listener"
        );
    }

    /// A daemon with no guest listener does not advertise the ALPN, so the
    /// dial is refused rather than falling through to the trusting listener.
    /// The failure this pins is silent widening, not a broken dial.
    #[tokio::test]
    async fn an_unrouted_guest_alpn_is_dropped_rather_than_falling_back_to_the_client_listener() {
        let client_addr = spawn_marker_listener(b"CLIENT").await;
        let server_ep =
            hermetic_endpoint(16, vec![CLIENT_ALPN.to_vec(), GUEST_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(CLIENT_ALPN.to_vec(), client_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        let mut target = EndpointAddr::new(server_ep.id());
        for s in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(s);
        }
        let got = read_marker_over(27, &target, GUEST_ALPN).await;
        assert!(
            got.is_empty(),
            "an unrouted guest dial must be dropped, never served by the client              listener — got {got:?}"
        );
    }

    /// Five protocols, five distinct byte strings. A collision would route
    /// one class to another's listener with no error anywhere.
    #[test]
    fn the_alpns_are_distinct() {
        let all = [ALPN, CLIENT_ALPN, GUEST_ALPN, RPC_ALPN, MEDIA_ALPN];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "ALPNs must be distinct");
            }
        }
    }

    /// The viewer half of federated media dials the ALPN the holder's
    /// acceptor admits members on — and ONLY that one. The failing input is
    /// `Media` falling into the `_ => ALPN` arm: the dial would reach the
    /// peer's internal router, which serves no bytes a player wants, and the
    /// symptom would be a bridge that "works" (loopback accepts) and a
    /// player that never starts.
    #[test]
    fn the_media_class_rides_the_media_alpn_and_no_other() {
        assert_eq!(
            IrohTransport::alpn_for_class(TrafficClass::Media),
            MEDIA_ALPN
        );
        for class in TrafficClass::ALL {
            if class != TrafficClass::Media {
                assert_ne!(
                    IrohTransport::alpn_for_class(class),
                    MEDIA_ALPN,
                    "{} must not reach the media origin",
                    class.as_str()
                );
            }
        }
    }

    /// A connection negotiating an ALPN with no route is closed, not
    /// misrouted to whatever happens to be in the map.
    #[tokio::test]
    async fn routed_acceptor_drops_unknown_alpn() {
        let internal_addr = spawn_marker_listener(b"INTERNAL").await;
        // Server offers BOTH ALPNs (so the handshake succeeds) but the
        // route table only knows the internal one.
        let server_ep = hermetic_endpoint(13, vec![ALPN.to_vec(), CLIENT_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(ALPN.to_vec(), internal_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        let mut target = EndpointAddr::new(server_ep.id());
        for s in loopback_sockets(&server_ep) {
            target = target.with_ip_addr(s);
        }
        // Dialing the unrouted (but offered) client ALPN: the acceptor
        // closes the connection, so we read zero bytes — never INTERNAL.
        let got = read_marker_over(24, &target, CLIENT_ALPN).await;
        assert!(got.is_empty(), "unrouted ALPN must be dropped, got {got:?}");
    }

    #[test]
    fn relay_config_from_parts_maps_discovery() {
        // Default / "n0" / absent / empty → n0 services on.
        assert!(RelayConfig::default().n0_services);
        assert!(
            RelayConfig::from_parts(vec![], None)
                .expect("absent is n0")
                .n0_services
        );
        assert!(
            RelayConfig::from_parts(vec![], Some("n0"))
                .expect("n0")
                .n0_services
        );
        assert!(
            RelayConfig::from_parts(vec![], Some(""))
                .expect("empty is n0")
                .n0_services
        );
        // The one sovereignty spelling → n0 severed.
        let c = RelayConfig::from_parts(vec![], Some("none")).expect("none");
        assert!(!c.n0_services);
        // relay_urls passes through.
        let c = RelayConfig::from_parts(vec!["https://r.example:443".into()], Some("none"))
            .expect("none");
        assert_eq!(c.relay_urls, vec!["https://r.example:443".to_string()]);
        assert!(!c.n0_services);
    }

    /// **C2: a typo'd value refuses to load** (ROOT_CAUSE_FIXES C2). The old
    /// shape warned and kept n0 — the substitution this door exists to stop.
    /// Watched failing first, with the warn-and-keep planted back.
    #[test]
    fn an_unknown_discovery_value_refuses_to_load() {
        let err = RelayConfig::from_parts(vec![], Some("carrier-pigeon")).unwrap_err();
        assert!(err.contains("carrier-pigeon"), "the value is named: {err}");
        assert!(
            err.contains("accepted"),
            "and the accepted set is named: {err}"
        );
    }

    /// **C2: `self`/`local` are refused naming `none`** — they were never
    /// implemented, and aliasing them to `none` would keep the lie in the
    /// vocabulary. Watched failing first with the same plant.
    #[test]
    fn self_and_local_refuse_naming_none() {
        for d in ["self", "local"] {
            let err = RelayConfig::from_parts(vec![], Some(d)).unwrap_err();
            assert!(err.contains("never implemented"), "{d}: {err}");
            assert!(
                err.contains("`none`"),
                "{d}: the honest spelling must be named: {err}"
            );
        }
    }

    #[tokio::test]
    async fn build_sovereign_endpoint_binds_without_n0() {
        // Sovereign mode (n0 severed, no custom relay = direct-addr
        // only): must still bind a real endpoint. This is the
        // air-gapped-LAN posture — Minimal preset, relays Disabled, no
        // n0 DNS. Nothing here should touch the network at all.
        let ep = build_relayed_endpoint(
            SecretKey::from_bytes(&[78u8; 32]),
            vec![ALPN.to_vec()],
            &RelayConfig {
                relay_urls: vec![],
                n0_services: false,
            },
        )
        .await
        .expect("sovereign (no-n0) endpoint must bind");
        assert!(
            !ep.bound_sockets().is_empty(),
            "endpoint must bind a socket"
        );
    }

    /// Read the marker a bridge's loopback port forwards to. Same handshake as
    /// `read_marker_over` (send first so the peer's `accept_bi` fires), but
    /// against an ALREADY-BOUND bridge port rather than a fresh bridge — the
    /// point being that the port outlives a retarget.
    async fn read_marker_from_port(port: SocketAddr) -> Vec<u8> {
        let mut tcp = tokio::net::TcpStream::connect(port)
            .await
            .expect("connect to bridge port");
        let _ = tcp.write_all(b"ping").await;
        let _ = tcp.shutdown().await;
        let mut buf = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tcp.read_to_end(&mut buf),
        )
        .await;
        read.expect("read did not time out").expect("read ok");
        buf
    }

    fn contact_for(server: &Endpoint, addrs: Vec<SocketAddr>) -> crate::PeerContact {
        crate::PeerContact {
            node_id: commonwealth_core::ids::NodeId::from_u128(9),
            addresses: vec![],
            node_pubkey: Some(NodePubkey(*server.id().as_bytes())),
            relay_url: None,
            iroh_direct_addrs: addrs,
        }
    }

    /// A peer's gossiped dial info changing must RETARGET the bridge, not
    /// rebuild it: the loopback port is what plain-TCP clients hold (ggml's
    /// rpc-server list, via the mesh's discovered worker endpoint string), and
    /// minting a new one made an unmoved peer read downstream as a stream of
    /// different workers (2026-07-25: 34207 → 40043 → 39419 → 34133 → 40021).
    /// The tunnel must still land on the NEW target — that's what the rebuild
    /// was originally protecting (the 2026-07-19 dual-restart heal deadlock), so
    /// this asserts both halves: same port, fresh destination.
    #[tokio::test]
    async fn bridge_retargets_in_place_when_peer_dial_info_changes() {
        let worker_addr = spawn_marker_listener(b"WORKER").await;
        let server_ep = hermetic_endpoint(31, vec![RPC_ALPN.to_vec()]).await;
        let mut routes = HashMap::new();
        routes.insert(RPC_ALPN.to_vec(), worker_addr);
        let _acceptor = IrohAcceptor::spawn_routed(server_ep.clone(), routes);

        let transport = IrohTransport::new(hermetic_endpoint(32, vec![]).await);

        // Tick 1: the peer's gossiped address is stale (nothing listens there).
        // The bridge binds a port; nothing is dialed until a client connects.
        let stale: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let first = transport
            .endpoints(
                &contact_for(&server_ep, vec![stale]),
                TrafficClass::RpcTensor,
            )
            .await;
        let port_before = first[0].base_url.clone();

        // Tick 2: gossip carries the peer's real address. Different dial key.
        let live = loopback_sockets(&server_ep);
        let second = transport
            .endpoints(&contact_for(&server_ep, live), TrafficClass::RpcTensor)
            .await;
        assert_eq!(
            second[0].base_url, port_before,
            "a dial-info change must hold the loopback port, not mint a new one"
        );

        // …and the tunnel now reaches the peer at its NEW address, proving the
        // held port is not a frozen bridge still dialing the dead one.
        let bridge_port: SocketAddr = second[0]
            .base_url
            .strip_prefix("http://")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            read_marker_from_port(bridge_port).await,
            b"WORKER",
            "the retargeted bridge must reach the peer's current address"
        );
    }

    /// An unchanged contact re-resolves to the SAME bridge with no rebind. This
    /// is what makes the mesh's per-tick re-mint of a known bridged worker
    /// (`sovereign_daemon::daemon::reaffirm_plan` → `Rebridge`) free: it replaces
    /// a `/status` probe that rode the same congested iroh path as the tunnel.
    #[tokio::test]
    async fn repeated_resolution_of_an_unchanged_peer_reuses_the_bridge() {
        let server_ep = hermetic_endpoint(33, vec![RPC_ALPN.to_vec()]).await;
        let transport = IrohTransport::new(hermetic_endpoint(34, vec![]).await);
        let contact = contact_for(&server_ep, loopback_sockets(&server_ep));
        let a = transport.endpoints(&contact, TrafficClass::RpcTensor).await;
        let b = transport.endpoints(&contact, TrafficClass::RpcTensor).await;
        assert_eq!(a[0].base_url, b[0].base_url, "cached bridge must be reused");
    }

    #[test]
    fn dial_key_normalizes_addr_order_and_dupes() {
        use crate::PeerContact;
        use commonwealth_core::ids::NodeId;
        let a: SocketAddr = "10.0.0.1:1000".parse().unwrap();
        let b: SocketAddr = "10.0.0.2:2000".parse().unwrap();
        let mk = |addrs: Vec<SocketAddr>| PeerContact {
            node_id: NodeId::from_u128(1),
            addresses: vec![],
            node_pubkey: None,
            relay_url: Some("https://r.example/".into()),
            iroh_direct_addrs: addrs,
        };
        // Same set, different order / dup → SAME key: a reordered gossip
        // record must NOT churn the bridge.
        assert_eq!(
            super::dial_key_for(&mk(vec![a, b])),
            super::dial_key_for(&mk(vec![b, a, b]))
        );
        // A changed port (peer restarted) → DIFFERENT key → rebuild.
        let b2: SocketAddr = "10.0.0.2:2001".parse().unwrap();
        assert_ne!(
            super::dial_key_for(&mk(vec![a, b])),
            super::dial_key_for(&mk(vec![a, b2]))
        );
    }

    /// The acceptor splices through the leaf's one `pump` (pb-reach-guest):
    /// the name it calls resolves to `mesh_reach::guest::pump`, so a local
    /// copy of the pump in this file turns this red.
    #[test]
    fn the_acceptor_pumps_through_the_leafs_one_pump() {
        assert_eq!(std::any::type_name_of_val(&pump), "mesh_reach::guest::pump");
    }
}
