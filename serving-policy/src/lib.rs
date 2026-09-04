// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serving policy — how a host decides to serve a request.
//!
//! Two modules, one question. [`fair_sched`] decides WHOSE turn runs when the
//! host is contended (fair-share caps per principal, an EWMA queue-wait
//! prediction, reciprocity weighting); [`pipeline_aliases`] decides WHICH
//! pipeline an incoming alias resolves to. Both are pure policy over plain
//! data, with no I/O, no clock and no wire types.
//!
//! # Why it is its own crate
//!
//! Until 2026-09-03 both lived in `commonwealth-core`, and `fair_sched` was
//! the ONLY reason two runtime-tier crates depended on the mesh foundation at
//! all: `sovereign-inference`'s entire commonwealth reference was
//! `fair_sched::EtaEwma` (`embedded/model_slot.rs:29`), and
//! `sovereign-server`'s was `scheduler.rs:32` plus `reciprocity.rs:27`.
//! Moving the module ERASES both cross-family edges rather than relocating
//! them — the layer map's direction of travel is fewer `[[exception]]` rows,
//! and this one asks for none.
//!
//! The empty in-repo dependency list is the contract, not an accident. A
//! `commonwealth-*` or `sovereign-*` dep here would restore exactly the edges
//! the crate was minted to delete, so `quality/ARCH_LAYERS.toml` carries a
//! `[[forbid]]` block in each direction and boundary-gate pins the closure.

pub mod fair_sched;
pub mod pipeline_aliases;
