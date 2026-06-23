// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod atlas_context_manager;
pub mod atlas_peer_advice;
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
pub mod corpus_store;
pub mod document;
pub mod document_asset;
pub mod document_operation;
pub mod email;
pub mod enrich;
pub mod enrichment_checker;
pub mod entity_graph;
pub mod epistemic;
pub mod file;
#[cfg(feature = "gliner-ner")]
pub mod gliner_ner;
pub mod index_validator;
pub mod knowledge;
pub mod knowledge_lookup;
pub mod knowledge_view;
pub mod local_corpus;
pub mod raptor_atlas;
pub mod raptor_checkpoint;
pub mod raptor_index;
pub mod wikipedia_fetch;
// `manifest` module retired 2026-05-22 — commonwealth-api now
// injects tool descriptors at construction time rather than pulling
// from a global static. See `commonwealth-api::middleware::tool_injector`
// and `context_injector` for the new shape.
pub mod mcp;
pub mod mcp_surface;
pub mod notes;
pub mod parcel_analytics;
pub mod rag;
pub mod recipe_author;
pub mod search;
pub mod shell;
pub mod spec_watcher;
pub mod typed_call;
pub mod typed_extension;
pub mod web;

pub use attached_document_search::AttachedDocumentSearchTool;
#[cfg(feature = "treesitter")]
pub use code::drift_findings::DriftFindingsTool;
pub use code::AtosVerifyTool;
#[cfg(feature = "treesitter")]
pub use code::BlastRadiusTool;
#[cfg(feature = "treesitter")]
pub use code::BuildTool;
#[cfg(feature = "treesitter")]
pub use code::CheckDocPathsTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::DesignSignalsExtractTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::ProjectContextTool;
#[cfg(feature = "treesitter")]
pub use code::SessionReflectionTool;
#[cfg(feature = "treesitter")]
pub use code::SymbolLookupTool;
#[cfg(feature = "treesitter")]
pub use code::{
    compute_posture, write_fingerprint, DriftFingerprint, DriftPosture, DriftPostureTool,
    PostureStatus, TopCritical, DEFAULT_NARRATIVES, FINGERPRINT_FILE,
};
#[cfg(feature = "treesitter")]
pub use code::{
    AtosPlanEmitTool, PromoteNoteTool, ReadNoteByIdTool, ReadNoteDigestTool, WriteRedteamFindingTool,
};
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::{ArchiveFeatureTool, ProvisionFeatureTool, RecordAtosEventTool};
pub use code::{CodeSearchTool, RecentChangesTool};
#[cfg(feature = "treesitter")]
pub use code::{DeleteNoteTool, ReadNotesTool, WriteNoteTool};
#[cfg(feature = "treesitter")]
pub use code::SpecTool;
#[cfg(all(feature = "treesitter", feature = "atos"))]
pub use code::DriftTool;
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
