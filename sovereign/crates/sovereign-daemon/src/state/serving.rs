//! Serving's part moved to its owner, `sovereign-serving-host`
//! (`REVIEW-build-daemon-parts`); this module re-exports it at the path the
//! daemon and its tests already name, and keeps the construction seed the
//! daemon assembles.
//!
//! Three of Serving's twenty fields stay in the daemon
//! (`sovereign_daemon::state::store::StorePart`): `inference_store` and
//! `peer_preferences` are backed by `commonwealth-state`, which the `serving`
//! package may not name (a third `[[exception]]` is the campaign's K4 kill
//! clause — `quality/campaigns/domains.toml:288`), and `rpc_shard_warmer`'s
//! trait method takes the daemon's `AppState`.

use std::sync::Arc;

pub use sovereign_serving_host::state::{
    PrincipalTally, RejectedNodeIdHeader, ServableModelFilesReader, ServingPart, SlotAliasesReader,
};

/// Everything Serving's part is constructed with (DC §4.2 "Construction is
/// staged, and parts are total"): the values that exist before the part is
/// built. The daemon gathers them and passes them to `AppState::new…`; a test
/// takes `Default`.
#[derive(Default)]
pub struct ServingSeed {
    /// The in-process inference service, when this node serves local chat.
    /// `None` on the standalone daemon and on storage-only nodes. Travels to
    /// `ServingPart::local_inference`.
    pub local_inference: Option<Arc<dyn super::LocalInferenceService>>,
    /// The worker-side auto-warm hook for distributed inference, installed
    /// alongside `local_inference`. `None` on a node that is not an inference
    /// worker. Travels to `StorePart::rpc_shard_warmer`, not to the host part:
    /// its trait method takes the daemon's `AppState`.
    pub rpc_shard_warmer: Option<Arc<dyn super::RpcShardWarmer>>,
}
