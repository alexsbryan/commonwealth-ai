// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end coverage for the Phase 2 MCP-surface contract.
//!
//! Phase 2 of the CLI refactor:
//!
//! - Renamed 6 tool ids (`find_callers` → `callers`,
//!   `symbol_lookup` → `symbols`, etc.) at the descriptor level.
//! - Centralised the MCP allowlist + alias map in
//!   [`sovereign_tools::mcp_surface`] so the daemon and the
//!   standalone server agree on the exposed surface.
//! - Added 3 new tools (`build`, `spec`, `drift`).
//!
//! The unit tests inside `mcp_surface.rs` cover the static contract
//! (alias map shape, retired ids excluded). This file covers the
//! dynamic behaviour: a populated `ToolRegistry` rendered via
//! `render_tools_list` produces exactly the canonical entries, and
//! renamed tools' `descriptor().id` matches the canonical name.
//!
//! Since 2026-08-17 the deprecated mirrors are no longer advertised
//! — they cost every session the duplicate schema of six tools. The
//! aliases remain accepted at dispatch via `resolve_alias`, and both
//! halves are asserted so neither can drift away alone.
//!
//! # Why this crate
//!
//! The check has two sides: the surface LIST lives in `sovereign_tools::
//! mcp_surface` and the tool IDS live in `sovereign-code` / `sovereign-atos`.
//! It ran from `sovereign-tools/tests/`, which made `sovereign-code`,
//! `sovereign-atos`, `corpus-engine-scip` and `corpus-engine-watchers`
//! dev-dependencies of `sovereign-tools` — four edges leaving the `svrn`
//! package closure for a test (`quality/ARCH_LAYERS.toml`, boundary-gate).
//! `sovereign-cli-dev` already holds both sides as NORMAL dependencies and is
//! where the registry under test is actually assembled
//! (`src/tools_cmd/registry.rs`), so the check moved here 2026-09-21 with no
//! assertion changed and no new dependency edge anywhere.

#![cfg(feature = "workbench")]

use std::sync::Arc;

use arc_swap::ArcSwap;
use corpus_engine_notes::NoteStore;
use corpus_engine_scip::ScipGraph;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::Tool;
use sovereign_tools::mcp_surface::{
    is_mcp_exposed, render_tools_list, resolve_alias, MCP_TOOLS_ALWAYS, MCP_TOOLS_RETIRED,
    MCP_TOOL_ALIASES,
};

fn empty_graph() -> sovereign_code::ScipGraphHandle {
    Arc::new(ArcSwap::from_pointee(
        ScipGraph::open_in_memory("test").expect("in-memory ScipGraph"),
    ))
}

fn empty_engine() -> Arc<corpus_engine::CorpusEngine> {
    let embed: corpus_engine::EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async {
            Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; corpus_engine::DEFAULT_EMBED_DIM])
        })
    });
    let dir = tempfile::tempdir().unwrap().keep();
    Arc::new(corpus_engine::CorpusEngine::new(dir.clone(), dir, embed))
}

/// Each renamed tool's `descriptor().id` is the canonical (new)
/// name, not the legacy id. The alias map at `mcp_surface` is the
/// only place the legacy spelling appears.
#[test]
fn renamed_tool_descriptor_ids_are_canonical() {
    let engine = empty_engine();
    let graph = empty_graph();

    let symbols = sovereign_code::SymbolLookupTool::new(
        Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
        Arc::clone(&graph),
    )
    .declared();
    assert_eq!(symbols.descriptor().id, "symbols");

    let callers = sovereign_code::FindCallersTool::new(
        Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
        Arc::clone(&graph),
    )
    .declared();
    assert_eq!(callers.descriptor().id, "callers");

    let callees = sovereign_code::FindCalleesTool::new(
        Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
        Arc::clone(&graph),
    )
    .declared();
    assert_eq!(callees.descriptor().id, "callees");

    let blast = sovereign_code::BlastRadiusTool::new(Arc::clone(&graph)).declared();
    assert_eq!(blast.descriptor().id, "blast");

    // Notes: no DB needed for descriptor introspection.
    let dir = tempfile::tempdir().unwrap();
    let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());
    let write = sovereign_code::WriteNoteTool::new(Arc::clone(&notes)).declared();
    assert_eq!(write.descriptor().id, "note");
    let read = sovereign_code::ReadNotesTool::new(Arc::clone(&notes)).declared();
    assert_eq!(read.descriptor().id, "notes");
}

