//! The three Serving fields the `serving` package may not name, held by the
//! daemon.
//!
//! DC §4.2 assigns all twenty of Serving's fields to `sovereign-serving-host`,
//! and seventeen went there at `REVIEW-build-daemon-parts`. These three cannot
//! cross the crate line:
//!
//! * `inference_store` (`commonwealth_state::store_adapter::InferenceStateStore`)
//!   and `peer_preferences` (`commonwealth_state::PeerPreferenceStore`) are
//!   backed by `commonwealth-state`, which is not a shared leaf of the
//!   `serving` package — a dep would be a third `[[exception]]`, and the
//!   campaign's K4 kill clause splits the cluster rather than widening the
//!   ledger (`quality/campaigns/domains.toml:288`);
//! * `rpc_shard_warmer`'s trait method ([`RpcShardWarmer::warm_shard`]) takes
//!   the daemon's `AppState`, so the trait cannot move to the host either.
//!
//! Both stores are backed by the node's `MeshStore`, so their home is the node
//! the daemon assembles; a later row can move them once the store arrives
//! through a port.

use std::sync::Arc;

use commonwealth_state::store_adapter::InferenceStateStore;
use commonwealth_state::PeerPreferenceStore;

use super::RpcShardWarmer;

/// The three Serving fields held by the daemon, read as `AppStateInner::store`.
pub struct StorePart {
    /// Inference plan, model info, ledger, and llama addresses — all via
    /// MeshStore.
    pub inference_store: InferenceStateStore,
    /// Per-peer preference store (Ostrom sanctions). Local-only,
    /// never gossiped — see
    /// `commonwealth_state::peer_preferences` for the structural
    /// invariants. The manifest endpoint reads this on every
    /// fetch to apply per-requester affinity multipliers.
    pub peer_preferences: PeerPreferenceStore,
    /// Worker-side auto-warm hook for distributed inference, passed at
    /// construction alongside `local_inference`; drives
    /// `POST /internal/rpc-warm`. `None` on a node that isn't an inference
    /// worker. See [`RpcShardWarmer`].
    pub rpc_shard_warmer: Option<Arc<dyn RpcShardWarmer>>,
}
