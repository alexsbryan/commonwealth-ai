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
pub mod safety;
pub mod sharding;
pub mod types;

// ─── Public API Re-exports ──────────────────────────────

pub use engine::CorpusEngine;
pub use enrichment::{
    ClaimRelationship, ContestedCluster, EnrichmentEngine, EpistemicLandscape,
    EpistemicStatus, ExtractedClaim, Position, RelationshipType,
};
pub use error::{Error, Result};
pub use index::{CorpusIndex, InsertChunk, StoredChunk};
pub use progress::{IngestProgress, ProgressCallback};
pub use recipe::{EnrichmentConfig, Recipe};
pub use types::{
    BuiltinCorpus, ChunkRange, CorpusSpec, EmbedFn, IndexInfo, IndexStats,
    InferenceFn, IngestResult, ScoredChunk, ScoredClaim, ShardInfo,
};
