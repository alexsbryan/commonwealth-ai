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
use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};

use crate::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

/// ALPN for Commonwealth HTTP-over-iroh tunnels. Version-suffixed so
/// a future class-aware protocol can coexist during migration.
pub const ALPN: &[u8] = b"cwth/http/0";

// Re-exported so feature consumers (the sovereign-mesh spike test)
// build endpoints without declaring their own iroh dependency —
// keeps the version pin in exactly one place.
pub use iroh::endpoint::Builder as EndpointBuilder;
pub use iroh::{Endpoint, SecretKey};

/// The rustls crypto provider for `EndpointBuilder::crypto_provider`.
/// iroh's `Builder::empty()` deliberately sets no provider (only
/// presets choose one), and `bind()` errors without it — pass this.
/// Ring, matching the rest of the workspace's rustls usage.
pub fn ring_crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct BridgeHandle {
    local_addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Client half: resolves a peer's `node_pubkey` to a localhost base
/// URL bridged over iroh.
#[derive(Debug)]
pub struct IrohTransport {
    endpoint: iroh::Endpoint,
    /// Out-of-band dial hints: pubkey → iroh UDP socket addresses.
    /// Spike-only surface (see module docs).
    known_addrs: std::sync::Mutex<HashMap<[u8; 32], Vec<SocketAddr>>>,
    bridges: tokio::sync::Mutex<HashMap<[u8; 32], Arc<BridgeHandle>>>,
}

impl IrohTransport {
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self {
            endpoint,
            known_addrs: std::sync::Mutex::new(HashMap::new()),
            bridges: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Seed dial hints for a peer (spike-only; production uses
    /// relays/address lookup).
    pub fn add_known_peer(&self, pubkey: NodePubkey, addrs: Vec<SocketAddr>) {
        if let Ok(mut map) = self.known_addrs.lock() {
            map.insert(*pubkey.as_bytes(), addrs);
        }
    }

    fn endpoint_addr_for(&self, pubkey: &NodePubkey) -> Option<iroh::EndpointAddr> {
        let id = iroh::PublicKey::from_bytes(pubkey.as_bytes()).ok()?;
        let addrs = self
            .known_addrs
            .lock()
            .ok()
            .and_then(|m| m.get(pubkey.as_bytes()).cloned())
            .unwrap_or_default();
        let mut ea = iroh::EndpointAddr::new(id);
        for a in addrs {
            ea = ea.with_ip_addr(a);
        }
        Some(ea)
    }

    /// Get or create the localhost TCP bridge for `pubkey`.
    async fn bridge_for(&self, pubkey: &NodePubkey) -> Option<SocketAddr> {
        let key = *pubkey.as_bytes();
        let mut bridges = self.bridges.lock().await;
        if let Some(b) = bridges.get(&key) {
            return Some(b.local_addr);
        }
        let target = self.endpoint_addr_for(pubkey)?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
        let local_addr = listener.local_addr().ok()?;
        let endpoint = self.endpoint.clone();
        let pubkey_hex = pubkey.to_string();
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let endpoint = endpoint.clone();
                let target = target.clone();
                let pubkey_hex = pubkey_hex.clone();
                tokio::spawn(async move {
                    // Dialing IS key verification: the QUIC handshake
                    // fails unless the responder holds the private
                    // key for this exact pubkey.
                    let conn = match endpoint.connect(target, ALPN).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(
                                target: "transport",
                                peer = %pubkey_hex,
                                error = %e,
                                "iroh bridge: dial failed"
                            );
                            return;
                        }
                    };
                    let (send, recv) = match conn.open_bi().await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                target: "transport",
                                peer = %pubkey_hex,
                                error = %e,
                                "iroh bridge: open_bi failed"
                            );
                            return;
                        }
                    };
                    pump(tcp, send, recv).await;
                });
            }
        });
        bridges.insert(
            key,
            Arc::new(BridgeHandle {
                local_addr,
                task,
            }),
        );
        Some(local_addr)
    }
}

/// Copy bytes both ways between a TCP socket and an iroh bi-stream
/// until both directions close.
async fn pump(
    tcp: tokio::net::TcpStream,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) {
    let (mut tcp_r, mut tcp_w) = tcp.into_split();
    let up = async {
        let _ = tokio::io::copy(&mut tcp_r, &mut send).await;
        let _ = send.finish();
    };
    let down = async {
        let _ = tokio::io::copy(&mut recv, &mut tcp_w).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut tcp_w).await;
    };
    tokio::join!(up, down);
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
        let Some(local) = self.bridge_for(&pubkey).await else {
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
    /// Spawn the accept loop. `forward_to` is the daemon's local
    /// listener (e.g. the internal-port axum router).
    pub fn spawn(endpoint: iroh::Endpoint, forward_to: SocketAddr) -> Self {
        let task = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
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
                    loop {
                        match conn.accept_bi().await {
                            Ok((send, recv)) => {
                                tokio::spawn(async move {
                                    match tokio::net::TcpStream::connect(forward_to).await {
                                        Ok(tcp) => pump(tcp, send, recv).await,
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
