use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Result;

// ─── Embedding Function ─────────────────────────────────

/// Embedding function injected by the caller.
/// Sovereign passes its local Embed slot.
/// Commonwealth passes an HTTP client to /v1/embeddings.
/// Tests pass a mock returning zero vectors.
pub type EmbedFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>>
        + Send
        + Sync,
>;

/// Batch embedding function — embeds multiple texts in a single call.
/// When available, this is significantly faster than calling `EmbedFn`
/// in a loop because the backend can process multiple sequences in
/// one forward pass on the GPU.
pub type BatchEmbedFn = Arc<
    dyn Fn(&[String]) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send>>
        + Send
        + Sync,
>;

// ─── Inference Function ─────────────────────────────────

/// Inference function injected by the caller — used by the optional
/// enrichment pipeline to run claim/relationship extraction prompts.
/// Sovereign passes its Primary slot.
/// Commonwealth passes the mesh inference endpoint.
/// Tests pass a mock returning canned JSON.
pub type InferenceFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;

// ─── Chunk Range ────────────────────────────────────────

/// A contiguous range of chunk IDs within a corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRange {
    /// First chunk ID (inclusive).
    pub start_id: u64,
    /// Last chunk ID (exclusive).
    pub end_id: u64,
}

impl ChunkRange {
    pub fn new(start_id: u64, end_id: u64) -> Self {
        debug_assert!(start_id < end_id, "empty chunk range: {start_id}..{end_id}");
        Self { start_id, end_id }
    }

    pub fn count(&self) -> u64 {
        self.end_id - self.start_id
    }
}

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

// ─── Index Info ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    pub corpus_id: String,
    pub corpus_name: String,
    pub path: PathBuf,
    pub chunk_count: u64,
    pub index_size_bytes: u64,
    pub created_at: u64,
    pub last_updated: u64,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub mesh_sharing: bool,
    pub is_shard: bool,
    pub chunk_range: Option<ChunkRange>,

    // ── Health-check fields ──────────────────────────────────
    /// Expected total chunks from ingestion start; None for legacy indexes.
    #[serde(default)]
    pub chunks_expected: Option<u64>,
    /// Resume cursor from the last interrupted ingest (batch ID).
    #[serde(default)]
    pub resume_from: Option<String>,
    /// True if the enrichment pipeline has ever been run for this corpus.
    #[serde(default)]
    pub enrichment_enabled: bool,
    /// Number of chunks that have at least one extracted claim.
    #[serde(default)]
    pub enriched_chunks: Option<u64>,
    /// Source dataset version (e.g. a date stamp or hash from the manifest).
    #[serde(default)]
    pub source_version: Option<String>,
    /// URL used to check for newer versions of this corpus.
    #[serde(default)]
    pub update_manifest_url: Option<String>,
}

// ─── Scored Chunk (search result) ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredChunk {
    pub content: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub corpus_id: String,
    pub score: f32,
    pub metadata: HashMap<String, String>,
}

// ─── Ingest Result ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub corpus_id: String,
    pub chunks_created: u64,
    pub index_size_bytes: u64,
    pub duration_secs: u64,
}

// ─── Builtin Corpus ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinCorpus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_compressed_gb: f64,
    pub size_indexed_gb: f64,
    pub license: String,
    pub mesh_sharing: bool,
}

// ─── Corpus Spec ────────────────────────────────────────

/// What to ingest: either a builtin corpus by ID or a recipe path.
#[derive(Debug, Clone)]
pub enum CorpusSpec {
    Builtin(String),
    RecipePath(PathBuf),
}

