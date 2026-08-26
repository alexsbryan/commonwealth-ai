// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one place this binary decides "which node am I" for the work atlas.
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

use commonwealth_core::ids::NodeId;

/// This workstation's atlas identity.
///
/// Always the ROOT data dir with the daemon's full precedence (`node_id` file
/// → `mesh.json` → generate), matching what the daemon itself resolves in
/// `bootstrap::resolve_self_node_id`. Do not inline this; a second spelling is
/// how the two broken call sites happened.
pub(crate) fn atlas_node_id() -> NodeId {
    sovereign_mesh::persist::resolve_self_node_id(&sovereign_cli_shared::dirs::sovereign_root())
}
