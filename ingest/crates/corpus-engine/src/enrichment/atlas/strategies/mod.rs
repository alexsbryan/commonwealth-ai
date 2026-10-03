// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AtlasIngestion` implementations.
//!
//! Each module here defines one strategy and a `register_into` hook
//! that the registry calls. Strategies stay self-contained — registry
//! itself imports nothing strategy-specific beyond the trait.

// Code-corpus branch of structure_first. Gated on the `treesitter`
// feature because it depends on `walkdir` + `scip_graph`, which are
// part of the same feature bundle.
#[cfg(feature = "treesitter")]
pub mod code_walk;
pub mod newsworthy_events;
pub mod structure_first;
