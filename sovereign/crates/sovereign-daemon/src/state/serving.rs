//! Serving's part (`part.rs`, svrn's own since pb-svrn-serving-ports), and the
//! construction seed the daemon assembles.
//!
//! Two of Serving's fields are held apart
//! (`sovereign_daemon::state::store::StorePart`): `inference_store` and
//! `peer_preferences` are backed by `commonwealth-state`. The worker-side
//! rpc-warm is serve's since the flip (pb-mesh-exit-transport).

use std::sync::Arc;

mod part;

pub use part::{PrincipalTally, RejectedNodeIdHeader, ServingPart, SlotAliasesReader};
pub use sovereign_contracts::rpc_warm::ServableModelFilesReader;

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
}
