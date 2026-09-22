// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host-side routing glue `InferenceRouter` reaches its ports through:
//! `ledger_emitter_for_venue` and the pinned-transport resolver. The ports
//! themselves — the identity reader (`VenueHost`) and the contribution-ledger
//! port (`LedgerEmitter`) — are contract-floor vocabulary now, re-exported
//! from `sovereign-contracts` (five-programs fp-16, §12 D2: the serving
//! cluster is cmnwlth's own process, so the port vocabulary sits at the
//! contract floor both ends name).
//!
//! Split out of `peer_inference.rs` (ARCH §3.1) and moved here from
//! `sovereign-mesh` by domains `REVIEW-build-serving-move-peer`: the host is
//! where the router lives, so the ports it holds live here too, and the daemon
//! (which owns the `EmbeddedDaemon`) implements them.
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_scheduler::venue::InferenceVenue;

pub use sovereign_contracts::venue_host::VenueHost;

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
