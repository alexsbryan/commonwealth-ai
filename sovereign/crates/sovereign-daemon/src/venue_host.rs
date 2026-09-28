// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon-side implementations of the host's serving ports: the identity
//! reader, the contribution-ledger port and the venue source, plus the
//! `DeferredDaemon` handle that stands in for the daemon before it is
//! commissioned.
//!
//! The ports themselves now live in `sovereign_contracts::venue_host`
//! (fp-16; `InferenceRouter` holds them, and they moved host-side by domains
//! `REVIEW-build-serving-move-peer`). What stays here is the half that names
//! `commonwealth_core` / `EmbeddedDaemon`, which the
//! serving package may not (`sovereign/SERVING_BOUNDARY.md` rule 5): the
//! `EmbeddedDaemon` impls and the deferred handle.
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_contracts::venue::{InferenceVenue, VenueSource};
use sovereign_contracts::venue_host::{LedgerEmitter, ShardTransferLedger, VenueHost};

use crate::daemon::EmbeddedDaemon;
use crate::state::AppState;

/// The daemon's implementation of the host's ledger port.
///
/// This is the only place the contribution ledger is named on the
/// serving path (`quality/DAEMON_CORE.md` §4.2 "The facts rule" — no context
/// outside Fabric names `ContributionEmitter`; Serving emits facts and Fabric
/// prices them). The host mints the fact from `RoutingOutcome` and this
/// records it.
///
/// `LedgerEmitter` is sync and the port is async, so the write rides the
/// current runtime and its failure is traced — the shape of
/// `InferenceCache::set_model_info` (rails_client/ledger.rs).
struct DaemonLedger {
    emitter: Arc<dyn sovereign_mesh::ledger_port::ContributionLedgerPort>,
}

impl LedgerEmitter for DaemonLedger {
    fn record_inference_received(
        &self,
        from_node: &kernel_types::NodeId,
        model_id: &str,
        tokens_generated: u64,
    ) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                model = %model_id,
                "venue ledger: no runtime to carry the InferenceReceived write; it did not reach the ledger"
            );
            return;
        };
        let emitter = Arc::clone(&self.emitter);
        let kind = commonwealth_core::contributions::LedgerEventKind::InferenceReceived {
            from_node: *from_node,
            model_id: model_id.to_string(),
            tokens_generated,
        };
        let model_id = model_id.to_string();
        runtime.spawn(async move {
            if let Err(e) = emitter.record(kind).await {
                tracing::warn!(
                    model = %model_id,
                    error = %e,
                    "venue ledger: the InferenceReceived write did not reach the ledger"
                );
            }
        });
    }
}

/// The same daemon ledger as `sovereign-grants`' shard-transfer fact port
/// (fp-94, five-programs-53): grants reports the fact, this records it.
impl ShardTransferLedger for DaemonLedger {
    fn record_shard_transferred(
        &self,
        from_node: &kernel_types::NodeId,
        to_node: &kernel_types::NodeId,
        corpus_id: &str,
        bytes: u64,
    ) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                corpus = %corpus_id,
                "shard ledger: no runtime to carry the ShardTransferred write; it did not reach the ledger"
            );
            return;
        };
        let emitter = Arc::clone(&self.emitter);
        let kind = commonwealth_core::contributions::LedgerEventKind::ShardTransferred {
            from_node: *from_node,
            to_node: *to_node,
            corpus_id: corpus_id.to_string(),
            bytes,
        };
        let corpus_id = corpus_id.to_string();
        runtime.spawn(async move {
            if let Err(e) = emitter.record(kind).await {
                tracing::warn!(
                    corpus = %corpus_id,
                    error = %e,
                    "shard ledger: the ShardTransferred write did not reach the ledger"
                );
            }
        });
    }
}

/// The shard-transfer fact port over this node's contribution ledger, for the
/// `ShardManager`s and `FoldRecovery` the daemon hands `sovereign-grants`.
pub(crate) fn shard_transfer_ledger(state: &AppState) -> Arc<dyn ShardTransferLedger> {
    Arc::new(DaemonLedger {
        emitter: Arc::clone(&state.inner.store.contribution_emitter),
    })
}

#[async_trait]
impl VenueSource for EmbeddedDaemon {
    async fn candidates(&self) -> Vec<InferenceVenue> {
        EmbeddedDaemon::peer_inference_endpoints(self).await
    }
}

#[async_trait]
impl VenueHost for EmbeddedDaemon {
    async fn local_node_id(&self) -> Option<kernel_types::NodeId> {
        EmbeddedDaemon::self_node_id(self).await
    }

    async fn ledger_emitter(&self) -> Option<Arc<dyn LedgerEmitter>> {
        let app_state = self.app_state().await?;
        Some(Arc::new(DaemonLedger {
            emitter: Arc::clone(&app_state.inner.store.contribution_emitter),
        }))
    }
}

/// A handle to a daemon this host will commission later in its boot, usable as
/// a [`VenueSource`] in the meantime.
///
/// Production wiring is genuinely cyclic and always was: the daemon serves
/// peers through a [`InferenceRouter`](sovereign_serving_host::peer_inference::InferenceRouter),
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
    async fn local_node_id(&self) -> Option<kernel_types::NodeId> {
        EmbeddedDaemon::self_node_id(self.daemon.get()?).await
    }

    async fn ledger_emitter(&self) -> Option<Arc<dyn LedgerEmitter>> {
        self.daemon.get()?.ledger_emitter().await
    }
}
