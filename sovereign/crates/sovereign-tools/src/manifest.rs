//! Canonical tool-descriptor manifest.
//!
//! Middleware and CLI surfaces that need a static catalog of tool
//! descriptors (e.g. `commonwealth-api::tool_injector`) used to
//! hand-maintain parallel JSON-schema blocks, which drifted whenever
//! a tool's parameter shape changed — classic ARCH_PRINCIPLES §1
//! drift.
//!
//! This module constructs every sovereign tool *once* with throwaway
//! in-memory stores, pulls its `descriptor()`, and caches the result
//! for the process lifetime. Consumers pay the construction cost
//! (~10ms cold) on first access and nothing thereafter.
//!
//! The tools are instantiated with real `Arc<…Store>` handles because
//! `Tool::descriptor()` takes `&self`; the backing SQLite is
//! `:memory:` for every store so nothing touches disk. If a tool's
//! `descriptor()` ever starts mutating state (it shouldn't — that'd
//! be a bug), the throwaway stores absorb the damage and gc on
//! process exit.
//!
//! Gated on `treesitter` for the same reason the code tools are — the
//! manifest currently covers the code-intelligence + ATOS surface,
//! which is the only surface middleware cares about today.

#![cfg(feature = "treesitter")]

use std::path::Path;
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;

use corpus_engine::{
    CorpusEngine, EmbedFn, FeatureStore, LintResultStore, NoteStore, ProjectDocsStore,
    ScipGraph, TestResultStore,
};
use sovereign_core::traits::Tool;
use sovereign_core::types::ToolDescriptor;

use crate::code::{
    ArchiveFeatureTool, AtosVerifyTool, BlastRadiusTool, CheckDocPathsTool, CodeSearchTool,
    DeleteNoteTool, FindCalleesTool, FindCallersTool, GetLintOutputTool, GetRunOutputTool,
    IndexHealthChecker, LintStatusTool, ProjectContextTool, PromoteNoteTool,
    ProvisionFeatureTool, ReadNoteByIdTool, ReadNoteDigestTool, ReadNotesTool,
    RecentChangesTool, RecordAtosEventTool, ScipGraphHandle, SessionReflectionTool,
    SymbolLookupTool, TestStatusTool, WriteNoteTool, WriteRedteamFindingTool,
};

/// All canonical sovereign tool descriptors, computed once per
/// process and cached. Includes the code-intelligence surface and
/// ATOS lifecycle tools that agents see through MCP.
///
/// Return is `&'static [ToolDescriptor]` so callers can iterate /
/// filter without allocation — and so there's literally one source
/// of truth for the process lifetime.
pub fn all_descriptors() -> &'static [ToolDescriptor] {
    static CACHE: OnceLock<Vec<ToolDescriptor>> = OnceLock::new();
    CACHE.get_or_init(build_descriptors).as_slice()
}

/// Return the ATOS-critical subset of descriptors — the note-handling
/// tools that every sovereign-coder pipeline session needs
/// regardless of what the opencode plugin registered locally.
/// These are the ones currently hand-maintained in
/// `commonwealth-api::tool_injector`.
pub fn atos_critical_descriptors() -> Vec<ToolDescriptor> {
    // Renamed in Phase 2 of the CLI refactor: `read_notes` → `notes`,
    // `write_note` → `note`. The legacy ids stay reachable via the
    // `mcp_surface::MCP_TOOL_ALIASES` alias map; the descriptor's
    // canonical id is what we filter on.
    const IDS: &[&str] = &[
        "notes",
        "read_note_by_id",
        "read_note_digest",
        "note",
        "write_redteam_finding",
    ];
    all_descriptors()
        .iter()
        .filter(|d| IDS.contains(&d.id.as_str()))
        .cloned()
        .collect()
}

// ─── Internal: one-shot construction ────────────────────────────────

