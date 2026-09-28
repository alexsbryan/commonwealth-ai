// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merged SCIP graph loader for code-intelligence tools.
//!
//! Lives here so both `sovereign-cli` (whose `tools_cmd` registry
//! opens the graph on every `svrn tools` invocation) and
//! `sovereign-cli-atos` (whose `project_cmd::cmd_serve` opens the
//! graph at daemon startup) can share one implementation.
//!
//! Pre-split, this fn lived at `project_cmd::load_merged_graph` and
//! was a flagged TODO at `tools_cmd/registry.rs:37` ("Blocked on
//! moving `load_merged_graph` to a neutral location").
//!
//! The loader itself moved to `corpus_engine_scip::merged_graph` (phase-b
//! pb-code-freshness), the one the daemon uses too; re-exported here at its
//! historical path.

pub use corpus_engine_scip::merged_graph::{
    load_merged_graph, MergedGraphSummary,
};
