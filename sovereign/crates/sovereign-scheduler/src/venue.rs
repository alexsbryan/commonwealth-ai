// SPDX-License-Identifier: AGPL-3.0-or-later
//! The published candidate record and the port that supplies it.
//!
//! `sovereign/SERVING_BOUNDARY.md` "The five entries" (a): the ranked thing
//! is a `Venue`, and the roster reaches the ranker through ONE port —
//! [`VenueSource::candidates`]. Two facts that used to ride this port do not:
//!
//! - the local node id, which is Fabric's identity READER over
//!   `kernel_types::NodeId` and is handed to the router's construction
//!   (`quality/DAEMON_CORE.md` §4.2 "Identity is a reader" — join adoption
//!   swaps the id inside a running daemon, so a cached value goes stale);
//! - the contribution-ledger emission, which the host mints from
//!   `RoutingOutcome` instead: Serving emits facts, Fabric prices them.
//!
//! A venue also carries only WHETHER it is a pinned worker pod
//! ([`InferenceVenue::pinned_transport`]), never the TLS-pinned transport
//! handle: the scheduler may not name `PinnedTransport`
//! (`quality/ARCH_LAYERS.toml` rule 2, layer `contract` vs host `runtime`) and
//! never reads it — the host resolves the handle by `node_id` through its own
//! resolver.

use async_trait::async_trait;
use kernel_types::NodeId;
use oicp_types::BenchmarkResult;

/// A candidate the scheduler may rank.
///
/// Renamed from `PeerInferenceEndpoint` (registry `[[noun]]`, decided
/// 2026-09-14): the record covers the local slot and a remote lender alike, so
/// the noun is the venue, not the peer.
#[derive(Debug, Clone)]
pub struct InferenceVenue {
    pub node_id: NodeId,
    pub name: String,
    /// Candidate base URLs in try-order. Each is a
    /// `http://<ip>:9741/v1` prefix ready to hand to `RemoteApiProvider::new`.
    /// Multiple when the peer is dual-homed (WiFi + Tailscale); the wrapper
    /// tries them in order until one succeeds.
    pub base_urls: Vec<String>,
    /// Peer's gossiped `system_ram_gb`, a crude-but-correct-direction signal
    /// in the v1 routing heuristic.
    pub system_ram_gb: u32,
    /// Peer's gossiped baseline-model benchmark; `None` for older peers or one
    /// that has not completed its startup probe.
    pub benchmark: Option<BenchmarkResult>,
    /// Peer's gossiped self-reported concurrent inference count. Authoritative
    /// over the founder-local view.
    pub current_in_flight: Option<u32>,
    /// Peer's gossiped `inference_availability` (0.0–1.0; 1.0 = fully idle).
    pub inference_availability: Option<f32>,
    /// `MemberRecord::last_seen` for the gossip record the two load signals
    /// above were read from (unix seconds; `0` = unknown). The staleness half
    /// of the pair F1 measures.
    pub gossip_last_seen_unix: u64,
    /// Whether this venue is a pinned worker pod. The scheduler normalises a
    /// pinned pod's claim affinity because it has no users of its own. The
    /// transport handle itself is NOT here: the host resolves it by `node_id`.
    pub pinned_transport: bool,
}

/// The one port the roster crosses into Serving through.
///
/// The registry `[[noun]]` decided the name 2026-09-14. The two Fabric leaks
/// that rode the legacy trait — `local_node_id` and `ledger_emission_for` —
/// are gone; see the module doc.
#[async_trait]
pub trait VenueSource: Send + Sync {
    /// Everything routable right now. No filtering, ranking or ordering
    /// guarantee: the scheduler does all three.
    async fn candidates(&self) -> Vec<InferenceVenue>;
}
