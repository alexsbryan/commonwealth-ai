// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod atlas_context_manager;
pub mod atlas_peer_advice;
pub mod atlas_phase;
pub mod atlas_postinstall;
pub mod atlas_status;
pub mod atlas_view;
pub mod attached_document_search;
pub mod calendar;
pub mod catalog;
pub mod catalog_ingest;
pub mod code;
pub mod compute;
pub mod conv_tiered_provider;
pub mod corpus;
pub mod corpus_search;
pub mod corpus_store;
pub mod document;
pub mod document_asset;
pub mod document_operation;
pub mod email;
pub mod enrich;
pub mod enrichment_bootstrap;
pub mod enrichment_checker;
pub mod entity_graph;
pub mod epistemic;
pub mod extract;
pub mod file;
pub mod index_validator;
pub mod knowledge;
pub mod knowledge_lookup;
pub mod knowledge_view;
pub mod local_corpus;
pub mod mem_atlas;
pub mod mem_tree;
pub mod raptor_atlas;
pub mod raptor_checkpoint;
pub mod raptor_index;
pub mod summary_verify;
pub use sovereign_tools_base::read_csv;
pub use sovereign_tools_base::vector_mean;
pub mod wikipedia_fetch;
// `manifest` module retired 2026-05-22 — commonwealth-api now
// injects tool descriptors at construction time rather than pulling
// from a global static. See `commonwealth-api::middleware::tool_injector`
// and `context_injector` for the new shape.
pub use sovereign_tools_base::mcp;
pub mod mcp_surface;
pub mod notes;
pub mod parcel_analytics;
pub mod rag;
pub mod sec_edgar;
pub mod sec_facts;
/// The recipe-authoring tool bundle moved into the extractable
/// `sovereign-recipe-author` package; re-exported here as the `recipe_author`
/// module so every existing `sovereign_tools::recipe_author::…` path (and the
/// crate-root tool re-exports below) keeps resolving unchanged. The monolith
/// adapters that back its seams (`recipe_notes_adapter`, `recipe_tester_adapter`)
/// stay in this crate.
pub use sovereign_recipe_author as recipe_author;
pub use sovereign_tools_base::read_file;
pub use sovereign_tools_base::read_json;
/// Monolith-side adapter binding the real `NoteStore` to the `RecipeNotes`
/// contract the recipe-author tools depend on (keeps that bundle
/// corpus-engine-notes-free).
pub mod recipe_notes_adapter;
/// Monolith-side adapter binding the real `CorpusEngine::test_recipe` to the
/// `RecipeTester` contract the recipe validate/test tools depend on (keeps that
/// bundle corpus-engine-free).
pub mod recipe_tester_adapter;
pub use sovereign_tools_base::search;
pub use sovereign_tools_base::shell;
pub mod spec_watcher;
pub mod typed_call;
pub mod typed_extension;
pub use sovereign_tools_base::web;
pub use sovereign_tools_base::write_file;
pub use sovereign_tools_base::write_json;
pub use sovereign_tools_base::zip;

