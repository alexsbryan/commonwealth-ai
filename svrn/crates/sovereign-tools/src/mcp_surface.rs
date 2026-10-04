// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's MCP exposure list: the ids of svrn's own tools the daemon
//! advertises over `/mcp`. Code's ids, the legacy aliases and the
//! spec-presence gate moved to `sovereign_code::mcp_surface` (phase-b
//! pb-code-freshness: tool exposure as manifest data, one list per
//! program). While the daemon still mounts code's tools
//! (until pb-code-daemon-exit) it filters with the union of both lists.

/// svrn's tools exposed over MCP unconditionally.
pub const MCP_TOOLS_ALWAYS: &[&str] = &[
    // Catalog-driven on-demand article ingest. Surfaced so an MCP
    // client (or `mcp call wikipedia_fetch`) can drive the
    // chat-with-wikipedia loop directly when the agent's autonomous
    // tool-selection doesn't pick the catalog-hit follow-up.
    "wikipedia_fetch",
    // Recipe-author surface. The five existing tools drive the
    // author → validate → test loop; web_search / web_fetch supply
    // domain research; checkpoint / decision_log / capability_request
    // are the recipe-author-only escalation + audit surface. Together
    // they're the live tool set for the `recipe-author` skill — the
    // skill's `[tools] required` list is descriptive; MCP exposure is
    // the gate that actually lets the live agent loop reach them.
    "recipe_read",
    "recipe_write",
    "recipe_write_structured",
    "recipe_validate",
    "recipe_test",
    "registry_browse",
    "web_search",
    "web_fetch",
    "checkpoint",
    "decision_log",
    "capability_request",
    // API-shape probing + durable web findings — closed the loop
    // where the agent guessed at API contracts and never persisted
    // what it learned. probe_url returns one HTTP GET's structured
    // response (status, top-level JSON keys, pagination hint, body
    // excerpt). research_finding is the ResearchFinding writer the
    // v7 NoteStore migration left without a tool wrapping it.
    "probe_url",
    "research_finding",
    // Corpus / atlas plane (B:P9d). These operate on the HOST's corpora and
    // structural atlas — `corpus_search`/`corpus_store` read/write the local
    // LanceDB corpus, `atlas_gaps`/`atlas_tensions` query the structural atlas,
    // and `extract` pulls text out of a document. `standard_registry` dropped
    // them when the studio bundle carved out corpus-engine (B:P5); the daemon
    // still links it and registers them (see the daemon's `build_tool_registry`),
    // so a corpus-engine-free studio client reaches them here over MCP — which is
    // what lets it run the shipped `notebook` / `summarize` workflows.
    //
    // Caveat for `extract`: its `path` argument resolves on THIS host's
    // filesystem, so it is correct for a loopback / same-box client (the studio
    // bin's primary mode) and returns a clean "file not found" for a remote
    // client whose local paths the daemon can't see — a clear error, never a
    // silent mis-read.
    "corpus_store",
    "corpus_search",
    "atlas_gaps",
    "atlas_tensions",
    "extract",
    // `solve`, `solve_status` and `solve_cancel` are code's since
    // pb-meshapp-solve (`sovereign_code::mcp_surface`).
];

/// svrn's tools registered but not exposed over MCP; `svrn tools call <id>`
/// still reaches them. Documentation only — exposure is decided by
/// [`is_mcp_exposed`]. Code's retired ids and the usage census behind them
/// are in `sovereign_code::mcp_surface::MCP_TOOLS_RETIRED`.
#[allow(dead_code)]
pub const MCP_TOOLS_RETIRED: &[&str] = &[
    "atos_verify",
    "design_signals_extract",
    "provision_feature",
    "archive_feature",
    "record_atos_event",
    "project_context",
];

/// Returns true iff `canonical_name` is one of svrn's exposed tools.
pub fn is_mcp_exposed(canonical_name: &str) -> bool {
    MCP_TOOLS_ALWAYS.contains(&canonical_name)
}

/// Moved to the wire leaf beside the JSON-RPC envelope (phase-b pb-mcp);
/// re-exported at the historical path.
pub use oicp_types::mcp::{negotiate_mcp_protocol_version, MCP_SUPPORTED_PROTOCOL_VERSIONS};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_always_id_is_exposed() {
        for id in MCP_TOOLS_ALWAYS {
            assert!(is_mcp_exposed(id), "ALWAYS entry {id} should be exposed");
        }
    }

    #[test]
    fn retired_ids_are_not_exposed() {
        for id in MCP_TOOLS_RETIRED {
            assert!(
                !is_mcp_exposed(id),
                "retired tool {id} should not be MCP-exposed"
            );
        }
    }
}
