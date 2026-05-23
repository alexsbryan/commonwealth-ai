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
pub mod alignment_projector;
pub mod archaeology_eval;
pub mod atlas_canonical;
pub mod atlas_traversal;
pub mod canonical_sync;
pub mod meta_atlas;
pub mod stream_axes;
pub mod chunkers;
pub mod engine;
pub mod enrichment;
pub mod error;
pub mod extractors;
pub mod filters;
pub mod git_archaeology;
pub mod index;
pub mod pii;
pub mod progress;
pub mod recipe;
pub mod recipe_builtin;
mod recipe_parsing;
pub mod registry;
pub mod rough_edges;
pub mod safety;
pub mod sharding;
pub mod snapshot;
mod snapshot_restore;
pub mod sovereign_config;
pub mod testing;
pub mod types;
pub mod update;
pub mod yield_hook;

// SCIP call graph + tree-sitter-driven scanning. Only modules that
// actually depend on tree-sitter grammars or scip-proto live behind
// the `treesitter` gate. The rusqlite-backed stores moved to
// `stores` so callers that only need NoteStore/FeatureStore don't
// drag in 5 grammar crates.
#[cfg(feature = "treesitter")]
pub mod scip_graph;
#[cfg(feature = "treesitter")]
pub mod scip_export;
#[cfg(feature = "treesitter")]
mod scip_proto;

// Wikipedia link graph (Atlas-style enrichment, Layer 0) — rusqlite,
// no tree-sitter. Gated by `stores`.
#[cfg(feature = "stores")]
pub mod wikipedia_graph;

// Test / lint result stores (rusqlite). Gated by `stores`.
#[cfg(feature = "stores")]
pub mod test_results;
#[cfg(feature = "stores")]
pub mod lint_results;

// Working notes and project documentation stores (rusqlite).
#[cfg(feature = "stores")]
pub mod notes;
#[cfg(feature = "stores")]
mod notes_schema;
// NoteStore ↔ alignment-corpus sync. Same schema as `notes`.
#[cfg(feature = "stores")]
pub mod notes_sync;
#[cfg(feature = "stores")]
pub mod project_docs;
// ATOS feature + milestone store (rusqlite).
#[cfg(feature = "stores")]
pub mod features;
// ATOS IMPLEMENTATION_PLAN.md index (rusqlite).
#[cfg(feature = "stores")]
pub mod plan_items;
// DESIGN.md structural parser — uses tree-sitter, so still gated by
// `treesitter`.
#[cfg(feature = "treesitter")]
pub mod design_signals;

// ─── Public API Re-exports ──────────────────────────────

pub use engine::{
    CancellationFlag, CancellationRegistry, CorpusDiskStatus, CorpusEngine, CustomAcquirerFn,
    CustomExtractorFn,
};
pub use enrichment::{
    Domain, EnrichmentProgress, FieldModelEngine, FieldModelStats, FieldSkeleton,
    reprocess_skeleton_failures,
};
pub use enrichment::atlas::atlas_teardown;
pub use extractors::html_sections::MissReport as SectionMissReport;
pub use extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
pub use error::{Error, Result};
pub use filters::{
    build_filter_pipeline, compute_signature as compute_filter_signature, ComposeMode,
    DocumentFilter, FilterConfig, FilterPipeline, PageviewRankFilter, TitleListFilter,
};
pub use index::{
    read_provenance, set_provenance, CorpusIndex, CorpusProvenance, DedupeReport,
    EnrichmentChunkRow, FilterOverride, InsertChunk, NeighborWindow, ScopeMeta, StoredChunk,
    StoredChunkWithMetadata,
};
pub use progress::{
    IngestProgress, ManifestReconstructionReport, ProgressCallback, ReconstructionMethod,
    SourceFileManifest, SourceFileRecord, SourceFileStatus,
};
pub use recipe::{
    Comparison, DisplayMeta, DocFormat, EnrichmentConfig, EntityTypeDecl, FollowConfig,
    HttpMethod, PaginationStrategy, ParameterKind, ParameterSpec, ParameterValue, PatternDecl,
    PrebuiltConfig, Recipe, RelationshipTypeDecl, RequestTemplate, ResolvedParameters,
};
pub use registry::{RecipeRegistry, RegistryEntry, RegistryPrebuilt, RegistrySnapshot};
pub use testing::{
    AcquisitionResult, ChunkingResult, CorpusEstimate, ExtractionResult,
    FailedRecord, SampleChunk, TestOptions, TestQueryResult, TestReport,
    ValidationResult,
};
pub use sharding::{
    append_partition_to_canonical, merge_partitions_into_canonical, AppendReport,
    MergePhaseProgress, PartitionMergeReport,
};
pub use snapshot::{
    default_snapshot_filename, prebuilt_toml_snippet, publish_snapshot, read_local_index_meta,
    read_manifest_from_archive, snapshot_enrichment_path, snapshot_index_path,
    LocalIndexMetaSummary, PublishOptions, PublishOutcome, SnapshotManifest,
    SNAPSHOT_ENRICHMENT_PREFIX, SNAPSHOT_INDEX_PREFIX, SNAPSHOT_MANIFEST_FILENAME,
    SNAPSHOT_SCHEMA_VERSION,
};
pub use snapshot_restore::{restore_snapshot_archive, RestoreOutcome};
pub use sovereign_config::{RunnerConfig, SovereignConfig};
pub use types::{
    BatchEmbedFn, BuiltinCorpus, ChunkRange, CorpusKind, CorpusSpec, DedupPicker, EmbedFn,
    IndexInfo, IndexStats, InferenceFn, IngestResult, RerankConfig, RerankFn, ScoredChunk,
    ShardInfo,
};
pub use yield_hook::YieldHook;

#[cfg(feature = "treesitter")]
pub use scip_graph::{
    BlastEntry, BlastRadiusResult, OpenError, RebuildLock, ScipGraph, ScipGraphStats,
    ScipRefRecord, ScipSymbolRecord, SymbolRow, SCHEMA_VERSION,
};

#[cfg(feature = "stores")]
pub use wikipedia_graph::{
    ArticleRecord as WikipediaArticleRecord, IngestSummary as WikipediaGraphIngestSummary,
    Neighbor as WikipediaNeighbor, StalenessCaution as WikipediaStaleness, WikipediaGraph,
};

// `watcher_coordinator` re-exports — gated on `stores` since the
// coordinator depends only on `notify` (and notify lives in
// `stores`). The actual watcher implementations (lint/test/project
// index) are still treesitter-gated.
#[cfg(feature = "stores")]
pub use update::watcher_coordinator::{
    ActivityCallback, BackgroundWatcher, CoordinatorHandle, WatcherCoordinator, WatcherStatus,
};

#[cfg(feature = "stores")]
pub use test_results::TestResultStore;
#[cfg(feature = "stores")]
pub use lint_results::LintResultStore;
#[cfg(feature = "treesitter")]
pub use update::test_watcher::TestWatcher;
#[cfg(feature = "treesitter")]
pub use update::lint_watcher::LintWatcher;
#[cfg(feature = "treesitter")]
pub use update::project_index_watcher::ProjectIndexWatcher;

#[cfg(feature = "stores")]
pub use notes::{NoteRow, NoteScope, NoteSource, NoteStore, ScopeFilter, ToolCallLogRow};
#[cfg(feature = "stores")]
pub use project_docs::{DocResult, ProjectDocsStore, find_markdown_files};
#[cfg(feature = "stores")]
pub use features::{
    AtosRunRow, AtosToolEvent, FeatureRow, FeatureState, FeatureStore, MilestoneRow,
};
