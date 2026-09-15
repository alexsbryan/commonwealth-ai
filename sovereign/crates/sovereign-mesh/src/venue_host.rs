// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh-side ports `MeshInferenceProvider` asks its host through: the
//! identity reader, the contribution-ledger emitter and the pinned-transport
//! resolver, plus their `EmbeddedDaemon` implementations. Split out of
//! `peer_inference.rs` (ARCH §3.1): these are the reaches that name
//! `commonwealth_core` / `commonwealth_state`, which the serving package may
//! not (`sovereign/SERVING_BOUNDARY.md`), so they stay in the mesh while
//! `peer_inference` is host-bound.
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_scheduler::venue::{InferenceVenue, VenueSource};

use crate::daemon::EmbeddedDaemon;

/// The host-side companion to [`VenueSource`].
///
/// `VenueSource` (`sovereign_scheduler::venue`) carries only the candidate
/// list. The two facts the old `PeerEndpointSource` also carried are host
/// concerns and ride here instead:
///
/// - the local node id, Fabric's identity READER (`quality/DAEMON_CORE.md`
///   §4.2 "Identity is a reader" — join adoption swaps the id inside a running
///   daemon, so a cached value goes stale);
/// - the contribution-ledger PORT (`sovereign_serving_host::ledger`), which
///   the daemon implements over its `ContributionEmitter`. The host mints the
///   fact from `RoutingOutcome` (`SERVING_BOUNDARY.md` (a)), so this trait no
///   longer builds a pre-filled emission per request.
///
/// `MeshInferenceProvider` holds one of these as a constructor argument.
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
    async fn ledger_emitter(
        &self,
    ) -> Option<Arc<dyn sovereign_serving_host::ledger::LedgerEmitter>> {
        None
    }
}

/// The daemon's implementation of the host's ledger port.
///
/// This is the only place the `commonwealth_state` emitter is named on the
/// serving path (`quality/DAEMON_CORE.md` §4.2 "The facts rule" — no context
/// outside Fabric names `ContributionEmitter`; Serving emits facts and Fabric
/// prices them). The host mints the fact from `RoutingOutcome` and this
/// records it.
struct DaemonLedger {
    emitter: commonwealth_state::ContributionEmitter,
}

impl sovereign_serving_host::ledger::LedgerEmitter for DaemonLedger {
    fn record_inference_received(
        &self,
        from_node: &kernel_types::NodeId,
        model_id: &str,
        tokens_generated: u64,
    ) {
        self.emitter.record(
            commonwealth_core::contributions::LedgerEventKind::InferenceReceived {
                from_node: *from_node,
                model_id: model_id.to_string(),
                tokens_generated,
            },
        );
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
) -> Option<Arc<dyn sovereign_serving_host::ledger::LedgerEmitter>> {
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

#[async_trait]
impl VenueSource for EmbeddedDaemon {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        EmbeddedDaemon::peer_inference_endpoints(self).await
    }
}

#[async_trait]
impl VenueHost for EmbeddedDaemon {
    async fn local_node_id(&self) -> Option<commonwealth_core::ids::NodeId> {
        EmbeddedDaemon::self_node_id(self).await
    }

    async fn ledger_emitter(
        &self,
    ) -> Option<Arc<dyn sovereign_serving_host::ledger::LedgerEmitter>> {
        let app_state = self.app_state().await?;
        Some(Arc::new(DaemonLedger {
            emitter: app_state.inner.contribution_emitter.clone(),
        }))
    }
}
