// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serving-policy arithmetic — the vocabulary both serving programs compute
//! with.
//!
//! Two modules, one question. [`fair_sched`] decides WHOSE turn runs when the
//! host is contended (fair-share caps per principal, an EWMA queue-wait
//! prediction, reciprocity weighting); [`pipeline_aliases`] decides WHICH
//! pipeline an incoming alias resolves to. Both are pure policy over plain
//! data, with no I/O, no clock and no wire types — the leaf test of
//! `quality/ARCH_LAYERS.toml`'s `[[package_leaf]]` row.
//!
//! # Why the arithmetic is its own crate
//!
//! Extracted from `serving-policy` (FIVE_PROGRAMS fp-17, under §12's leaf
//! rules) because the daemon's admission path (`sovereign-daemon`, a member of
//! the `svrn` package) computes the same share rule the [cmnwlth] serving
//! cluster does, and a package member may not name another package's crate.
//! The arithmetic is the shared part — both ends own it, like wire vocabulary,
//! except it is policy rather than wire format so §12 decision 3 does not put
//! it in `sovereign-contracts`. `serving-policy` re-exports both modules at
//! their historical paths; until 2026-09-03 they lived in `commonwealth-core`,
//! where `fair_sched` was the ONLY reason two runtime-tier crates depended on
//! the mesh foundation at all — the [[forbid]] rows on `serving-policy` in
//! `quality/ARCH_LAYERS.toml` keep that edge from coming back.

pub mod fair_sched;
pub mod pipeline_aliases;