pub use attached_document_search::AttachedDocumentSearchTool;
#[cfg(feature = "treesitter")]
pub use code::drift_findings::DriftFindingsTool;
pub use code::facts_tool::FactsTool;
pub use code::session_state::SessionStateTool;
pub use code::AtosVerifyTool;
#[cfg(feature = "treesitter")]
pub use code::BlastRadiusTool;
#[cfg(feature = "treesitter")]
pub use code::BuildTool;
#[cfg(feature = "treesitter")]
pub use code::CapabilityMapTool;
#[cfg(feature = "treesitter")]
pub use code::CheckDocPathsTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::DesignSignalsExtractTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::DriftTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::ProjectContextTool;
#[cfg(feature = "treesitter")]
pub use code::SessionReflectionTool;
#[cfg(feature = "treesitter")]
pub use code::SpecTool;
#[cfg(feature = "treesitter")]
pub use code::SymbolLookupTool;
#[cfg(feature = "treesitter")]
pub use code::{
    compute_posture, hash_file, write_fingerprint, DriftFingerprint, DriftPosture,
    DriftPostureTool, PostureStatus, TopCritical, DEFAULT_NARRATIVES, FINGERPRINT_FILE,
};
pub use code::{overlaps_for_working_set, BriefingTool, OverlapAccumulator};
#[cfg(feature = "treesitter")]
pub use code::{ArchPostureTool, ArchReportTool};
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::{ArchiveFeatureTool, ProvisionFeatureTool, RecordAtosEventTool};
#[cfg(feature = "treesitter")]
pub use code::{
    AtosPlanEmitTool, PromoteNoteTool, ReadNoteByIdTool, ReadNoteDigestTool,
    WriteRedteamFindingTool,
};
#[cfg(feature = "treesitter")]
pub use code::{CapabilityFindingsTool, CapabilityPostureTool};
pub use code::{CodeSearchTool, RecentChangesTool};
#[cfg(feature = "treesitter")]
pub use code::{DeleteNoteTool, ReadNotesTool, RetireNoteTool, WriteNoteTool};
#[cfg(feature = "treesitter")]
pub use code::{FindCalleesTool, FindCallersTool, ScipGraphHandle};
#[cfg(feature = "treesitter")]
pub use code::{GetLintOutputTool, LintStatusTool};
#[cfg(feature = "treesitter")]
pub use code::{GetRunOutputTool, RunTestsTool, TestStatusTool};
#[cfg(feature = "treesitter")]
pub use code::{IndexHealth, IndexHealthChecker, StalenessLevel};
pub use document_asset::DocumentAssetManager;
pub use document_operation::DocumentOperationTool;
pub use epistemic::{ClaimSearchTool, EpistemicLandscapeTool};
pub use knowledge_lookup::{
    Evidence, EvidenceId, EvidenceKind, KindCounts, KnowledgeLookupResponse, KnowledgeLookupTool,
    SYSTEM_PROMPT as KNOWLEDGE_LOOKUP_SYSTEM_PROMPT,
    TOOL_DESCRIPTION as KNOWLEDGE_LOOKUP_TOOL_DESCRIPTION,
};
pub use recipe_author::{
    CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool, RecipeProject,
    RecipeReadTool, RecipeTestTool, RecipeValidateTool, RecipeWriteStructuredTool, RecipeWriteTool,
    RegistryBrowseTool, ResearchFindingTool,
};
pub use sovereign_core;
pub use wikipedia_fetch::WikipediaFetchTool;

/// The corpus/atlas-backed workflow tools that intentionally do NOT live in
/// `sovereign-tools-base` — each drags corpus-engine/LanceDB, which is exactly
/// what the base bundle exists to avoid. `sovereign-workflow-host`'s
/// `standard_registry` therefore registers only the pure tools; every call site
/// that runs workflows and wants the *full* surface (the CLI `workflow run` /
/// `corpus ingest`, the desktop run/ingest commands, the daemon living-trigger)
/// injects these through the runner's `extra_tools` slot.
///
/// Kept here — beside the tools themselves — so "which five" is stated once
/// rather than re-listed (and drifting) at each injection site. Registration
/// order is irrelevant: the registry keys on tool id and these ids are distinct
/// from the base set, so injecting them via `extra_tools` reproduces exactly the
/// pre-extraction 16-tool registry.
pub fn workflow_corpus_tools() -> Vec<Box<dyn sovereign_core::traits::Tool>> {
    vec![
        Box::new(extract::ExtractTool),
        Box::new(corpus_store::CorpusStoreTool),
        Box::new(corpus_search::CorpusSearchTool),
        Box::new(atlas_phase::gaps::AtlasGapsTool),
        Box::new(atlas_phase::tensions::AtlasTensionsTool),
    ]
}
