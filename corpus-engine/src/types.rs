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

// ─── Corpus Kind ────────────────────────────────────────

/// First-class classification of what kind of content an index
/// holds. The default is `Knowledge` — regular documents, books,
/// encyclopedia articles, the stuff that should ground a chat
/// answer. `Code` indexes are produced by `sovereign code index`
/// and serve the code-intelligence MCP tools (symbol_lookup,
/// code_search, etc.); they should be excluded from general chat
/// retrieval so BM25 keyword overlap on common tokens (`main`,
/// `argument`, `democracy`) doesn't drown out the actual knowledge
/// corpora. Surfaces in `IndexInfo` so every consumer (retrieval,
/// UI, health checks) can branch on it without re-deriving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CorpusKind {
    /// General documents — books, articles, scraped pages, the
    /// conversation-history corpus, etc. Default.
    #[default]
    Knowledge,
    /// A source-code repository indexed via `sovereign code index`.
    /// Has a `source_path` in its `IndexMeta`, and its chunks are
    /// typed for symbol-lookup and SCIP-style traversal rather than
    /// prose retrieval.
    Code,
    /// Catalog of works the system is *aware of* but has not read in
    /// detail. One chunk per work; the chunk text is the work's
    /// metadata (title, author, subjects, year, …) — not its full
    /// text. Catalog hits trigger an on-demand ingest of the
    /// corresponding content recipe (see `CorpusMeta::on_demand` and
    /// `Recipe::catalog`). Search consumers should partition catalog
    /// hits from full-text hits and surface them to the user as an
    /// "I know of this, want me to read it?" offer.
    Catalog,
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
    /// Resolved `query_sharing` flag — whether peers may run
    /// federated knowledge searches against this corpus. See
    /// `recipe::CorpusMeta::query_sharing` for the full rationale.
    /// Always populated at runtime (falls back to `mesh_sharing`
    /// when the on-disk meta lacks an explicit value).
    #[serde(default)]
    pub query_sharing: bool,
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
    /// Kind of corpus. Derived from `IndexMeta.source_path` at listing
    /// time: present → `Code`, absent → `Knowledge`. Consumers that
    /// retrieve for chat should filter out `Code`; consumers that
    /// serve the code-intelligence MCP tools should filter out
    /// `Knowledge`. Default preserves backward compatibility with
    /// any external caller that constructs `IndexInfo` by hand.
    #[serde(default)]
    pub kind: CorpusKind,
    /// For per-work corpora produced by an on-demand catalog ingest
    /// (e.g. `gutenberg-2701`), the id of the catalog corpus they were
    /// ingested from (e.g. `gutenberg`). Lets the UI group per-work
    /// indexes under their parent and lets retrieval suppress catalog
    /// offers for works that have already been read. `None` for
    /// stand-alone corpora.
    #[serde(default)]
    pub parent_corpus_id: Option<String>,
    /// Whether the IVF-PQ vector index has been built for this corpus.
    /// Mirrors `IndexMeta.vector_index_built` from
    /// `_corpus_meta.json` — exposed here so desktop callers don't
    /// need to re-read the meta file. The desktop's
    /// `vector_index_ready` SQLite cache is allowed to lag behind
    /// reality (the regular ingest path doesn't write to it); reading
    /// this flag from the on-disk meta is the source of truth.
    #[serde(default)]
    pub vector_index_built: bool,
    /// Stable content fingerprint for canonical indexes. See
    /// `IndexMeta::canonical_fingerprint` for the full contract; in
    /// short, two nodes with identical content arrive at the same
    /// hex string here, so the mesh can compare its peers' canonical
    /// states without shipping the index. `None` for partition
    /// indexes and for legacy canonicals that haven't been stamped
    /// yet (the daemon's `auto_recover` tick stamps them lazily).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_fingerprint: Option<String>,
    /// Total source shards this corpus expects (e.g. 38 for the
    /// canonical Wikipedia ingest). `None` for non-sharded corpora
    /// and for legacy indexes where the count was never stamped.
    /// Mirrors `IndexMeta.total_shards`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_shards: Option<usize>,
    /// Source shards this index has processed. Surfaces
    /// `IndexMeta.processed_shards` so callers (gossip publish, the
    /// auto-recover ratio compute) don't have to reach into the
    /// `CorpusIndex` for one extra method call.
    #[serde(default)]
    pub processed_shards: Vec<usize>,
    /// Reconciliation policy stamped on this index, mirroring
    /// `IndexMeta.mutable_merge`. `None` means classic content-hash
    /// dedupe; `Some(...)` opts a future merge into the chosen rule.
    /// Surfaced here so `merge_shards` can read the policy off the
    /// first input shard's `IndexInfo` without a second meta read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutable_merge: Option<crate::recipe::MutableMergePolicy>,
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
    /// Stable LanceDB row id for the chunk. `None` for synthetic
    /// chunks that don't correspond to a row (e.g. atlas-virtual
    /// summaries, local-doc chunks with String ids). Consumers that
    /// need to deref a citation back to the source — the desktop
    /// reading surface, atom-span detection — require this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u64>,
    /// The document this chunk belongs to (for grouping neighbors and
    /// for "elsewhere in this document" lookups). `None` when the
    /// extractor doesn't tag chunks with a document id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_doc_id: Option<String>,
    /// Raw cosine distance from the query embedding to this chunk's
    /// stored embedding (`1 - cosine_similarity`, range `[0, 2]`,
    /// lower = more semantically similar). Populated when search ran
    /// with a non-empty query embedding AND the chunk batch carried
    /// the `embedding` column.
    ///
    /// Why it lives alongside `score`: `score` collapses LanceDB's
    /// `_distance` / `_relevance_score` (RRF) / `_score` (BM25) into
    /// a single number to keep within-corpus ranking consistent. But
    /// those three sources have different scales and DON'T compose
    /// across corpora — RRF's `≈ 1/(60+rank)` saturation pattern
    /// makes a small corpus's top-1 hit beat a large corpus's
    /// semantically-better answer that happens to land at rank-1 in
    /// only one of (vector, FTS). `vector_distance` is the
    /// apples-to-apples signal cross-corpus consumers can sort by
    /// to break that tie. `None` for FTS-only paths (no query
    /// embedding) and for legacy callers that didn't request it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f32>,
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

