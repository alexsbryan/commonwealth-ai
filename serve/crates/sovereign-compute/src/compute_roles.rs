// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which `[[compute.slot]]` roles a node still runs, and which are retired
//! (ENGINE_SWAP_PLAN A7, G2, P dec 2).
//!
//! The generate and embed child roles hosted the embedded llama.cpp engine,
//! so a crash in its weights took only the child. The router runs every model
//! in its own supervised child, and the embedded engine was deleted (G2), so a
//! `[[compute.slot]]` naming either role has nothing to host: refused at
//! startup, by name, on every engine. So is `rerank`, whose child loaded the
//! reranker through the same engine.
//!
//! The supervisor and the manager stay: the served kinds (`served_kind`) and
//! the mock run in children.

use sovereign_contracts::setup_config::ComputeSection;

/// The retired role that hosted the embedded llama.cpp generation engine.
pub const GENERATE_ROLE: &str = "generate";

/// The retired role that hosted the embedded embedding model.
pub const EMBED_ROLE: &str = "embed";

/// Why a compute-child role cannot run: `None` for a role a node may still
/// declare. `rerank`'s child loaded through the embedded engine too; the
/// router serves it now (`served_kind::RERANK_IN_ROUTER`).
pub fn retired(role: &str) -> Option<String> {
    matches!(
        role,
        GENERATE_ROLE | EMBED_ROLE | sovereign_contracts::rerank_kind::RERANK_ROLE
    )
    .then(|| retired_role(role))
}

/// The refusal for a retired role, in the one wording the startup check and
/// the child's own argument parse share.
pub fn retired_role(role: &str) -> String {
    let why = match role {
        GENERATE_ROLE => {
            "its generation is a router model section (`[models]`); the router loads \
             and supervises that process itself"
        }
        EMBED_ROLE => {
            "its embedding is the router's `[models] embed` section (`[engine] embed_path` \
             on a hosted node); the router loads and supervises that process itself"
        }
        _ => crate::served_kind::RERANK_IN_ROUTER,
    };
    format!(
        "`[[compute.slot]]` role = \"{role}\" is retired with the embedded engine: {why}. \
         Remove the slot."
    )
}

/// Every `[[compute.slot]]` under `section` that is retired, as
/// `<name>: <reason>` — empty when the section can run.
pub fn retired_slots(section: &ComputeSection) -> Vec<String> {
    if !section.enabled {
        return Vec::new();
    }
    section
        .slot
        .iter()
        .filter_map(|slot| retired(&slot.role).map(|why| format!("{}: {why}", slot.name)))
        .collect()
}

#[cfg(test)]
#[path = "compute_roles_tests.rs"]
mod tests;
