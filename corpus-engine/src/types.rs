// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::{HashMap, HashSet};
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
/// Canonical default embedding dimensionality (Qwen3-Embedding-0.6B → 1024).
/// The ONE place this number lives: stub / zero-vector `EmbedFn`s and any
/// "what dim should I assume?" fallback must reference this, never a bare
/// literal. The recurring `768` leak was a wrong guess that kept getting
/// copy-pasted into new stubs; routing every stub through this constant stops
/// it drifting back. Real ingests still read the model's actual `n_embd` — this
/// is only the fallback for model-free / stub paths.
pub const DEFAULT_EMBED_DIM: usize = 1024;

pub type EmbedFn =
    Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>> + Send + Sync>;

/// Batch embedding function — embeds multiple texts in a single call.
/// When available, this is significantly faster than calling `EmbedFn`
/// in a loop because the backend can process multiple sequences in
/// one forward pass on the GPU.
pub type BatchEmbedFn = Arc<
    dyn Fn(&[String]) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send>> + Send + Sync,
>;

// ─── Inference Function ─────────────────────────────────

/// Inference function injected by the caller — used by the optional
/// enrichment pipeline to run claim/relationship extraction prompts.
/// Sovereign passes its Primary slot.
/// Commonwealth passes the mesh inference endpoint.
/// Tests pass a mock returning canned JSON.
///
/// The second argument is an optional JSON Schema (per OpenAI's
/// `structured_output` shape) the inference adapter should constrain
/// the response to. The corpus-engine side reads it from
/// `Domain::entity_extraction_schema()` and threads it through;
/// callers that pass `None` get the legacy free-form path. This is
/// the structural fix for the Phase 1b parse-failure tail observed
/// on enron-sample-multi-wide (2026-05-29): grammar-bounded output
/// can't emit unclosed arrays or extraneous prose.
pub type InferenceFn = Arc<
    dyn Fn(&str, Option<&serde_json::Value>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;

// ─── Rerank Function ────────────────────────────────────

/// Cross-encoder reranker injected by the caller. Given a query and
/// a slice of candidate documents (in the same order), returns one
/// relevance score per document. Higher = more relevant. The score
/// is the raw rank logit from the cross-encoder; absolute magnitude
/// is model-dependent (bge-reranker-v2-m3 returns ~[-10, +10]),
/// so callers should treat it as ordinal within a single call —
/// not directly comparable across calls or across rerankers.
///
/// Sovereign passes its local Rerank slot. Commonwealth passes a
/// mesh peer advertising `x:rerank`. Tests pass a mock returning
/// uniform scores (which preserves input order).
///
/// Length contract: `out.len() == docs.len()`, in the same order.
pub type RerankFn = Arc<
    dyn Fn(&str, Vec<String>) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>>
        + Send
        + Sync,
>;

/// Configuration for the cross-encoder rerank pass that runs on
/// top of vector + FTS hybrid retrieval. The pass is opt-in: if
/// `enabled` is false (or the runtime doesn't pass a `RerankFn`),
/// `CorpusIndex::search` behaves exactly as before — same scores,
/// same ordering, same threshold semantics.
///
/// When enabled, the search path overfetches `candidates_k`
/// candidates from LanceDB, scores all of them with the cross-encoder
/// in a single batched call, and then truncates to the caller's
/// requested `limit`. The rerank score replaces `ScoredChunk.score`;
/// the original hybrid score lands in `metadata["fusion_score"]` and
/// the rerank logit in `metadata["rerank_score"]` for observability.
#[derive(Debug, Clone)]
pub struct RerankConfig {
    /// Master switch. When false, the search path skips rerank
    /// entirely — no overfetch, no extra latency.
    pub enabled: bool,
    /// How many candidates to pull from LanceDB before reranking.
    /// Default 50; raise to widen the funnel, lower to cap latency.
    pub candidates_k: usize,
    /// Optional minimum rank logit. Candidates scoring below this
    /// are dropped before truncation. `None` keeps everything.
    /// bge-reranker-v2-m3 conventionally treats `0.0` as the
    /// relevance threshold (sigmoid → 0.5).
    pub min_score: Option<f32>,
    /// Blend weight on the rerank score in `[0.0, 1.0]`.
    ///
    /// - `1.0` (default) — final score is purely the rerank logit
    ///   (min-max normalised across the candidate pool); the
    ///   "replace fusion score" behaviour.
    /// - `0.0` — final score is the original fusion score; rerank
    ///   is computed but ignored (useful for instrumentation /
    ///   ablation).
    /// - In between — linear blend
    ///   `alpha * rerank_norm + (1 - alpha) * fusion_norm`. Both
    ///   sides are min-max normalised to `[0, 1]` within the
    ///   candidate pool first so the units match.
    ///
    /// Empirical knob — the right alpha is corpus-dependent. SEP
    /// (narrow canonical-source attribution) favours lower alpha
    /// because pure rerank promotes tangential articles densely
    /// mentioning the topic over the canonical entry; Wikipedia
    /// (topical-article attribution) tolerates higher alpha.
    pub alpha: f32,
    /// When true, aggregate chunks by `source_doc_id` (or `title`
    /// fallback) after reranking, keep each source's single
    /// best-scoring chunk, then return the top-`limit` distinct
    /// sources. Addresses the failure mode where the cross-encoder
    /// promotes 3-4 chunks from a tangential article that mentions
    /// the topic densely, crowding out the canonical entry's
    /// single highest-scoring chunk.
    ///
    /// Trade-off: caps depth-per-source at 1 chunk, which hurts
    /// answers that legitimately need multi-chunk coverage of a
    /// single article. Off by default.
    pub per_article: bool,
    /// Optional allow-list of `corpus_id`s eligible for the
    /// per-article dedup pass. Empirically, dedup-only is a clean
    /// win on SEP (narrow canonical sources, +10 sources) and a
    /// clean regression on Wikipedia (broader topical articles,
    /// -3 sources, see RERANK_EXPERIMENT.md ablation).
    ///
    /// - `None` (default): apply per_article to every corpus when
    ///   `per_article = true`. Matches the original ablation
    ///   behaviour.
    /// - `Some(set)`: only apply per_article to corpora whose ID
    ///   is in the set. Other corpora keep baseline-order results
    ///   even when `per_article = true`. SEP-only is the
    ///   empirically-validated default.
    pub dedup_corpus_filter: Option<HashSet<String>>,
    /// How the per-article dedup pass picks the "best chunk" within
    /// each source. The hypothesis under test (RERANK_EXPERIMENT.md
    /// §RRF noise investigation): the wiki dedup-only regression is
    /// driven by LanceDB's RRF noise inside an article. RRF is
    /// position-based, so an article's tangential paragraph can
    /// land at higher RRF rank than its canonical-summary paragraph
    /// purely by quirk. Switching the picker to `VectorDistance`
    /// uses cosine-to-query as the within-article signal, which is
    /// what a cross-encoder approximates without the cost.
    pub dedup_picker: DedupPicker,
    /// Weight on a per-candidate atlas signal added to the blend.
    /// `0.0` (default) reproduces baseline rerank+fusion ordering —
    /// the atlas-scores parameter, even when populated, drops out of
    /// the math.
    ///
    /// When `> 0.0` and `search_with_rerank` is passed a
    /// per-article-slug score map, the final blend becomes
    /// `final = alpha * rerank_norm + (1 - alpha) * fusion_norm
    ///          + atlas_weight * atlas_norm` with all three terms
    /// min-max normalised inside the candidate pool. Additive — the
    /// atlas term doesn't steal budget from rerank+fusion, it raises
    /// the floor for articles the atlas considers canonical.
    ///
    /// Candidates whose source article is absent from the atlas map
    /// score `0.0` for this term, i.e. the pool's floor. That is the
    /// intended bias for SEP-shaped corpora: articles outside the
    /// curated atlas shouldn't out-rank enriched canonical entries
    /// when the cross-encoder logit alone can't separate them.
    pub atlas_weight: f32,
}

/// Which signal the per-article dedup pass uses to pick the
/// best chunk within each source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DedupPicker {
    /// Use the fused score (RRF or rerank-blended) — the simplest
    /// rule: keep the first chunk we encounter per source in the
    /// already-sorted candidate list. Vulnerable to RRF noise
    /// inside an article.
    #[default]
    FusedScore,
    /// Use the raw `vector_distance` (cosine to query embedding) —
    /// re-orders by closest-to-query before the dedup walk so the
    /// within-article winner is the chunk whose embedding most
    /// resembles the query. Chunks without a `vector_distance`
    /// (FTS-only matches) sort last.
    VectorDistance,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            candidates_k: 50,
            min_score: None,
            alpha: 1.0,
            per_article: false,
            dedup_corpus_filter: None,
            dedup_picker: DedupPicker::FusedScore,
            atlas_weight: 0.0,
        }
    }
}

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
    /// Resolved `dedup_by_source` flag (recipe `[retrieval]
    /// dedup_by_source`). When true the runtime applies per-article
    /// source dedup to this corpus's retrieval. Falls back to `false`
    /// for legacy indexes whose `_corpus_meta.json` predates the field.
    #[serde(default)]
    pub dedup_by_source: bool,
    /// Resolved `personal_scope` flag (recipe `[retrieval]
    /// personal_scope`). When true this corpus counts as user-owned
    /// personal content: personal-scope turns retain it in retrieval
    /// instead of dropping it with the reference corpora. Falls back
    /// to `false` for legacy indexes whose `_corpus_meta.json`
    /// predates the field.
    #[serde(default)]
    pub personal_scope: bool,
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
    /// Whether the full index build completed for this corpus. Mirrors
    /// `IndexMeta.indexes_built` from `_corpus_meta.json`. `false` means
    /// the ingest/build never finished (e.g. a sync that paused
    /// mid-build): the corpus has few-or-no searchable chunks and must
    /// be rebuilt or resumed before it can serve retrieval. Surfaced
    /// here so the retrieval readiness gate can skip it and the desktop
    /// can flag it, without re-reading the meta file.
    #[serde(default)]
    pub indexes_built: bool,
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
    /// Stream-axis block (Move 5, Stage 2). Per-corpus stability tag
    /// surfaced here so retrieval-time consumers can render the
    /// freshness contract on chunk headers without a second meta
    /// read. `None` for legacy indexes pre-Stream-axes; `sovereign
    /// corpus stream-axes` backfills lazily.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<crate::stream_axes::StreamAxes>,
    /// Presentation hints from the recipe's `[display]` block —
    /// `category` + `icon`. Pure UI metadata; the retrieval layer
    /// reads `category == "conversation"` to label chunks "From your
    /// conversations" rather than emitting the corpus_id slug, and
    /// the Atlas View groups corpora that share a category under one
    /// rail header. `None` on legacy indexes ingested before the
    /// `[display]` block existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<crate::recipe::DisplayMeta>,
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
