// SPDX-License-Identifier: AGPL-3.0-or-later
//! ATLAS_STORAGE_V2 — the eval-side seed/backend axes for `eval run`.
//!
//! Two orthogonal knobs the eval flips to verify the v2 migration against v1:
//! [`SeedMode`] (how `atlas_navigate` seeds — v1 cosine-over-the-bag vs v2 ANN)
//! and [`AtlasBackend`] (which on-disk store backs the `AtlasGraph` — v1 rkyv vs
//! the v2 `atoms.lance`).
//!
//! The ANN seeding itself now lives in PRODUCTION (the eval no longer forks it):
//! `sovereign_core::atlas_context::atlas_navigate_ann` does the navigate,
//! `build_persistent_ann_seed_table` writes the per-corpus
//! `atlas/atoms_ann.lance`, and `open_and_attach_ann_seed_table` loads it. The
//! `--atlas-seed ann` arm opens those same persistent tables and drives the
//! production navigate, so the gate exercises the daemon's exact code path.
//! Backfill the atlases first (`svrn atlas backfill-ann <corpus>`), then
//! run the gate — a graph without a table contributes name-match seeds only,
//! which the run banner flags.

/// Which seed source the retrieve probe uses for `atlas_navigate`. Part of the
/// probe's request, so it lives with it in `sovereign_contracts::probe`.
pub use sovereign_contracts::probe::SeedMode;