/// `render_tools_list` against a registry containing all renamed
/// tools emits one canonical entry per tool and NO alias mirrors —
/// aliases live on the `tools/call` rewrite path only.
#[test]
fn render_tools_list_emits_canonical_only() {
    let engine = empty_engine();
    let graph = empty_graph();
    let dir = tempfile::tempdir().unwrap();
    let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(
        sovereign_code::SymbolLookupTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
            Arc::clone(&graph),
        )
        .declared(),
    ));
    registry.register(Box::new(
        sovereign_code::FindCallersTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
            Arc::clone(&graph),
        )
        .declared(),
    ));
    registry.register(Box::new(
        sovereign_code::FindCalleesTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
            Arc::clone(&graph),
        )
        .declared(),
    ));
    registry.register(Box::new(
        sovereign_code::BlastRadiusTool::new(Arc::clone(&graph)).declared(),
    ));
    registry.register(Box::new(
        sovereign_code::WriteNoteTool::new(Arc::clone(&notes)).declared(),
    ));
    registry.register(Box::new(
        sovereign_code::ReadNotesTool::new(Arc::clone(&notes)).declared(),
    ));

    let listed = render_tools_list(&registry.descriptors());
    let names: Vec<&str> = listed.iter().filter_map(|t| t["name"].as_str()).collect();

    // Canonical (renamed) ids appear.
    for canonical in &["symbols", "callers", "blast", "note", "notes"] {
        assert!(
            names.contains(canonical),
            "missing canonical {canonical} in {names:?}"
        );
    }

    // `callees` keeps its canonical descriptor id (asserted above) but left
    // the MCP surface on 2026-08-31: 0 calls across 190 sessions, against 37
    // for its sibling `callers`. Registered still, so `svrn tools call
    // callees` works — this asserts only that it is not ADVERTISED, which is
    // where the per-session cost was.
    assert!(
        !names.contains(&"callees"),
        "callees is registry-only now, not advertised: {names:?}"
    );

    // No legacy alias is advertised (changed 2026-08-17). The mirrors
    // duplicated the full schema of every renamed tool into every
    // session's context — measured at 9,435 chars ≈ 2,550 tokens —
    // to offer fresh clients a name they should never choose.
    for (legacy, _canonical) in MCP_TOOL_ALIASES {
        assert!(
            !names.contains(legacy),
            "deprecated alias {legacy} should not be advertised, got {names:?}"
        );
    }

    // Exactly the canonicals, nothing more: catches a mirror creeping
    // back in under a name this loop does not enumerate.
    assert_eq!(
        listed.len(),
        5,
        "expected only the 5 canonical entries, got {names:?}"
    );

    // Compatibility is preserved on the CALL path, not the list path.
    // Asserted here so a later cleanup cannot quietly delete the
    // rewrite along with the advertisement (ARCH §18.6).
    for (legacy, canonical) in MCP_TOOL_ALIASES {
        assert_eq!(
            resolve_alias(legacy),
            *canonical,
            "alias {legacy} must still resolve at dispatch time"
        );
    }
}

/// `resolve_alias` is the dispatch-time rewrite. An incoming MCP
/// request named with a legacy id should land at the canonical
/// handler; resolve_alias is the substring of that flow we can
/// test without spinning up an HTTP server.
#[test]
fn resolve_alias_matches_descriptor_ids() {
    for (legacy, canonical) in MCP_TOOL_ALIASES {
        let resolved = resolve_alias(legacy);
        assert_eq!(
            resolved, *canonical,
            "alias map says {legacy} → {canonical} but resolve_alias returned {resolved}"
        );
        // Resolving an already-canonical id is a no-op.
        assert_eq!(resolve_alias(canonical), *canonical);
    }
}

/// New Phase 2 tools (`build`, `spec`, `drift`) advertise the
/// expected canonical ids and are present in the appropriate
/// allowlist tier.
#[test]
fn new_tools_advertise_canonical_ids() {
    use sovereign_tools::mcp_surface::MCP_TOOLS_SPEC_GATED;

    // build → ALWAYS tier (exposed unconditionally).
    let dir = tempfile::tempdir().unwrap();
    let lint = Arc::new(
        corpus_engine_watchers::LintResultStore::open(&dir.path().join("lint.db")).unwrap(),
    );
    let build = sovereign_code::BuildTool::new(Arc::clone(&lint)).declared();
    assert_eq!(build.descriptor().id, "build");
    assert!(MCP_TOOLS_ALWAYS.contains(&"build"));

    // spec → SPEC_GATED tier.
    let spec = sovereign_code::SpecTool::new().declared();
    assert_eq!(spec.descriptor().id, "spec");
    assert!(MCP_TOOLS_SPEC_GATED.contains(&"spec"));

    // drift → SPEC_GATED tier. Ran only under sovereign-tools' `atos` feature
    // before the move; this crate pins `sovereign-atos`, so it always runs.
    let drift = sovereign_atos::tools::DriftTool::new().declared();
    assert_eq!(drift.descriptor().id, "drift");
    assert!(MCP_TOOLS_SPEC_GATED.contains(&"drift"));

    // Phase 2 unconditionally unions the two tiers, so all three
    // are exposed today; Phase 5 will gate spec/drift on
    // `.sovereign/features/*/spec.md` presence.
    for name in ["build", "spec", "drift"] {
        assert!(is_mcp_exposed(name), "{name} should be exposed");
    }
}

