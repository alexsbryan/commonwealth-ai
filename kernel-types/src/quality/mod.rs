// SPDX-License-Identifier: AGPL-3.0-or-later
//! The quality-surface vocabulary — one parser for `quality/instruments.toml`.
//!
//! Beside [`crate::judgement`] on purpose: an instrument is the thing that
//! PRODUCES a [`Judgement`](crate::Judgement), and the two were already spoken
//! by the same three consumers (`xtask instrument-gate`, `svrn quality map`,
//! `svrn posture`). Three consumers, one parser — the same shape
//! `arch-layers` gives the layer map, and the reason is ARCH §10.6: a schema
//! read by three programs and parsed by three programs is three schemas.
//!
//! Feature-gated (`quality-registry`) so the crate's default four-dep budget
//! still holds for anyone lifting this leaf out of the monorepo — the same
//! contract `wire-fixture` follows for `serde_json`.
//!
//! This module knows NOTHING about the repo. It parses text and validates the
//! closed sets; resolving `quality/instruments.toml` to a path, and
//! cross-checking ids against `quality/sabotage/*.toml`, belong to the callers
//! that can see a checkout.

mod instruments;
mod render;

pub use render::{
    coverage_line, render_fidelity, render_layers, render_load_bearing, render_map, render_where,
    venues,
};

pub use instruments::{
    Baseline, BaselineKind, Cost, Coverage, Enforcement, Fidelity, Instrument, Kind,
    NotAnInstrument, Precondition, Registry, RunsIn,
};
