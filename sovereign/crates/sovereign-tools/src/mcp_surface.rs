//! Shared MCP-server surface contract.
//!
//! Sovereign exposes its tool registry via two HTTP entry points:
//!
//! - `sovereign-mesh::mcp_router` — the embedded daemon's mount,
//!   active when `sovereign daemon` owns `:9741`.
//! - `sovereign-server::routes_mcp` — the standalone MCP server,
//!   active when `sovereign serve` (or the legacy `sovereign
//!   project serve`) is running.
//!
//! Both surfaces must agree on:
//!
//! - which canonical tool ids are exposed,
//! - which legacy ids alias to which new ones,
//! - whether a given request name should be rewritten before
//!   registry lookup.
//!
//! Letting each module define its own allowlist drifted previously
//! and is the hazard `.claude/plans/...` (the CLI refactor) wants
//! to eliminate. This module is the single source of truth — both
//! HTTP modules import from here.
//!
//! ## Phase 2 vs Phase 5 layering
//!
//! Phase 2 lays down the structural split (`MCP_TOOLS_ALWAYS` +
//! `MCP_TOOLS_SPEC_GATED`) but keeps the union policy unconditional
//! — every spec-gated tool is exposed alongside the always tools.
//! Phase 5 adds the file-presence gate that conditionally unions
//! the spec-gated portion based on `.sovereign/features/*/spec.md`
//! presence.
//!
//! Test coverage for the surface lives in this crate's `tests/`.

/// MCP tools exposed unconditionally once the workspace is set up.
///
/// The flat-namespace CLI refactor renamed the original ids to
/// short canonical forms (e.g. `find_callers` → `callers`). Old
/// ids stay reachable via [`MCP_TOOL_ALIASES`] — `tools/list` emits
/// a deprecated mirror entry for every alias and `tools/call`
/// rewrites old names before the registry lookup.
pub const MCP_TOOLS_ALWAYS: &[&str] = &[
    // Code intelligence (compiler-resolved, fast).
    "symbols", "callers", "callees", "blast",
    // Build/lint status. `build` is the canonical single-call
    // tool; `lint_status` + `get_lint_output` remain registered
    // for backward-compat during the alias window.
    "build", "lint_status", "get_lint_output",
    // Working notes (the audit's primary input).
    "note", "notes",
];

/// MCP tools that should only appear when a spec exists in the
/// workspace. Phase 2 populates this slot; Phase 5 wires the
/// file-presence gate that conditionally unions them into
/// [`MCP_TOOLS_ALWAYS`] at request time. Until then the union is
/// unconditional — a fresh repo with no `.sovereign/features/`
/// will see `spec`/`drift` advertise empty content.
pub const MCP_TOOLS_SPEC_GATED: &[&str] = &[
    "spec",
    "drift",
];

/// Tools registered in the in-process [`sovereign_core::ToolRegistry`]
/// but no longer exposed via MCP. The flat-namespace plan retires
/// these from the agent-facing surface — their value is folded into
/// `notes` / `spec` / `audit`. They stay registered so the CLI's
/// `sovereign tools call <name>` debugging surface still works.
///
/// Documentation only — exposure is decided by [`is_mcp_exposed`].
#[allow(dead_code)]
pub const MCP_TOOLS_RETIRED: &[&str] = &[
    "code_search", "recent_changes",
    "test_status", "run_tests", "get_run_output",
    "delete_note", "read_note_by_id", "read_note_digest",
    "promote_note", "suggest_note", "session_reflection",
    "check_doc_paths", "design_signals_extract",
    "provision_feature", "archive_feature",
    "record_atos_event", "write_redteam_finding",
    "project_context",
];

/// Backward-compat aliases mapping old MCP tool names → canonical
/// new names. `tools/list` emits a deprecated mirror for each alias
/// so cached clients keep working; `tools/call` rewrites the alias
/// before looking up the registry.
pub const MCP_TOOL_ALIASES: &[(&str, &str)] = &[
    ("find_callers", "callers"),
    ("find_callees", "callees"),
    ("blast_radius", "blast"),
    ("symbol_lookup", "symbols"),
    ("write_note", "note"),
    ("read_notes", "notes"),
];

