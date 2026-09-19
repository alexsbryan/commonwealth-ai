// SPDX-License-Identifier: AGPL-3.0-or-later
//! corpus-engine's shared type vocabulary.
//!
//! The index row types — `IndexInfo`, `ScoredChunk` and their closure — moved
//! to the `corpus-index` leaf (domains `REVIEW-build-index-read-port`, DE "The
//! read-port leaf, measured again") and are re-exported here at their
//! historical `crate::types::*` paths. What remains is the ingest/engine
//! vocabulary.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::enrichment::pipeline::types::ChatPrompt;
use crate::error::Result;

// The index row types are DEFINED in the `corpus-index` leaf.
pub use corpus_index::types::{
    BatchEmbedFn, ChunkRange, CorpusKind, DedupPicker, EmbedFn, IncompleteIngest, IndexInfo,
    RerankConfig, RerankFn, ScoredChunk, DEFAULT_EMBED_DIM,
}; // shim: moved by domains REVIEW-build-index-read-port

// ─── Inference Function ─────────────────────────────────

/// The ONE completion closure port, injected by the caller — the
/// enrichment pipeline runs claim/relationship extraction, section
/// naming, reconciliation and synthesis prompts through it. Sovereign
/// passes its Primary slot; Commonwealth passes the mesh chat
/// endpoint; tests pass a deterministic closure returning canned JSON.
///
/// Converged 2026-09-17 (ARCH 8) from three aliases that named this one
/// capability: the single-message `InferenceFn`, the multi-message
/// `ChatCompletionFn`, and `ChatCompletionWithTokensFn`, its
/// per-call `max_tokens` arm. The prompt is a [`ChatPrompt`], which
/// carries the system and user messages, the optional JSON Schema for
/// grammar-constrained generation (read from
/// `Domain::entity_extraction_schema()` and threaded through), the
/// per-phase sampling controls and the prompt's own output budget.
/// The second argument is the per-call output-token override the retry
/// paths need: `Some(n)` wins over the prompt's budget, `None` defers
/// to it. Single-message callers pass `ChatPrompt::new("", prompt)`.
pub type InferenceFn = Arc<
    dyn Fn(&ChatPrompt, Option<u32>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;

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

// ─── Ingest Result ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub corpus_id: String,
    pub chunks_created: u64,
    pub index_size_bytes: u64,
    pub duration_secs: u64,
    /// Documents skipped due to extraction errors (e.g. invalid UTF-8, corrupt lines).
    /// Non-zero warrants inspection of the source file on the ingesting node.
    #[serde(default)]
    pub docs_skipped: u64,
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
    /// If set, this corpus is a layer/satellite of `parent_corpus_id`.
    /// Sourced from the registry snapshot; the desktop hides children
    /// from the top-level picker and renders them as toggles under the
    /// parent row. `None` for top-level corpora.
    #[serde(default)]
    pub parent_corpus_id: Option<String>,
    /// Catalog presentation tier — `"featured" | "preview" | "hidden"`.
    /// Mirrors `RegistryEntry::catalog_status`. `None` defaults to
    /// `"preview"` on the desktop side so newly-registered recipes
    /// land under "Coming soon" until explicitly promoted.
    #[serde(default)]
    pub catalog_status: Option<String>,
}

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
