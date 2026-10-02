// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`SlotManifest`] implementation over the bundled model manifest
//! (`sovereign_contracts::models_manifest::DEFAULT_MANIFEST`).
//!
//! Moved here from `sovereign-daemon` (phase-b pb-serve-program): the
//! manifest lives in `sovereign-contracts`, which the host names, so the
//! daemon and `serve` read it through this ONE reader. The daemon's
//! re-export went with its serving-host edge (pb-serve-ranks): serve's
//! adapter and router are the reader's only users.

use crate::slot_select::{SlotManifest, SlotManifestInfo};

/// The bundled manifest's reader, projected onto the two facts the host
/// reads per loaded model file.
pub struct CoreSlotManifest;

impl SlotManifest for CoreSlotManifest {
    fn capabilities_for_file(&self, file: &str) -> Option<oicp_types::CapabilityProfile> {
        sovereign_contracts::models_manifest::DEFAULT_MANIFEST.capabilities_for_file(file)
    }

    fn info_for_file(&self, file: &str) -> Option<SlotManifestInfo> {
        sovereign_contracts::models_manifest::DEFAULT_MANIFEST
            .info_for_file(file)
            .map(|slot| SlotManifestInfo {
                capabilities: slot.capabilities,
                size_gb: slot.size_gb,
            })
    }
}
