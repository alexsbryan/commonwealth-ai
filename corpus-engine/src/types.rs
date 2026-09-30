// SPDX-License-Identifier: AGPL-3.0-or-later
//! corpus-engine's shared type vocabulary.
//!
//! The index row types — `IndexInfo`, `ScoredChunk` and their closure — moved
//! to the `corpus-index` leaf (domains `REVIEW-build-index-read-port`, DE "The
//! read-port leaf, measured again") and are re-exported here at their
//! historical `crate::types::*` paths. What remains is the ingest/engine
//! vocabulary.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// The index row types are DEFINED in the `corpus-index` leaf.
pub use corpus_index::prompt::InferenceFn;
pub use corpus_index::types::{
    BatchEmbedFn, BuiltinCorpus, ChunkRange, CorpusKind, DedupPicker, EmbedFn, IncompleteIngest,
    IndexInfo, RerankConfig, RerankFn, ScoredChunk, DEFAULT_EMBED_DIM,
}; // shim: moved by domains REVIEW-build-index-read-port // shim: moved by pb-cli-llm-ingest-move-remainder

// ─── Index Statistics ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    pub corpus_id: String,
    pub total_chunks: u64,
    pub min_chunk_id: u64,
    pub max_chunk_id: u64,
    pub index_size_bytes: u64,
}

// ─── Shard Info ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardInfo {
    pub path: PathBuf,
    pub chunk_range: ChunkRange,
    pub chunk_count: u64,
    pub size_bytes: u64,
}

// The ingest port's vocabulary lives beside the port (pb-ingest-dial-daemon-ports).
pub use corpus_index::ingest_port::daemon::IngestResult;

// ─── Corpus Spec ────────────────────────────────────────

/// What to ingest: either a builtin corpus by ID, a recipe path, or
/// an in-memory recipe.
///
/// `Inline` is used by the on-demand catalog flow: the
/// [`crate::catalog::CatalogIngestService`] resolves a content
/// recipe (`gutenberg-work`), patches its `[corpus] id`, the acquire
/// URL, and the parent corpus id, then hands the mutated recipe to
/// `CorpusEngine::ingest()` without writing a per-work TOML to
/// disk.
#[derive(Debug, Clone)]
pub enum CorpusSpec {
    Builtin(String),
    RecipePath(PathBuf),
    Inline(Box<crate::recipe::Recipe>),
}
