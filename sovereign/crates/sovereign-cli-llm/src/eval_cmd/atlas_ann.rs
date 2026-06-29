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
//! Backfill the atlases first (`sovereign atlas backfill-ann <corpus>`), then
//! run the gate — a graph without a table contributes name-match seeds only,
//! which the run banner flags.

/// Which seed source `run_question` uses for `atlas_navigate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedMode {
    /// v1: exact cosine over the in-memory embedding bag + `resolve_atom_id`.
    Cosine,
    /// v2: ANN over each corpus's persistent vector column — atom-ids returned
    /// directly, no per-query resolve. Requires the corpus to be backfilled.
    Ann,
}

/// Which on-disk store backs the `AtlasGraph` the eval loads — the
/// ATLAS_STORAGE_V2 Increment-C reader axis (orthogonal to [`SeedMode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtlasBackend {
    /// v1: the rkyv archive (`atoms.rkyv`), or convert-on-load from `atoms.json`.
    Rkyv,
    /// v2: the columnar store (`atoms.lance` + `edges.csr`), read through the
    /// production direct-read backend (`AtlasGraph::load_lance_from_disk` →
    /// `LancePreload`) — the same reader the daemon uses.
    Lance,
}
