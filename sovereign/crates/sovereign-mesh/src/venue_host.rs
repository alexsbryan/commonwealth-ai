// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon-side implementations of the host's serving ports: the identity
//! reader, the contribution-ledger port and the venue source, plus the
//! `DeferredDaemon` handle that stands in for the daemon before it is
//! commissioned.
//!
//! The ports themselves now live in `sovereign_serving_host::venue_host`
//! (`MeshInferenceProvider` holds them, and it moved host-side by domains
//! `REVIEW-build-serving-move-peer`). What stays here is the half that names
//! `commonwealth_core` / `commonwealth_state` / `EmbeddedDaemon`, which the
//! serving package may not (`sovereign/SERVING_BOUNDARY.md` rule 5): the
//! `EmbeddedDaemon` impls and the deferred handle.
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_scheduler::venue::{InferenceVenue, VenueSource};
use sovereign_serving_host::ledger::LedgerEmitter;
use sovereign_serving_host::venue_host::VenueHost;

use crate::daemon::EmbeddedDaemon;

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

impl LedgerEmitter for DaemonLedger {
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

    async fn ledger_emitter(&self) -> Option<Arc<dyn LedgerEmitter>> {
        let app_state = self.app_state().await?;
        Some(Arc::new(DaemonLedger {
            emitter: app_state.inner.contribution_emitter.clone(),
        }))
    }
}

/// A handle to a daemon this host will commission later in its boot, usable as
/// a [`VenueSource`] in the meantime.
///
/// Production wiring is genuinely cyclic and always was: the daemon serves
/// peers through a [`MeshInferenceProvider`](sovereign_serving_host::peer_inference::MeshInferenceProvider),
/// and that provider routes through the daemon. One of the two has to exist
/// first. Before 2026-08-24 the cycle was broken by leaving the daemon's
/// provider slot empty and punching it in afterwards, which is what made "no
/// provider installed" and "this host has no inference role" the same
/// observable state.
///
/// This breaks it in the other direction, and it is the ONLY late binding
/// left in the daemon's assembly. Before [`bind`](Self::bind) every method
/// answers exactly as a commissioned-but-stopped daemon does — no peers, no
/// node id, no ledger emission — so no caller can tell the two apart, and no
/// *capability* is deferred, only the daemon's own handle.
pub struct DeferredDaemon {
    daemon: std::sync::OnceLock<Arc<EmbeddedDaemon>>,
}

impl Default for DeferredDaemon {
    fn default() -> Self {
        Self::new()
    }
}

impl DeferredDaemon {
    pub fn new() -> Self {
        Self {
            daemon: std::sync::OnceLock::new(),
        }
    }

    /// Bind the commissioned daemon. Idempotent by `OnceLock`: a second call
    /// is a no-op, so a host cannot swap the routing target mid-flight.
    pub fn bind(&self, daemon: Arc<EmbeddedDaemon>) {
        if self.daemon.set(daemon).is_err() {
            tracing::warn!("DeferredDaemon already bound — ignoring rebind");
        }
    }

    /// The commissioned daemon, or `None` before [`bind`](Self::bind).
    /// Callers that run after boot (the admin-reload provider factory) can
    /// treat `None` as "reload arrived before the daemon existed", which is
    /// not reachable through the HTTP surface the daemon itself serves.
    pub fn get(&self) -> Option<Arc<EmbeddedDaemon>> {
        self.daemon.get().cloned()
    }
}

#[async_trait]
impl VenueSource for DeferredDaemon {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        match self.daemon.get() {
            Some(d) => EmbeddedDaemon::peer_inference_endpoints(d).await,
            None => Vec::new(),
        }
    }
}

#[async_trait]
impl VenueHost for DeferredDaemon {
    async fn local_node_id(&self) -> Option<commonwealth_core::ids::NodeId> {
        EmbeddedDaemon::self_node_id(self.daemon.get()?).await
    }

    async fn ledger_emitter(&self) -> Option<Arc<dyn LedgerEmitter>> {
        self.daemon.get()?.ledger_emitter().await
    }
}
