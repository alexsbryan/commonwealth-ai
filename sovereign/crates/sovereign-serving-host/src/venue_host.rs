// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host-side ports `InferenceRouter` asks its host through: the
//! identity reader, the contribution-ledger port and the pinned-transport
//! resolver.
//!
//! Split out of `peer_inference.rs` (ARCH §3.1) and moved here from
//! `sovereign-mesh` by domains `REVIEW-build-serving-move-peer`: the host is
//! where the router lives, so the ports it holds live here too, and the daemon
//! (which owns the `EmbeddedDaemon`) implements them.
//!
//! [`VenueSource`](sovereign_scheduler::venue::VenueSource) carries only the
//! candidate list. The two facts the legacy port also carried are host
//! concerns and ride here instead:
//!
//! - the local node id, Fabric's identity READER (`quality/DAEMON_CORE.md`
//!   §4.2 "Identity is a reader" — join adoption swaps the id inside a running
//!   daemon, so a cached value goes stale);
//! - the contribution-ledger PORT ([`crate::ledger`]), which the daemon
//!   implements over its `ContributionEmitter`. The host mints the fact from
//!   `RoutingOutcome` (`sovereign/SERVING_BOUNDARY.md` (a)), so this trait no
//!   longer builds a pre-filled emission per request.
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_scheduler::venue::InferenceVenue;

/// The host-side companion to [`VenueSource`](sovereign_scheduler::venue::VenueSource).
///
/// `InferenceRouter` holds one of these as a constructor argument.
#[async_trait]
pub trait VenueHost: Send + Sync {
    /// This node's id. Stamped onto outbound manifest fetches via the
    /// `X-Node-Id` header so the peer can apply local-only affinity
    /// preferences before serializing the manifest. `None` when the daemon has
    /// not joined a mesh.
    async fn local_node_id(&self) -> Option<commonwealth_core::ids::NodeId> {
        None
    }

    /// The contribution-ledger port, or `None` when this host has none.
    /// Default returns `None` — test stubs without a wired
    /// `ContributionEmitter` skip the emission entirely.
    async fn ledger_emitter(&self) -> Option<Arc<dyn crate::ledger::LedgerEmitter>> {
        None
    }
}

/// The ledger port to attach to a peer-routed stream: the host's, unless the
/// venue is a pinned worker pod.
///
/// Pinned pods are not mesh members (spec §8 — no shared secret, no gossip, no
/// node id), so a "received from self" fact about one is meaningless. The
/// composite source used to suppress this by node id; the venue already
/// carries the fact ([`InferenceVenue::pinned_transport`]), so the decision
/// lives where the route is chosen.
pub(crate) async fn ledger_emitter_for_venue(
    host: &Arc<dyn VenueHost>,
    venue: &InferenceVenue,
) -> Option<Arc<dyn crate::ledger::LedgerEmitter>> {
    if venue.pinned_transport {
        return None;
    }
    host.ledger_emitter().await
}

/// Resolve the TLS-pinned transport handle for a pinned venue, by `node_id`.
///
/// The scheduler may not name `PinnedTransport`, so the handle does not travel
/// with [`InferenceVenue`]; the host's router holds this resolver instead and
/// the pinned source supplies it. Default: no pinned transports.
#[async_trait]
pub trait PinnedTransportResolver: Send + Sync {
    async fn resolve(
        &self,
        node_id: &commonwealth_core::ids::NodeId,
    ) -> Option<crate::pinned_transport::PinnedTransport>;
}

/// The default resolver: the mesh carries no pinned transports.
pub struct NoPinnedTransports;

#[async_trait]
impl PinnedTransportResolver for NoPinnedTransports {
    async fn resolve(
        &self,
        _node_id: &commonwealth_core::ids::NodeId,
    ) -> Option<crate::pinned_transport::PinnedTransport> {
        None
    }
}
