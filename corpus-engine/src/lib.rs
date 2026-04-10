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
pub mod chunkers;
pub mod engine;
pub mod enrichment;
pub mod error;
pub mod extractors;
pub mod index;
pub mod progress;
pub mod recipe;
pub mod registry;
pub mod safety;
pub mod sharding;
pub mod testing;
pub mod types;
pub mod update;

// ─── Public API Re-exports ──────────────────────────────

pub use engine::CorpusEngine;
pub use enrichment::{
    Domain, EnrichmentProgress, FieldModelEngine, FieldModelStats, FieldSkeleton,
};
pub use extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
pub use error::{Error, Result};
pub use index::{CorpusIndex, InsertChunk, StoredChunk, StoredChunkWithMetadata};
pub use progress::{IngestProgress, ProgressCallback};
pub use recipe::{EnrichmentConfig, PrebuiltConfig, Recipe};
pub use registry::{RecipeRegistry, RegistryEntry, RegistryPrebuilt, RegistrySnapshot};
pub use testing::{
    AcquisitionResult, ChunkingResult, CorpusEstimate, ExtractionResult,
    FailedRecord, SampleChunk, TestOptions, TestQueryResult, TestReport,
    ValidationResult,
};
pub use types::{
    BuiltinCorpus, ChunkRange, CorpusSpec, EmbedFn, IndexInfo, IndexStats,
    InferenceFn, IngestResult, ScoredChunk, ShardInfo,
};
