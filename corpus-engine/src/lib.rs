//! corpus-engine — shared corpus ingestion and index management.
//!
//! This crate handles:
//! - Recipe TOML parsing and validation
//! - Source data acquisition (download, crawl, local file)
//! - Content extraction (XML, JSON, HTML, CSV, Parquet, plaintext)
//! - Text chunking (paragraph, sentence, fixed, semantic)
//! - LanceDB indexing with IVF-PQ vector search and Tantivy full-text search
//! - Shard operations (extract, merge, stats) for distributed indexes
//!
//! It has zero dependency on Sovereign or Commonwealth crates.

pub mod acquirers;
pub mod atlas_traversal;
pub mod chunkers;
pub mod engine;
pub mod enrichment;
pub mod error;
pub mod extractors;
pub mod filters;
pub mod index;
pub mod progress;
pub mod recipe;
pub mod registry;
pub mod safety;
pub mod sharding;
pub mod sovereign_config;
pub mod testing;
pub mod types;
pub mod update;
pub mod yield_hook;

// SCIP call graph (gated on treesitter alongside the code intelligence stack).
#[cfg(feature = "treesitter")]
pub mod scip_graph;
#[cfg(feature = "treesitter")]
pub mod scip_export;
#[cfg(feature = "treesitter")]
mod scip_proto;

// Test / lint result stores (use rusqlite, gated alongside SCIP).
#[cfg(feature = "treesitter")]
pub mod test_results;
#[cfg(feature = "treesitter")]
pub mod lint_results;

// Working notes and project documentation stores.
#[cfg(feature = "treesitter")]
pub mod notes;
#[cfg(feature = "treesitter")]
pub mod project_docs;
// ATOS feature + milestone store.
#[cfg(feature = "treesitter")]
pub mod features;
// ATOS IMPLEMENTATION_PLAN.md index — see `plan_items.rs` for why
// this is separate from `notes.rs` (different query shape,
// different regeneration semantics).
#[cfg(feature = "treesitter")]
pub mod plan_items;
// DESIGN.md structural parser — used by the agent (via the
// `design_signals_extract` MCP tool in sovereign-tools), by
// sovereign-cli's `project found`/`project plan` for signal-gated
// question selection, and by solo-mode CLI prompts.
#[cfg(feature = "treesitter")]
pub mod design_signals;

// ─── Public API Re-exports ──────────────────────────────

pub use engine::{
    CancellationFlag, CancellationRegistry, CorpusDiskStatus, CorpusEngine, CustomAcquirerFn,
};
pub use enrichment::{
    Domain, EnrichmentProgress, FieldModelEngine, FieldModelStats, FieldSkeleton,
    reprocess_skeleton_failures,
};
pub use extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
pub use error::{Error, Result};
pub use filters::{
    build_filter_pipeline, compute_signature as compute_filter_signature, ComposeMode,
    DocumentFilter, FilterConfig, FilterPipeline, PageviewRankFilter, TitleListFilter,
};
pub use index::{
    CorpusIndex, FilterOverride, InsertChunk, ScopeMeta, StoredChunk, StoredChunkWithMetadata,
};
pub use progress::{
    IngestProgress, ManifestReconstructionReport, ProgressCallback, ReconstructionMethod,
    SourceFileManifest, SourceFileRecord, SourceFileStatus,
};
pub use recipe::{EnrichmentConfig, PrebuiltConfig, Recipe};
pub use registry::{RecipeRegistry, RegistryEntry, RegistryPrebuilt, RegistrySnapshot};
pub use testing::{
    AcquisitionResult, ChunkingResult, CorpusEstimate, ExtractionResult,
    FailedRecord, SampleChunk, TestOptions, TestQueryResult, TestReport,
    ValidationResult,
};
pub use sovereign_config::{RunnerConfig, SovereignConfig};
pub use types::{
    BatchEmbedFn, BuiltinCorpus, ChunkRange, CorpusKind, CorpusSpec, EmbedFn, IndexInfo,
    IndexStats, InferenceFn, IngestResult, ScoredChunk, ShardInfo,
};
pub use yield_hook::YieldHook;

#[cfg(feature = "treesitter")]
pub use scip_graph::{
    BlastEntry, BlastRadiusResult, OpenError, RebuildLock, ScipGraph, ScipGraphStats,
    ScipRefRecord, ScipSymbolRecord, SCHEMA_VERSION,
};

#[cfg(feature = "treesitter")]
pub use update::watcher_coordinator::{
    ActivityCallback, BackgroundWatcher, CoordinatorHandle, WatcherCoordinator, WatcherStatus,
};

#[cfg(feature = "treesitter")]
pub use test_results::TestResultStore;
#[cfg(feature = "treesitter")]
pub use lint_results::LintResultStore;
#[cfg(feature = "treesitter")]
pub use update::test_watcher::TestWatcher;
#[cfg(feature = "treesitter")]
pub use update::lint_watcher::LintWatcher;
#[cfg(feature = "treesitter")]
pub use update::project_index_watcher::ProjectIndexWatcher;

#[cfg(feature = "treesitter")]
pub use notes::{NoteRow, NoteScope, NoteStore, ScopeFilter, ToolCallLogRow};
#[cfg(feature = "treesitter")]
pub use project_docs::{DocResult, ProjectDocsStore, find_markdown_files};
#[cfg(feature = "treesitter")]
pub use features::{
    AtosRunRow, AtosToolEvent, FeatureRow, FeatureState, FeatureStore, MilestoneRow,
};
