// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-authoring contract helpers shared between the daemon's bespoke ingest
//! path and the extractable workflow/recipe package.
//!
//! These are pure-CPU building blocks (no LanceDB, no llama.cpp, no document
//! I/O) that BOTH sides must agree on bit-for-bit, so a recipe authored against
//! the package behaves identically when run by the daemon, and the package can
//! compute them locally instead of shipping whole documents over MCP.
//!
//! The section detectors used to live here for that reason and no longer do.
//! Shared agreement does not require a shared *home* — it requires one
//! implementation, reachable DOWNWARD by everyone who needs it. Housing
//! `corpus-engine`'s own segmentation vocabulary in a sovereign crate made the
//! knowledge layer name a type owned by the layer above it, which was the only
//! reason `corpus-engine` depended on `sovereign-contracts` at all. They now
//! live in the `corpus-engine-sections` leaf (`regex` and nothing else), which
//! `corpus-engine` re-exports at `chunkers::sectioned::*` and the workflow
//! `SectionTool` reaches down to. Same one implementation, no back-edge.
//! (noun-convergence rung 2, 2026-08-20.)

pub mod json_to_toml;
pub mod notes;
pub mod paths;
pub mod registry;
pub mod schema;
pub mod testing;
pub mod url_template;
