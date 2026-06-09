// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub mod asset_store;
// archaeology_eval, git_archaeology, rough_edges moved to
// corpus-engine-archaeology (step 4 of the decomposition plan,
// 2026-05-23). corpus-engine has no internal users of these
// modules — they only ever served the CLI + tool layer.
pub mod atlas_canonical;
pub mod atlas_traversal;
pub mod canonical_sync;
pub mod chunkers;
pub mod engine;
pub mod enrichment;
pub mod error;
pub mod extractors;
pub mod filters;
pub mod freshness;
pub mod index;
pub mod meta_atlas;
pub mod pii;
pub mod progress;
pub mod recipe;
pub mod recipe_builtin;
mod recipe_parsing;
pub mod registry;
pub mod safety;
pub mod sharding;
pub mod snapshot;
mod snapshot_restore;
pub mod sovereign_config;
pub mod stream_axes;
pub mod testing;
pub mod types;
pub mod update;
pub mod yield_hook;

// SCIP call graph + per-language exporter dispatch live in their
// own crate now (`corpus-engine-scip`, carved out 2026-05-23). The
// `scip_graph` / `scip_export` paths under corpus-engine remain as
// re-exports for backward compatibility during the consumer
// migration — see the `pub use corpus_engine_scip` block below.
//
// Only modules that actually depend on tree-sitter grammars live
// behind the `treesitter` gate now. The rusqlite-backed stores
// moved to `stores` so callers that only need NoteStore/FeatureStore
// don't drag in 5 grammar crates.

// Wikipedia link graph (Atlas-style enrichment, Layer 0) — rusqlite,
// no tree-sitter. Gated by `stores`.
#[cfg(feature = "stores")]
pub mod wikipedia_graph;

// Test / lint result stores (rusqlite). Gated by `stores`.
#[cfg(feature = "stores")]
pub mod lint_results;
#[cfg(feature = "stores")]
pub mod test_results;

// NoteStore + project_docs live in `corpus-engine-notes` (carved out
// 2026-05-23, step 3 of the decomposition plan). `notes_sync` (the
// bridge between corpus-engine's `ExtractedDoc` and the new crate's
// `NoteStore`) stays here to avoid a cyclic workspace dep — the bridge
// translates one direction, corpus-engine→notes, which puts it
// naturally on the corpus-engine side. External consumers depend on
// `corpus-engine-notes` directly.
#[cfg(feature = "stores")]
pub mod notes_sync;
// ATOS state (features, plan_items, design_signals) lives in
// `corpus-engine-atos` (carved out 2026-05-23, step 2 of the
// decomposition plan). corpus-engine itself doesn't consume any of
// them; consumers (sovereign-atos, sovereign-cli-dev, sovereign-tools,
// commonwealth-api) depend on `corpus-engine-atos` directly.

// ─── Public API Re-exports ──────────────────────────────

pub use engine::{
    CancellationFlag, CancellationRegistry, CorpusDiskStatus, CorpusEngine, CustomAcquirerFn,
    CustomExtractorFn,
};
pub use enrichment::atlas::atlas_teardown;
pub use enrichment::{
    reprocess_skeleton_failures, Domain, EnrichmentProgress, FieldModelEngine, FieldModelStats,
    FieldSkeleton,
};
pub use error::{Error, Result};
pub use extractors::html_sections::MissReport as SectionMissReport;
pub use extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
pub use filters::{
    build_filter_pipeline, compute_signature as compute_filter_signature, ComposeMode,
    DocumentFilter, FilterConfig, FilterPipeline, PageviewRankFilter, TitleListFilter,
};
pub use index::raptor::{
    build_raptor_index, read_raptor_meta, search_raptor_summaries, RaptorHit, RaptorIndexMeta,
    RaptorSummaryRow,
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
    Comparison, DisplayMeta, DocFormat, EnrichmentConfig, EntityTypeDecl, FollowConfig, HttpMethod,
    PaginationStrategy, ParameterKind, ParameterSpec, ParameterValue, PatternDecl, PrebuiltConfig,
    Recipe, RelationshipTypeDecl, RequestTemplate, ResolvedParameters, RetrievalConfig,
};
pub use registry::{RecipeRegistry, RegistryEntry, RegistryPrebuilt, RegistrySnapshot};
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
pub use testing::{
    AcquisitionResult, ChunkingResult, CorpusEstimate, ExtractionResult, FailedRecord, SampleChunk,
    TestOptions, TestQueryResult, TestReport, ValidationResult,
};
pub use types::{
    BatchEmbedFn, BuiltinCorpus, ChunkRange, CorpusKind, CorpusSpec, DedupPicker, EmbedFn,
    IndexInfo, IndexStats, InferenceFn, IngestResult, RerankConfig, RerankFn, ScoredChunk,
    ShardInfo,
};
pub use yield_hook::YieldHook;

// SCIP call graph + exporter dispatch live in `corpus-engine-scip`
// (carved out 2026-05-23). corpus-engine itself still uses scip via
// `update::watch::CodeWatcher` and
// `enrichment::atlas::strategies::code_walk`, but external consumers
// import directly from `corpus_engine_scip::*`. No re-export shim
// from this crate — keeping the seam clean prevents the `oicp-types`
// version-skew failure mode (§8.3) where re-exports invite two
// crates to depend on different versions of the same logical type.

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
    ActivityCallback, BackgroundWatcher, CoordinatorHandle, WatcherCoordinator, WatcherHeartbeat,
    WatcherStatus,
};

#[cfg(feature = "stores")]
pub use lint_results::LintResultStore;
#[cfg(feature = "stores")]
pub use test_results::TestResultStore;
#[cfg(feature = "treesitter")]
pub use update::lint_watcher::LintWatcher;
#[cfg(feature = "treesitter")]
pub use update::project_index_watcher::ProjectIndexWatcher;
#[cfg(feature = "treesitter")]
pub use update::test_watcher::TestWatcher;

// notes / project_docs moved to corpus-engine-notes (step 3 of the
// decomposition plan, 2026-05-23). No shim left here — external
// consumers depend on `corpus-engine-notes` directly. The crate
// is reachable inside corpus-engine via the `stores` feature for
// the three internal users (alignment_projector, alignment_workspace,
// project_index_watcher).
// `features::{FeatureStore, FeatureRow, …}` moved to corpus-engine-atos
// (step 2 of the decomposition plan, 2026-05-23). No shim left here —
// the four consumer crates (sovereign-atos, sovereign-cli-dev,
// sovereign-tools, commonwealth-api) depend on `corpus-engine-atos`
// directly.
