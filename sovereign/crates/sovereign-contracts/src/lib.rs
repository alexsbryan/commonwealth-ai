// SPDX-License-Identifier: AGPL-3.0-or-later
// Contract crate: the public surface IS the product — every pub item needs
// docs (count-ratcheted by lint-gate, never a hard deny).
#![warn(missing_docs)]
//! `sovereign-contracts` — the daemon↔package contract.
//!
//! The trait + type vocabulary a package (the workflow/recipe authoring stack)
//! needs to talk to a Sovereign daemon, carved out of `sovereign-core` so a
//! package can depend on the *contract* without dragging the runtime hub (and
//! through it llama.cpp, LanceDB, the mesh). The only workspace dependency is
//! `oicp-types` (re-exported below as `oicp`); everything else is a serde leaf.
//!
//! `sovereign-core` re-exports every item here at its historical paths
//! (`sovereign_core::{error, traits, registry, types, ...}`), so the ~200
//! existing importers are unaffected — this is a pure relocation.

/// The OICP protocol types, re-exported so contract types can reference
/// `crate::oicp::*` exactly as they did inside `sovereign-core`.
pub use oicp_types as oicp;

pub mod error;
pub mod health;
pub mod intent_policy;
pub mod mcp_config;
pub mod memory_config;
pub mod observer;
pub mod rebrand;
pub mod recipe;
pub mod registry;
pub mod setup_config;
pub mod skills;
pub mod slot_policy;
pub mod tool_result_cache;
pub mod traits;
pub mod types;

// Root re-exports mirroring the ones `sovereign-core` exposed, so
// `sovereign_contracts::{Error, Result, ToolRegistry, <Type>}` all resolve.
//
// The two module aliases below are BOUNDED globs (quality program R1,
// 2026-07-11): `traits` declares every item explicitly and `types` re-exports
// its submodules through explicit lists (types/mod.rs), so nothing can join
// this crate root without an explicit, reviewable edit — and api-gate
// snapshots the resulting surface. New pub items declared directly in
// types/mod.rs still flow through; the api snapshot is the net for those.
pub use error::{Error, Result};
pub use registry::ToolRegistry;
pub use traits::*;
pub use types::*;
