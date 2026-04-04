//! corpus-engine — shared corpus ingestion and index management.
//!
//! This crate handles:
//! - Recipe TOML parsing and validation
//! - Source data acquisition (download, crawl, local file)
//! - Content extraction (XML, JSON, HTML, CSV, Parquet, plaintext)
//! - Text chunking (paragraph, sentence, fixed, semantic)
//! - SQLite indexing with sqlite-vec (vector search) and FTS5 (keyword search)
//! - Shard operations (extract, merge, stats) for distributed indexes
//!
//! It has zero dependency on Sovereign or Commonwealth crates.

pub mod acquirers;
pub mod chunkers;
pub mod engine;
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
pub use error::{Error, Result};
pub use index::{CorpusIndex, InsertChunk};
pub use progress::{IngestProgress, ProgressCallback};
pub use recipe::Recipe;
pub use types::{
    BuiltinCorpus, ChunkRange, CorpusSpec, EmbedFn, IndexInfo,
    IndexStats, IngestResult, ScoredChunk, ShardInfo,
};
