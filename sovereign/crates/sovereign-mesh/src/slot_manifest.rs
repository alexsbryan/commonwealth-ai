// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's [`SlotManifest`](sovereign_serving_host::slot_select::SlotManifest)
//! implementation over `sovereign-core`'s bundled manifest.
//!
//! It lives here, not in `sovereign-serving-host`, because the host may not
//! name `sovereign-core` (`sovereign/SERVING_BOUNDARY.md` rule 5; the serving
//! package's two grandfathered exceptions are `sovereign-inference` and
//! `commonwealth-core`, and a third means the boundary is drawn in the wrong
//! place). The host names the port; this side supplies the manifest.
//!
//! Split out of `inference_adapter` when that file moved host-side (domains
//! `REVIEW-build-serving-move-adapter`): the adapter receives this through its
//! constructor, and `peer_inference` supplies it to the host's
//! `oicp_synthesis::build_self_manifest`. One manifest reader, one name.

use sovereign_serving_host::slot_select::{SlotManifest, SlotManifestInfo};

/// The daemon's manifest reader: `sovereign-core`'s bundled `DEFAULT_MANIFEST`,
/// projected onto the two facts the host reads per loaded model file.
pub struct CoreSlotManifest;

impl SlotManifest for CoreSlotManifest {
    fn capabilities_for_file(&self, file: &str) -> Option<oicp_types::CapabilityProfile> {
        sovereign_core::models_manifest::DEFAULT_MANIFEST.capabilities_for_file(file)
    }

    fn info_for_file(&self, file: &str) -> Option<SlotManifestInfo> {
        sovereign_core::models_manifest::DEFAULT_MANIFEST
            .info_for_file(file)
            .map(|slot| SlotManifestInfo {
                capabilities: slot.capabilities,
                size_gb: slot.size_gb,
            })
    }
}
