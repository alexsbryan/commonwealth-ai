// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `terminal` node's binding to its entry node, resolved per turn.
//!
//! A terminal holds no weights and forwards every turn and every embedding to
//! one bound node. Until 2026-08-31 that bind was an `http://host:9741/v1`
//! string written into `config.toml`, which ARCH §7.5 forbids for a stable
//! thing ("the address is a mutable attribute of the thing, never its name")
//! and which this mesh has a scar for — an iroh bridge's loopback port used as
//! peer identity produced 14 rebuilds in 21 minutes for a peer that had not
//! moved.
//!
//! Three separate failures came from that one shortcut, and they are all the
//! same failure:
//!
//! - **A moved lease redirects chat.** The terminal keeps POSTing to an address
//!   another machine now answers, and nothing errors.
//! - **No multi-homing.** `PeerTransport` ranks a peer's WiFi, Tailscale and
//!   IPv6 candidates; a literal string is one of them, chosen once.
//! - **An encrypted mesh has no plaintext ingress at all.** Its client API is
//!   forced loopback-only and peers arrive over the iroh acceptor
//!   (`commonwealth-transport::iroh`), so the address in the config answers
//!   nothing — which made the whole feature unreachable on the posture the
//!   fleet actually runs.
//!
//! Resolving the identity through the same `PeerEndpointSource` every other
//! peer-bound traffic class uses fixes all three at once, because all three
//! were the consequence of bypassing it.

use std::sync::Arc;

use async_trait::async_trait;
use commonwealth_core::ids::NodeId;
use sovereign_inference::remote::EndpointResolver;

use crate::peer_inference::PeerEndpointSource;

/// A terminal's entry node, named by mesh identity and located on demand.
pub struct EntryNodeEndpoint {
    /// The mesh view. `DeferredDaemon` in production, so this can be built
    /// before the daemon is commissioned — the terminal's provider is
    /// constructed during `load_provider`, which runs first.
    source: Arc<dyn PeerEndpointSource>,
    /// Who we are bound to. Never an address.
    node_id: NodeId,
}

impl std::fmt::Debug for EntryNodeEndpoint {
    /// Hand-written because `PeerEndpointSource` is not `Debug` and should not
    /// become so to satisfy a derive — the mesh view is a capability, not a
    /// value, and printing it would print the whole daemon.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryNodeEndpoint")
            .field("node_id", &self.node_id)
            .finish_non_exhaustive()
    }
}

impl EntryNodeEndpoint {
    /// Bind to `node_id`, resolving through `source` on every call.
    pub fn new(source: Arc<dyn PeerEndpointSource>, node_id: NodeId) -> Self {
        Self { source, node_id }
    }

    /// Parse the hex id a config carries. `None` on anything that is not a
    /// full 32-char node id — a truncated `svrn mesh status` display, a mesh
    /// name, a URL someone pasted into the wrong field.
    ///
    /// Refusing here rather than storing the string and failing at the first
    /// turn is the point: a config that cannot name a node is broken at load,
    /// far from a user waiting on an answer.
    pub fn parse(source: Arc<dyn PeerEndpointSource>, hex: &str) -> Result<Self, String> {
        let node_id = NodeId::from_hex(hex).ok_or_else(|| {
            format!(
                "'{hex}' is not a mesh node id (expected 32 hex characters). \
                 `svrn mesh status` prints ids truncated for reading; the full \
                 id is what `svrn setup --terminal <join-link>` records."
            )
        })?;
        Ok(Self::new(source, node_id))
    }

    /// The bound node's id.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }
}

#[async_trait]
impl EndpointResolver for EntryNodeEndpoint {
    /// The entry node's current `/v1` base, or `None` when it is not a
    /// reachable peer right now.
    ///
    /// `None` covers three states that all mean "cannot serve this turn here":
    /// the mesh has not converged yet (a terminal that just booted), the entry
    /// node is offline, or this node has not joined the mesh at all. They are
    /// not separated because the caller does the same thing with all three —
    /// refuse the turn and say the entry node is unreachable. What matters is
    /// that none of them silently produces a DIFFERENT node's address.
    async fn base_url(&self) -> Option<String> {
        let peers = self.source.peer_inference_endpoints().await;
        let peer = peers.into_iter().find(|p| p.node_id == self.node_id)?;
        // `base_urls` is already in try-order — `IpTransport` ranks a
        // multi-homed peer's candidates (IPv4/Tailscale before IPv6 ULA), and
        // `IrohTransport` returns the single bridge port that tunnels to it.
        // We take the top-ranked one.
        //
        // KNOWN LIMIT, stated rather than hidden: the peer-inference path tries
        // the whole list within one turn and this takes only the head, so a
        // terminal whose entry node is reachable ONLY on a lower-ranked
        // candidate fails the turn and re-resolves on the next. Carrying the
        // list into the provider means teaching its retry loop about candidate
        // order, which is a second implementation of a thing `peer_inference`
        // already owns (§10.6). Left as one resolution per call until a real
        // multi-homed terminal shows it is not enough.
        let chosen = peer.base_urls.into_iter().next()?;
        tracing::debug!(
            target: "transport",
            entry_node = %self.node_id,
            peer = %peer.name,
            endpoint = %chosen,
            "terminal: resolved entry node"
        );
        Some(chosen)
    }

