// SPDX-License-Identifier: AGPL-3.0-or-later
//! The serving venue ports: the identity reader and the contribution-ledger
//! port the serving host's `InferenceRouter` asks its host through.
//!
//! Moved out of `sovereign-serving-host::{venue_host, ledger}` (five-programs
//! fp-16, §12 D2 — the serving cluster is cmnwlth's own process and the
//! daemon dials it, so the port vocabulary sits at the contract floor both
//! ends already name). The host re-exports both traits at their historical
//! paths; construction stays with the serving process.

use std::sync::Arc;

use async_trait::async_trait;
use kernel_types::NodeId;

/// The contribution-ledger port, which the daemon implements over its
/// `ContributionEmitter` (`quality/DAEMON_CORE.md` §4.2 "The facts rule": no
/// context outside Fabric names `ContributionEmitter` — serving emits FACTS;
/// Fabric prices them).
pub trait LedgerEmitter: Send + Sync {
    /// Record that a peer-routed inference completed: `tokens_generated`
    /// tokens of `model_id` were received from `from_node`.
    fn record_inference_received(&self, from_node: &NodeId, model_id: &str, tokens_generated: u64);
}

/// The host-side companion to the scheduler's `VenueSource`
/// (`sovereign_scheduler::venue::VenueSource`, the candidate list alone).
///
/// `InferenceRouter` holds one of these as a constructor argument.
#[async_trait]
pub trait VenueHost: Send + Sync {
    /// This node's id. Stamped onto outbound manifest fetches via the
    /// `X-Node-Id` header so the peer can apply local-only affinity
    /// preferences before serializing the manifest. `None` when the daemon has
    /// not joined a mesh.
    async fn local_node_id(&self) -> Option<NodeId> {
        None
    }

    /// The contribution-ledger port, or `None` when this host has none.
    /// Default returns `None` — test stubs without a wired
    /// `ContributionEmitter` skip the emission entirely.
    async fn ledger_emitter(&self) -> Option<Arc<dyn LedgerEmitter>> {
        None
    }
}