fn build_descriptors() -> Vec<ToolDescriptor> {
    // Every store opens in-memory so nothing touches disk. CorpusEngine
    // needs a data_dir but never reads from it during `descriptor()`.
    let tmp_data = std::env::temp_dir().join("sovereign-manifest-stub");
    let _ = std::fs::create_dir_all(&tmp_data);

    let embed: EmbedFn = Arc::new(|_text: &str| {
        Box::pin(async { Ok::<Vec<f32>, corpus_engine::Error>(vec![0.0; 768]) })
    });
    let engine = Arc::new(CorpusEngine::new(tmp_data.clone(), tmp_data.clone(), embed));

    let notes = Arc::new(
        NoteStore::open(Path::new(":memory:")).expect("manifest: in-memory notes"),
    );
    let features = Arc::new(
        FeatureStore::open(Path::new(":memory:")).expect("manifest: in-memory features"),
    );
    let lint = Arc::new(
        LintResultStore::open(Path::new(":memory:")).expect("manifest: in-memory lint"),
    );
    let tests = Arc::new(
        TestResultStore::open(Path::new(":memory:")).expect("manifest: in-memory tests"),
    );
    let docs = Arc::new(
        ProjectDocsStore::open(Path::new(":memory:")).expect("manifest: in-memory docs"),
    );
    let graph = ScipGraph::open_in_memory("manifest").expect("manifest: in-memory SCIP graph");
    let scip_handle: ScipGraphHandle = Arc::new(ArcSwap::from_pointee(graph));
    let health = Arc::new(IndexHealthChecker::new(Arc::clone(&scip_handle)));

    // Order mirrors project_cmd.rs's registration so the manifest
    // matches any diagnostics that reference the registry's ordering.
    vec![
        SymbolLookupTool::new(Arc::clone(&engine)).descriptor(),
        CodeSearchTool::new(Arc::clone(&engine)).descriptor(),
        RecentChangesTool::new(Arc::clone(&engine)).descriptor(),
        FindCalleesTool::new(Arc::clone(&engine), Arc::clone(&scip_handle))
            .with_health_checker(Arc::clone(&health))
            .descriptor(),
        FindCallersTool::new(Arc::clone(&engine), Arc::clone(&scip_handle))
            .with_health_checker(Arc::clone(&health))
            .descriptor(),
        BlastRadiusTool::new(Arc::clone(&scip_handle))
            .with_health_checker(Arc::clone(&health))
            .descriptor(),
        LintStatusTool::new(Arc::clone(&lint)).descriptor(),
        GetLintOutputTool::new(Arc::clone(&lint)).descriptor(),
        TestStatusTool::new(Arc::clone(&tests)).descriptor(),
        GetRunOutputTool::new(Arc::clone(&tests)).descriptor(),
        WriteNoteTool::new(Arc::clone(&notes)).descriptor(),
        ReadNotesTool::new(Arc::clone(&notes)).descriptor(),
        DeleteNoteTool::new(Arc::clone(&notes)).descriptor(),
        ReadNoteByIdTool::new(Arc::clone(&notes)).descriptor(),
        ReadNoteDigestTool::new(Arc::clone(&notes)).descriptor(),
        PromoteNoteTool::new(Arc::clone(&notes)).descriptor(),
        ProvisionFeatureTool::new(Arc::clone(&features)).descriptor(),
        ArchiveFeatureTool::new(Arc::clone(&features)).descriptor(),
        RecordAtosEventTool::new(Arc::clone(&features)).descriptor(),
        // AtosPlanEmitTool::new().descriptor(),
        // ^ intentionally commented out — markdown PLAN.md replaces
        //   the structured-JSON path the tool was designed for. Tool
        //   source kept for future use; manifest withdrawal is the
        //   minimum unwiring that hides it from the discovery surface.
        AtosVerifyTool::new().descriptor(),
        WriteRedteamFindingTool::new(Arc::clone(&notes)).descriptor(),
        SessionReflectionTool::new(Arc::clone(&notes)).descriptor(),
        ProjectContextTool::new(Arc::clone(&docs))
            .with_features(Arc::clone(&features))
            .descriptor(),
        CheckDocPathsTool::new().descriptor(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_non_empty() {
        assert!(
            !all_descriptors().is_empty(),
            "manifest must surface at least one tool"
        );
    }

    #[test]
    fn manifest_stable_across_calls() {
        // OnceLock caches; second call must return the same slice.
        let first = all_descriptors();
        let second = all_descriptors();
        assert_eq!(first.len(), second.len());
        assert!(std::ptr::eq(first.as_ptr(), second.as_ptr()));
    }

    #[test]
    fn atos_critical_subset_covers_the_five_pinned_ids() {
        // Phase 2 of the CLI refactor renamed `read_notes` → `notes`
        // and `write_note` → `note` at the descriptor layer. The
        // pinned ids here are the post-rename canonical names.
        let ids: Vec<String> = atos_critical_descriptors()
            .iter()
            .map(|d| d.id.clone())
            .collect();
        for expected in [
            "notes",
            "read_note_by_id",
            "read_note_digest",
            "note",
            "write_redteam_finding",
        ] {
            assert!(
                ids.contains(&expected.to_string()),
                "atos-critical subset missing '{expected}'; got {ids:?}"
            );
        }
    }

    #[test]
    fn every_manifest_id_is_unique() {
        let mut ids: Vec<String> = all_descriptors().iter().map(|d| d.id.clone()).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(
            total,
            ids.len(),
            "duplicate tool id in manifest (sorted: {ids:?})"
        );
    }
}