    fn describe(&self) -> String {
        format!("entry node {}", self.node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::PeerInferenceEndpoint;

    /// A mesh view with a fixed peer set.
    struct Peers(Vec<PeerInferenceEndpoint>);

    #[async_trait]
    impl PeerEndpointSource for Peers {
        async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
            self.0.clone()
        }
    }

    fn peer(node_id: NodeId, name: &str, urls: &[&str]) -> PeerInferenceEndpoint {
        PeerInferenceEndpoint {
            node_id,
            name: name.to_string(),
            base_urls: urls.iter().map(|u| u.to_string()).collect(),
            system_ram_gb: 64,
            benchmark: None,
            current_in_flight: None,
            inference_availability: None,
            gossip_last_seen_unix: 0,
            transport: None,
        }
    }

    fn source(peers: Vec<PeerInferenceEndpoint>) -> Arc<dyn PeerEndpointSource> {
        Arc::new(Peers(peers))
    }

    /// The whole point: the address comes from the mesh at call time, so the
    /// same binding follows the node when its address changes.
    #[tokio::test]
    async fn the_binding_follows_the_node_across_addresses() {
        let entry = NodeId::generate();
        let before = EntryNodeEndpoint::new(
            source(vec![peer(entry, "halo", &["http://192.168.1.5:9741/v1"])]),
            entry,
        );
        assert_eq!(
            before.base_url().await.as_deref(),
            Some("http://192.168.1.5:9741/v1")
        );

        // Same node, new lease. Nothing about the binding changed.
        let after = EntryNodeEndpoint::new(
            source(vec![peer(entry, "halo", &["http://192.168.1.77:9741/v1"])]),
            entry,
        );
        assert_eq!(
            after.base_url().await.as_deref(),
            Some("http://192.168.1.77:9741/v1"),
            "an identity binding resolves to wherever the node is NOW"
        );
    }

    /// The failure an address binding could not detect: another machine
    /// answering where the entry node used to be. Resolution is by id, so the
    /// impostor is simply not the peer we asked for.
    #[tokio::test]
    async fn another_node_at_the_same_address_is_not_the_entry_node() {
        let entry = NodeId::generate();
        let squatter = NodeId::generate();
        let ep = EntryNodeEndpoint::new(
            source(vec![peer(squatter, "someone-else", &["http://192.168.1.5:9741/v1"])]),
            entry,
        );
        assert_eq!(
            ep.base_url().await,
            None,
            "the address is right and the node is wrong — that is a miss, not a hit"
        );
    }

    /// An unreachable entry node is reported as absent, never defaulted to
    /// some other peer that happens to be online (§18.3).
    #[tokio::test]
    async fn an_offline_entry_node_does_not_fall_back_to_another_peer() {
        let entry = NodeId::generate();
        let other = NodeId::generate();
        let ep = EntryNodeEndpoint::new(
            source(vec![peer(other, "unrelated", &["http://192.168.1.9:9741/v1"])]),
            entry,
        );
        assert_eq!(ep.base_url().await, None);
    }

    /// A terminal that has not joined yet sees no peers at all. Same answer,
    /// and it starts working on its own once gossip converges.
    #[tokio::test]
    async fn an_empty_mesh_resolves_to_nothing() {
        let entry = NodeId::generate();
        let ep = EntryNodeEndpoint::new(source(vec![]), entry);
        assert_eq!(ep.base_url().await, None);
    }

    /// The top-ranked candidate wins — `PeerTransport` already decided the
    /// order and this must not re-sort it.
    #[tokio::test]
    async fn the_transports_own_ranking_is_honoured() {
        let entry = NodeId::generate();
        let ep = EntryNodeEndpoint::new(
            source(vec![peer(
                entry,
                "halo",
                &[
                    "http://100.104.36.28:9741/v1",
                    "http://[fd7a:115c:a1e0::a3a:241c]:9741/v1",
                ],
            )]),
            entry,
        );
        assert_eq!(
            ep.base_url().await.as_deref(),
            Some("http://100.104.36.28:9741/v1")
        );
    }

    /// A truncated display id is refused at parse rather than stored and
    /// failed on later.
    #[test]
    fn a_truncated_display_id_is_not_a_binding() {
        let err = EntryNodeEndpoint::parse(source(vec![]), "44ae76142b0c3c723051ff")
            .expect_err("22 chars is the Display form, not the id");
        assert!(err.contains("32 hex characters"), "got: {err}");
    }

    #[test]
    fn a_full_hex_id_parses() {
        let id = NodeId::generate();
        let ep = EntryNodeEndpoint::parse(source(vec![]), &id.to_hex()).expect("round-trips");
        assert_eq!(ep.node_id(), id);
    }
}
