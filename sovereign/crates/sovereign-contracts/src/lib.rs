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
pub mod frame;
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

/// Test-only support shared across this crate's modules.
#[cfg(test)]
pub(crate) mod test_support {
    /// One process-wide lock for tests that read *or* mutate the **global**
    /// `HOME` env var. `rebrand`'s `projects_json_prefers_populated_branded_home`
    /// points `HOME` at a tempdir; `setup_config`'s tilde-expansion tests read
    /// `dirs::home_dir()` and assert against the REAL home. They compile into
    /// the SAME test binary and run concurrently, so the writer swaps `HOME`
    /// out from under the readers and they resolve a tempdir instead — observed
    /// 2026-08-10 as `extra_slots_expand_home_at_load` expecting
    /// `/Users/<me>/dev/big.gguf` and getting `/var/folders/.../svrnmesh-projects-json-<pid>/dev/big.gguf`.
    ///
    /// READERS must take it too, not just writers: excluding them is what makes
    /// this look like an unreproducible flake. Mirrors the same lock in
    /// `sovereign-desktop`'s `test_support`. Any new HOME-touching test in this
    /// crate must take it.
    ///
    /// Poison is ignored on purpose — a panicking test must not cascade into
    /// every other test in the binary.
    pub fn home_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
}
