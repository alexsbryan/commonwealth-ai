// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub mod registry;
pub mod setup_config;
pub mod skills;
pub mod tool_result_cache;
pub mod traits;
pub mod types;

// Root re-exports mirroring the ones `sovereign-core` exposed, so
// `sovereign_contracts::{Error, Result, ToolRegistry, <Type>}` all resolve.
pub use error::{Error, Result};
pub use registry::ToolRegistry;
pub use traits::*;
pub use types::*;
