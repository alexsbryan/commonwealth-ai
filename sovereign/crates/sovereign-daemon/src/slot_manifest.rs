// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's [`SlotManifest`](sovereign_serving_host::slot_select::SlotManifest)
//! reader, at its historical path. It moved to
//! `sovereign_serving_host::slot_manifest` (phase-b pb-serve-program): the
//! bundled manifest is `sovereign-contracts`', so the host can read it, and
//! the daemon and `serve` share the one reader.

pub use sovereign_serving_host::slot_manifest::CoreSlotManifest;
