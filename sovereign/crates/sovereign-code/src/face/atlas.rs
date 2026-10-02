// SPDX-License-Identifier: AGPL-3.0-or-later
//! The work atlas's two process-level inputs, as the code program resolves
//! them: which node this is, and the store claims are written to. Moved
//! whole from sovereign-cli-dev's `atlas_identity` and `mesh_kv_client`
//! with code's face (pb-code-daemon-exit), so `svrn code mcp`, the stock
//! binary's composition and cli-dev's verbs keep one decider each; cli-dev
//! re-exports both at their historical paths.
//!
//! ## Identity
//! The one place the code program decides "which node am I" for the work atlas.
//!
//! Every surface that stamps atlas records — claims, observations, overlap
//! queries — must agree on this node's id, because the atlas's whole job is
//! telling YOUR edits apart from a PEER's. Get it wrong and the failure is
//! inverted rather than loud: self-filtering stops matching, your own edits
//! surface as a peer's, and the collision warning fires on every commit until
//! people learn to ignore it.
//!
//! It had drifted into three call sites with three answers:
//!
//!   - `tools_cmd::registry`  — `resolve_self_node_id(sovereign_root())`, correct
//!   - `project_cmd::serve`   — `load_or_generate_self_node_id(<root>/indexes)`
//!   - `code_cmd`             — `load_or_generate_self_node_id(<root>/indexes)`
//!
//! The latter two are the 2026-07-31 defect verbatim, still live: resolving
//! against `<root>/indexes` mints a SECOND identity for one workstation, and
//! `load_or_generate_self_node_id` skips the `mesh.json` fallback that
//! `resolve_self_node_id` documents as mandatory for exactly these surfaces.
//! It was repaired in `registry` and left in the other two, which is what one
//! decision living in three places buys you (`ARCH_PRINCIPLES` §10.6).

use kernel_types::NodeId;
use sovereign_contracts::setup_config::SetupConfig;
use sovereign_turn_client::rails_kv::{resolve_rails_base, RailsKv};

/// This workstation's atlas identity.
///
/// Always the ROOT data dir with the daemon's full precedence (`node_id` file
/// → `mesh.json` → generate), matching what the daemon itself resolves in
/// `bootstrap::resolve_self_node_id`. The decider lives in
/// `sovereign_contracts::node_identity` (moved there by fp-33 so this crate
/// stops linking the mesh substrate); do not inline it, a second spelling is
/// how the two broken call sites happened.
pub fn atlas_node_id() -> NodeId {
    sovereign_contracts::node_identity::resolve_self_node_id(
        &sovereign_contracts::rebrand::svrnmesh_root(),
    )
}

// ## Store
//
// The work atlas's store — dialed at the mesh's rails daemon (`cw-rails`)
// through the ONE sync `ReplicatedKv` client the daemon dials with,
// [`sovereign_turn_client::rails_kv::RailsKv`] (pb-atlas-kv, phase-b-7).
//
// Until pb-atlas-kv this module was that client's twin: a
// `reqwest::blocking` client on the daemon's `/v1/mesh/kv` proxy, which
// panicked when built inside the async `svrn tools` path. A second process
// never opens the mesh's store, it dials the process that owns it (fp-33).
//
// Absence is reported, never defaulted (principle 6): a cw-rails that is down
// surfaces on the first call as `ReplicatedKvError::Backend` naming the URL.
// Nothing here brings cw-rails up.

/// The `RailsKv` at the base `[daemon] rails_base` names, through that key's
/// one reader. Construction checks no presence and cannot fail.
pub fn atlas_kv() -> RailsKv {
    let daemon = match SetupConfig::load() {
        Ok(c) => c.daemon,
        Err(e) => {
            tracing::warn!(error = %e, "work atlas: no setup config; the rails base is the default");
            SetupConfig::unconfigured().daemon
        }
    };
    RailsKv::new(resolve_rails_base(&daemon))
}