/// The planner's constrained-decoding schema is built from REAL tool
/// descriptors, and every tool's `parameters` becomes the `params`
/// sub-schema of its own `oneOf` branch. A tool whose schema is not a
/// typed object cannot be masked, and `plan_schema` refuses rather
/// than widening `params` back to "anything" — so this test is what
/// catches that refusal on descriptors the registry actually serves,
/// instead of on a hand-written fixture that can drift from them.
///
/// Coverage note, stated because a green tick here reads broader than
/// it is: this registry holds the six code-intel tools, not all forty
/// the server registers (the rest need a store, an inference provider
/// and the egress boundary). The tools NOT covered here still fail
/// loudly rather than quietly — a non-object schema refuses in
/// `plan_schema`, an uncompilable one 503s at the sampler (F1).
#[test]
fn plan_schema_builds_over_real_tool_descriptors() {
    let engine = empty_engine();
    let graph = empty_graph();
    let dir = tempfile::tempdir().unwrap();
    let notes = Arc::new(NoteStore::open(&dir.path().join("notes.db")).unwrap());

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(
        sovereign_code::SymbolLookupTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
            Arc::clone(&graph),
        )
        .declared(),
    ));
    registry.register(Box::new(
        sovereign_code::FindCallersTool::new(
            Arc::clone(&engine) as std::sync::Arc<dyn sovereign_code::CodeIndexSource>,
            Arc::clone(&graph),
        )
        .declared(),
    ));
    registry.register(Box::new(
        sovereign_code::BlastRadiusTool::new(Arc::clone(&graph)).declared(),
    ));
    registry.register(Box::new(
        sovereign_code::WriteNoteTool::new(Arc::clone(&notes)).declared(),
    ));

    let descriptors = registry.descriptors();
    let schema = sovereign_core::planner::plan_schema(&descriptors)
        .expect("every registered tool must declare a maskable `parameters` schema");

    let branches = schema["properties"]["steps"]["items"]["oneOf"]
        .as_array()
        .expect("steps.items.oneOf");
    for d in &descriptors {
        let branch = branches
            .iter()
            .find(|b| b["properties"]["tool_id"]["const"] == d.id.as_str())
            .unwrap_or_else(|| panic!("no branch for registered tool {}", d.id));
        assert_eq!(
            branch["properties"]["params"], d.parameters,
            "tool {} must be masked to ITS OWN declared arguments, verbatim — a \
             copy here would be a second decider and would drift from the tool",
            d.id
        );
    }
}

/// Each code tool's descriptor id must land on the surface list it was
/// classified into, or the tool is advertised over MCP yet not callable
/// (or retired yet still advertised).
///
/// These three assertions used to live beside each tool in
/// `sovereign-tools/src/code/`, then in `sovereign-tools/src/mcp_surface.rs`
/// when `code` became its own crate (FIVE_PROGRAMS §2) — a `#[cfg(test)]`
/// module, which is still a dev-dependency edge out of the `svrn` closure.
/// They are here because this is the one crate that holds the surface list
/// and the tools as normal dependencies. The
/// `assert_eq!(descriptor().id, "...")` half stayed with each tool, where it
/// needs nothing from here.
#[test]
fn code_tool_descriptor_ids_land_on_their_surface_list() {
    use sovereign_contracts::traits::Tool;
    use std::path::PathBuf;

    let capability_map =
        sovereign_code::CapabilityMapTool::with_indexes_dir(PathBuf::from("/nonexistent"))
            .declared();
    assert!(MCP_TOOLS_ALWAYS.contains(&capability_map.descriptor().id.as_str()));

    // Registry-only since 2026-08-31: both retired from the MCP surface on
    // usage evidence (arch_posture 1 call in 190 sessions, arch_report 0),
    // still reachable through `svrn tools call <id>`.
    for retired in [
        sovereign_code::ArchPostureTool::with_data_dir(PathBuf::from("/nonexistent")).declared(),
        sovereign_code::ArchReportTool::with_indexes_dir(PathBuf::from("/nonexistent")).declared(),
    ] {
        let id = retired.descriptor().id;
        assert!(MCP_TOOLS_RETIRED.contains(&id.as_str()), "{id} not retired");
        assert!(!is_mcp_exposed(&id), "{id} still exposed");
    }
}
