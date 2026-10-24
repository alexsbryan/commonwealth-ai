// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which `[[compute.slot]]` roles a node still runs, and which the router
//! retired (ENGINE_SWAP_PLAN A7, P dec 2).
//!
//! The generate and embed child roles existed because the embedded engine
//! loaded GGUF weights in this process and a crash there took the daemon with
//! it. On `kind = "llama-server"` the router already runs every model in its
//! own supervised child, so a `[[compute.slot]]` naming either role would be a
//! second, unreachable copy of the weights: refused at startup, by name.
//!
//! The supervisor and the manager stay — the NER kind hosts nothing in a
//! child yet (`ner.rs::NO_CHILD`), and the distributed-primary / rpc-peer
//! work is es-f1's — so this module decides roles only.

use sovereign_contracts::setup_config::{ComputeSection, EngineKind};

/// The role whose engine is the embedded llama.cpp build.
pub const GENERATE_ROLE: &str = "generate";

/// The role whose engine is the embedded embedding model.
pub const EMBED_ROLE: &str = "embed";

/// Why a compute-child role cannot run on `kind = "llama-server"`.
///
/// `None` for a role the node may still declare, and for every engine but
/// the router: the embedded path is unchanged until `es-e2-flip-default`.
pub fn retired_on_router(engine: &EngineKind, role: &str) -> Option<String> {
    if *engine != EngineKind::LlamaServer {
        return None;
    }
    let why = match role {
        GENERATE_ROLE => {
            "its generation is a router model section (`[models]`); the router loads \
             and supervises that process itself"
        }
        EMBED_ROLE => {
            "its embedding is the router's `[models] embed` section (`[engine] embed_path` \
             on a hosted node); the router loads and supervises that process itself"
        }
        _ => return None,
    };
    Some(format!(
        "`[[compute.slot]]` role = \"{role}\" is retired on kind = \"llama-server\": {why}. \
         Remove the slot, or run kind = \"llama\"."
    ))
}

/// Every `[[compute.slot]]` under `section` that the router's engine has
/// retired, as `<name>: <reason>` — empty when the section can run.
pub fn retired_slots(engine: &EngineKind, section: &ComputeSection) -> Vec<String> {
    if !section.enabled {
        return Vec::new();
    }
    section
        .slot
        .iter()
        .filter_map(|slot| {
            retired_on_router(engine, &slot.role).map(|why| format!("{}: {why}", slot.name))
        })
        .collect()
}

#[cfg(test)]
#[path = "compute_roles_tests.rs"]
mod tests;
