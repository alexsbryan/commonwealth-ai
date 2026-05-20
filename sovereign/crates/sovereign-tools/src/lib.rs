pub mod atlas_context_manager;
pub mod atlas_peer_advice;
pub mod atlas_postinstall;
pub mod atlas_status;
pub mod atlas_view;
pub mod wikipedia_fetch;
pub mod calendar;
pub mod catalog;
pub mod catalog_ingest;
pub mod code;
pub mod compute;
pub mod corpus;
pub mod document;
pub mod document_asset;
pub mod document_operation;
pub mod email;
pub mod enrich;
pub mod enrichment_checker;
pub mod epistemic;
pub mod file;
pub mod index_validator;
pub mod knowledge;
pub mod knowledge_lookup;
pub mod knowledge_view;
pub mod local_corpus;
#[cfg(feature = "treesitter")]
pub mod manifest;
pub mod mcp;
pub mod mcp_surface;
pub mod notes;
pub mod spec_watcher;
pub mod rag;
pub mod recipe_author;
pub mod search;
pub mod shell;
pub mod web;

pub use code::{CodeSearchTool, RecentChangesTool, SymbolLookupTool};
pub use knowledge_lookup::{
    Evidence, EvidenceId, EvidenceKind, KindCounts, KnowledgeLookupResponse,
    KnowledgeLookupTool, SYSTEM_PROMPT as KNOWLEDGE_LOOKUP_SYSTEM_PROMPT,
    TOOL_DESCRIPTION as KNOWLEDGE_LOOKUP_TOOL_DESCRIPTION,
};
#[cfg(feature = "treesitter")]
pub use code::{FindCalleesTool, FindCallersTool, ScipGraphHandle};
#[cfg(feature = "treesitter")]
pub use code::{GetRunOutputTool, RunTestsTool, TestStatusTool};
#[cfg(feature = "treesitter")]
pub use code::{GetLintOutputTool, LintStatusTool};
#[cfg(feature = "treesitter")]
pub use code::BuildTool;
#[cfg(feature = "treesitter")]
pub use code::{DriftTool, SpecTool};
#[cfg(feature = "treesitter")]
pub use code::{
    compute_posture, write_fingerprint, DriftFingerprint, DriftPosture, DriftPostureTool,
    PostureStatus, TopCritical, DEFAULT_NARRATIVES, FINGERPRINT_FILE,
};
#[cfg(feature = "treesitter")]
pub use code::drift_findings::DriftFindingsTool;
#[cfg(feature = "treesitter")]
pub use code::{WriteNoteTool, ReadNotesTool, DeleteNoteTool};
#[cfg(feature = "treesitter")]
pub use code::BlastRadiusTool;
#[cfg(feature = "treesitter")]
pub use code::{IndexHealth, IndexHealthChecker, StalenessLevel};
#[cfg(feature = "treesitter")]
pub use code::ProjectContextTool;
#[cfg(feature = "treesitter")]
pub use code::SessionReflectionTool;
#[cfg(feature = "treesitter")]
pub use code::CheckDocPathsTool;
#[cfg(feature = "treesitter")]
pub use code::{
    ArchiveFeatureTool, AtosPlanEmitTool, PromoteNoteTool, ProvisionFeatureTool, ReadNoteByIdTool,
    ReadNoteDigestTool, RecordAtosEventTool, WriteRedteamFindingTool,
};
#[cfg(feature = "treesitter")]
pub use code::DesignSignalsExtractTool;
pub use code::AtosVerifyTool;
pub use document_asset::DocumentAssetManager;
pub use document_operation::DocumentOperationTool;
pub use wikipedia_fetch::WikipediaFetchTool;
pub use epistemic::{ClaimSearchTool, EpistemicLandscapeTool};
pub use recipe_author::{
    CapabilityRequestTool, CheckpointTool, DecisionLogTool, ProbeUrlTool,
    RecipeProject, RecipeReadTool, RecipeTestTool, RecipeValidateTool,
    RecipeWriteStructuredTool, RecipeWriteTool, RegistryBrowseTool,
    ResearchFindingTool,
};
pub use sovereign_core;