/// Returns the canonical tool name for an incoming request. If
/// `name` is in [`MCP_TOOL_ALIASES`], returns the new name;
/// otherwise returns `name` unchanged. Borrowing the input avoids
/// allocation on the common (no-alias) path.
pub fn resolve_alias(name: &str) -> &str {
    for (old, new) in MCP_TOOL_ALIASES {
        if name == *old {
            return new;
        }
    }
    name
}

/// Returns true iff `canonical_name` (already alias-resolved) is
/// exposed via the MCP surface. Phase 2 unconditionally unions
/// `ALWAYS` and `SPEC_GATED`; Phase 5 will replace the union with
/// a file-presence-gated variant.
pub fn is_mcp_exposed(canonical_name: &str) -> bool {
    MCP_TOOLS_ALWAYS.contains(&canonical_name)
        || MCP_TOOLS_SPEC_GATED.contains(&canonical_name)
}

/// Render the MCP `tools/list` payload for a registry's descriptors.
///
/// Emits one entry per canonical exposed tool, plus one mirror
/// entry per [`MCP_TOOL_ALIASES`] whose target is exposed. The
/// mirror's description is prefixed with a deprecation marker so a
/// fresh client doesn't pick the alias by mistake; the input
/// schema is shared with the canonical entry so both call paths
/// validate identically.
///
/// Centralised here so the daemon and the standalone server agree
/// on the listing without subtle divergence.
pub fn render_tools_list(
    descriptors: &[sovereign_core::types::ToolDescriptor],
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for desc in descriptors {
        if is_mcp_exposed(&desc.id) {
            out.push(serde_json::json!({
                "name": desc.id,
                "description": desc.description,
                "inputSchema": desc.parameters,
            }));
        }
    }
    for (old, new) in MCP_TOOL_ALIASES {
        if !is_mcp_exposed(new) {
            continue;
        }
        let Some(canonical) = descriptors.iter().find(|d| d.id == *new) else {
            // Canonical handler not registered in this build (e.g.
            // SCIP-disabled). Skip — emitting an alias for an
            // unreachable handler would just produce confusing 503s.
            continue;
        };
        out.push(serde_json::json!({
            "name": old,
            "description": format!(
                "(deprecated alias for `{new}`) {}",
                canonical.description
            ),
            "inputSchema": canonical.parameters,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolve_alias` round-trips legacy ids to their new canonical
    /// form and is a no-op for already-canonical / unknown ids.
    #[test]
    fn resolve_alias_rewrites_legacy_ids() {
        assert_eq!(resolve_alias("find_callers"), "callers");
        assert_eq!(resolve_alias("find_callees"), "callees");
        assert_eq!(resolve_alias("blast_radius"), "blast");
        assert_eq!(resolve_alias("symbol_lookup"), "symbols");
        assert_eq!(resolve_alias("write_note"), "note");
        assert_eq!(resolve_alias("read_notes"), "notes");
        // Already canonical — returned verbatim.
        assert_eq!(resolve_alias("callers"), "callers");
        // Unknown — returned verbatim (the caller decides whether
        // to reject).
        assert_eq!(resolve_alias("not_a_tool"), "not_a_tool");
    }

    #[test]
    fn is_mcp_exposed_admits_renamed_canonical_ids() {
        for canonical in MCP_TOOLS_ALWAYS {
            assert!(
                is_mcp_exposed(canonical),
                "ALWAYS entry {canonical} should be exposed"
            );
        }
    }

    #[test]
    fn retired_ids_are_not_exposed() {
        for retired in MCP_TOOLS_RETIRED {
            assert!(
                !is_mcp_exposed(retired),
                "retired tool {retired} should not be MCP-exposed"
            );
        }
    }

    /// Every legacy alias must point to a canonical id that is in
    /// `MCP_TOOLS_ALWAYS` or `MCP_TOOLS_SPEC_GATED`. An alias that
    /// targets a non-exposed id is a configuration error — the
    /// `tools/list` mirror would advertise a name `tools/call`
    /// rejects.
    #[test]
    fn every_alias_target_is_exposed() {
        for (old, new) in MCP_TOOL_ALIASES {
            assert!(
                is_mcp_exposed(new),
                "alias {old} → {new} but {new} is not in ALWAYS or SPEC_GATED"
            );
        }
    }
}
