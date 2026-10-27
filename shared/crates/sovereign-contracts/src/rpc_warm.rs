// SPDX-License-Identifier: AGPL-3.0-or-later
//! The servable-model-files allowlist the peer-fetch routes read
//! (`sovereign_compute::model_transfer`). The rpc-warm port that shared this
//! module was deleted with the mesh fabric (es-g4-delete-mesh-fabric).

use std::sync::Arc;

use arc_swap::ArcSwap;

/// The servable-model-files allowlist, published as a **reader** for the same
/// reason as svrn's `SlotAliasesReader`: it is seeded empty at construction and the
/// daemon publishes the boot list (and any `[models]` reload) through its own
/// handle while the peer-fetch routes read.
#[derive(Clone)]
pub struct ServableModelFilesReader(Arc<ArcSwap<Vec<std::path::PathBuf>>>);

impl ServableModelFilesReader {
    /// The allowlist right now.
    pub fn current(&self) -> Arc<Vec<std::path::PathBuf>> {
        self.0.load_full()
    }

    /// Publish a new allowlist (boot, or a `[models]` reload).
    pub fn publish(&self, files: Vec<std::path::PathBuf>) {
        self.0.store(Arc::new(files));
    }
}

impl Default for ServableModelFilesReader {
    fn default() -> Self {
        Self(Arc::new(ArcSwap::from_pointee(Vec::new())))
    }
}
