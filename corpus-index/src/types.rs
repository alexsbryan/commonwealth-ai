// SPDX-License-Identifier: AGPL-3.0-or-later
//! The index row types — what an index directory reports and what a search
//! returns.
//!
//! `IndexInfo`, `ScoredChunk` and their closure are the shape of
//! `_corpus_meta.json` and of a search hit, so they are DEFINED here and
//! `corpus-engine`'s `types` module re-exports them at their historical paths
//! (DE "The read-port leaf, measured again": "a type the index persists is
//! defined by the index").

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

// ─── Rerank Config ──────────────────────────────────────

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
    /// Resolved `grantable` flag (recipe `[corpus] grantable`). When
    /// true this corpus may be lent to selected mesh peers under an
    /// ephemeral, revocable one-off ingest grant even though
    /// `mesh_sharing` is false. Falls back to `false` for legacy indexes
    /// and for structural KnowledgeView corpora that never opt in.
    #[serde(default)]
    pub grantable: bool,
    pub is_shard: bool,
    pub chunk_range: Option<ChunkRange>,

    // ── Health-check fields ──────────────────────────────────
    /// Expected total chunks from ingestion start; None for legacy indexes.
    #[serde(default)]
    pub chunks_expected: Option<u64>,
    /// Resume cursor from the last interrupted ingest (batch ID).
    #[serde(default)]
    pub resume_from: Option<String>,
    /// True if this corpus's recipe **asked** for enrichment — the projection
    /// of `IndexMeta::enrichment_requested`, stamped at the entry of ingest's
    /// enrichment block. It records intent, not completion: `true` with no
    /// field-model tables on disk is precisely the "requested but never
    /// finished" state `EnrichmentChecker` reports. Do not confuse it with
    /// `RegistryEntry::enrichment_enabled`, which is a fact about the
    /// catalogue recipe. Alias kept so hand-built serde fixtures and any
    /// pre-rename JSON still deserialize.
    #[serde(default, alias = "enrichment_enabled")]
    pub enrichment_requested: bool,
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

// ─── Incomplete ingest ──────────────────────────────────

/// An index directory that [`CorpusEngine::installed_indexes`] deliberately
/// drops: its `_corpus_meta.json` parses fine, but `ingestion_in_progress` is
/// still `true`, so the ingest that created it never reached
/// `mark_ingestion_complete`.
///
/// **Why this needs a type of its own.** `installed_indexes()` is the only
/// enumeration of corpora on disk, and it skips these — correctly, since a
/// half-built index must not be served to retrieval. The side effect is that
/// nothing built on that list can ever *report* one. So a health check whose
/// entire job is "did this install finish?" was structurally unable to see
/// the failures it exists to describe, and had to ask for them by name.
///
/// The shape on disk is usually `<corpus_id>-partition-<node>/`, because
/// promotion to the canonical `<corpus_id>/` runs only on `Ok`
/// (`finalise_solo_ingest`). That is also why the `corpus_id` here is read
/// from the meta rather than parsed out of the directory name.
///
/// [`CorpusEngine::installed_indexes`]: crate::CorpusEngine::installed_indexes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncompleteIngest {
    /// Corpus the dead ingest was building, from its own meta.
    pub corpus_id: String,
    /// Directory as found under the engine's index dir.
    pub path: PathBuf,
    /// `indexes_built: true` beside `ingestion_in_progress: true` is the
    /// failed-ingest fingerprint documented in
    /// `docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §3: `build_indexes()` finished
    /// and stamped this, then a LATER phase threw — enrichment being the one
    /// that actually did — and `mark_ingestion_complete()` never ran. `false`
    /// means the ingest died earlier, mid-embed, which is a different and far
    /// noisier failure.
    pub indexes_built: bool,
    /// Whether the dead ingest's recipe had asked for enrichment. Lets an
    /// enrichment-scoped consumer stay in its lane instead of reporting every
    /// interrupted ingest on the machine.
    pub enrichment_requested: bool,
}

impl IndexInfo {
    /// Is this corpus one that code-aware consumers should query?
    ///
    /// The honest answer is **not** `kind == CorpusKind::Code`. That tag
    /// is unreliable-by-design for repos: `Recipe.corpus.kind` is a
    /// non-`Option` field defaulting to `Knowledge`, and the two recipes
    /// that build code corpora deliberately leave it that way, because
    /// chat retrieval admits only `Knowledge | Catalog` and
    /// `CODE_INTEL_CHAT.md` routes code questions *through* the knowledge
    /// path — a repo tagged `Code` would vanish from chat. So
    /// `commonwealth-ai` ships as `kind:"knowledge"` with 36k code chunks
    /// and a full SCIP graph.
    ///
    /// Every consumer that screened on the tag alone therefore matched
    /// **nothing** on a healthy install: `code_search` reported "0 code
    /// corpora" against a perfect index, and `handle_metalingual_query`
    /// declined every "in this codebase" question with `no_source` while
    /// 41k indexed chunks sat beside it. The robust signal is an on-disk
    /// `scip_graph.db` next to the chunk table.
    ///
    /// Accepting *either* keeps the original safety property intact: a
    /// prose corpus (Wikipedia, SEP) has neither a `Code` tag nor a
    /// graph, so it is still skipped before any Lance call — which
    /// matters, because its chunk table lacks the typed code columns
    /// entirely and the query would error at column resolution rather
    /// than return zero rows.
    ///
    /// This lives on `IndexInfo` rather than in a consumer crate because
    /// it previously existed as three copies that disagreed
    /// (`sovereign_code::has_code_graph`,
    /// `Runtime::code_corpus_ids`, and the metalingual handler's tag
    /// filter). `sovereign-core` carries `sovereign-tools` only as a
    /// dev-dependency, so the runtime could not reach the corrected
    /// version even in principle. Both crates already depend on
    /// corpus-engine, and corpus-engine owns `IndexInfo` — so this is the
    /// one place all of them can share.
    pub fn is_code_corpus(&self) -> bool {
        self.kind == CorpusKind::Code || self.path.join("scip_graph.db").exists()
    }
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
    /// Where this chunk came from — see [`Provenance`](crate::index::ChunkProvenance).
    ///
    /// A FIELD rather than two keys in [`Self::metadata`], because whether a
    /// chunk may ground a claim was a string compare on an untyped map:
    /// `metadata["custody"]` set the egress floor and
    /// `metadata["source"] == "raptor"` decided quotability, written at one
    /// site and read at roughly fifteen (TOPOLOGY §10 phase 9 rung 9.1,
    /// hazard 1). The `Acquired` arm has no public constructor, so a chunk
    /// this process built can only say so.
    ///
    /// Required — there is no `Default`. A chunk that could omit its
    /// provenance would be a manufactured chunk passing as an acquired one by
    /// omission, which is the state the rung removes.
    ///
    /// `skip_deserializing` because `Provenance` deliberately has no
    /// `Deserialize` (a derive is a public constructor). `ScoredChunk`'s own
    /// `Deserialize` is vestigial — no production path deserializes one — and
    /// what it yields is honestly "this process did not acquire it".
    #[serde(
        skip_deserializing,
        default = "crate::index::ChunkProvenance::off_the_wire"
    )]
    pub provenance: crate::index::ChunkProvenance,
}

// ─── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod is_code_corpus_tests {
    use super::*;

    /// Build a minimal `IndexInfo` through serde so the test doesn't have to
    /// name the ~30 `#[serde(default)]` fields that are irrelevant here.
    fn info(path: &std::path::Path, kind: &str) -> IndexInfo {
        serde_json::from_value(serde_json::json!({
            "corpus_id": "fixture",
            "corpus_name": "Fixture",
            "path": path,
            "chunk_count": 1,
            "index_size_bytes": 1,
            "created_at": 0,
            "last_updated": 0,
            "embedding_model": "test",
            "embedding_dimensions": 1024,
            "mesh_sharing": false,
            "is_shard": false,
            "chunk_range": null,
            "kind": kind,
        }))
        .expect("fixture IndexInfo deserializes")
    }

    #[test]
    fn code_tag_alone_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        assert!(info(dir.path(), "code").is_code_corpus());
    }

    /// The regression this predicate exists for. Repo corpora ship as
    /// `knowledge`-kind on purpose, so a tag-only test matched nothing on a
    /// healthy install — `commonwealth-ai` (41k chunks, full SCIP graph) was
    /// invisible to every code-aware consumer.
    #[test]
    fn scip_graph_alone_is_enough_even_when_tagged_knowledge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("scip_graph.db"), b"").unwrap();
        let info = info(dir.path(), "knowledge");
        assert_eq!(info.kind, CorpusKind::Knowledge);
        assert!(
            info.is_code_corpus(),
            "a knowledge-tagged corpus with a SCIP graph IS a code corpus — this is \
             the shape every real repo index has"
        );
    }

    #[test]
    fn both_signals_together_still_true() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("scip_graph.db"), b"").unwrap();
        assert!(info(dir.path(), "code").is_code_corpus());
    }

    /// The safety property: a prose corpus has neither signal, so it is
    /// skipped before any Lance call. Its chunk table lacks the typed code
    /// columns entirely, so querying it would error at column resolution
    /// rather than return zero rows.
    #[test]
    fn prose_corpus_with_neither_signal_is_not_code() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!info(dir.path(), "knowledge").is_code_corpus());
    }
}

// ─── Note-wire vocabulary (fp-3, §12 decision 3) ───────────────────────────
// Moved from `corpus-engine-notes/src/notes.rs` — the vocabulary the
// AgentNotes port's construction sites name. `EmbedFn` above is the one alias:
// the notes-side copy was a documented shape twin of it and is now a
// re-export. `GlinerFn`/`PropagationSinkFn` follow the same injectable-
// closure shape. Re-exported at the historical `corpus_engine_notes::` paths.

/// Propagation event shipped on the mesh wire for a single note.
///
/// The full unit of propagation: note row + T1 embedding (if
/// present) + T2 entities (empty until T2 lands) + the propagation
/// metadata (`tombstone`, `updated_at`, `private`). Identified
/// uniquely by `content_hash` — stable across `origin_node_id`
/// rotation, idempotent on re-delivery, the same on every peer.
///
/// Wire format chosen so:
/// - Two peers writing semantically identical notes produce
///   byte-identical events keyed by the same `content_hash`.
///   Dedup on receive is a `SELECT WHERE content_hash = ?`.
/// - The reader can apply embeddings + entities in the same SQL
///   transaction as the note row.
/// - A `tombstone=true` event with the same `content_hash` as a
///   prior note marks it deleted; LWW on `updated_at` is the
///   tiebreak, but a tombstone always wins (per Step 6b).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotePropagationEvent {
    pub content_hash: String,
    pub note: ExportedNoteRow,
    /// Legacy T1 embedding payload — **received, never sent.**
    ///
    /// Until order `mesh-scale-t1-notes` (2026-08-13) the gossip wire
    /// carried the note's embedding as a JSON array of decimal
    /// integers: 14.5 KB of a 16.1 KB event against a 1.5 KB note
    /// body, putting the 8 MiB `MAX_REQUEST_BODY_BYTES` push limit at
    /// ~520 notes (measured — `research/scale-analysis/`
    /// `MESH_SCALE_100_USERS_1000_CORPORA.md` §8.3.1). It was also a
    /// correctness bug: a peer on a different embed model shipped
    /// vectors from a foreign space that the local cosine pool scored
    /// as peers of local ones (§8.3.2).
    ///
    /// [`serialize_embedding_as_absent`] is the structural half of
    /// the fix (`ARCH §7` — encode the invariant so it cannot be
    /// forgotten): the serializer writes `null` for this field
    /// unconditionally, so no producer *can* put a vector on the
    /// wire, whatever a future constructor sets it to. It writes
    /// `null` rather than omitting the field so a peer still running
    /// the pre-strip build — whose `embedding` field has no
    /// `#[serde(default)]` and therefore *requires* the key — can
    /// still decode our events.
    ///
    /// `default` is the other half of the tolerance: a peer on an
    /// older build still sends a populated field, it still
    /// deserializes here, and [`NoteStore::ingest_remote_notes`]
    /// DISCARDS the foreign vector and re-embeds the content in the
    /// local space. Both shapes are accepted indefinitely, in both
    /// directions. No schema break.
    ///
    /// Proved by `corpus-engine-notes/tests/note_wire_shapes.rs`,
    /// which decodes a new event with a byte-for-byte mirror of the
    /// pre-strip struct.
    #[serde(default, serialize_with = "serialize_embedding_as_absent")]
    pub embedding: Option<ExportedNoteEmbedding>,
    /// Empty until T2 lands; the wire field is provisioned now so
    /// T2 ships as a data change, not a schema change.
    #[serde(default)]
    pub entities: Vec<ExportedNoteEntity>,
    pub tombstone: bool,
    pub updated_at: i64,
    /// Origin-clock publication receipt (order `commons-fluency`
    /// fix 3): when the origin's sink successfully published this
    /// event onto the mesh. Stamped by the sink (the daemon's
    /// `wire_note_propagation_sink`) at `set()` time — NOT at write
    /// time, so a re-published delta carries its own stamp — and
    /// also carried by pull-delta events from the origin row's
    /// `sent_at` column.
    ///
    /// `None` means the event never went through a stamping sink:
    /// pre-v12 origins, or stores without a mesh identity. Peers
    /// that receive a `sent_at: null` event still stamp their own
    /// `received_at` — the receipt exists one-sided, and the
    /// negative arm is a first-class answer (§18.3: absence is
    /// reported, never defaulted).
    #[serde(default)]
    pub sent_at: Option<i64>,
}

/// Note row carried on the propagation wire. Mirrors the
/// `Note` shape (minus rowid + retirement metadata, which is
/// node-local lifecycle state) plus the v9 propagation fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedNoteRow {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub symbols: Vec<String>,
    pub files: Vec<String>,
    pub session_id: String,
    pub created_at: i64,
    pub scope: String,
    pub feature_id: Option<String>,
    pub related_entity: Option<String>,
    pub source: String,
    pub supersedes: Option<String>,
    pub payload_json: Option<String>,
    pub origin_node_id: Option<String>,
}

/// T1 embedding wire payload. Carries the LE-encoded BLOB along
/// with the model id + dim so a peer that's running a different
/// embed model can fall back to recomputing rather than blending
/// incompatible vectors.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedNoteEmbedding {
    pub model_id: String,
    pub dim: i64,
    pub embedding: Vec<u8>,
}

/// Serialize [`NotePropagationEvent::embedding`] as `null`, always,
/// whatever it holds.
///
/// This is the whole of the `t1-notes-clean-wire` fix and it lives in
/// exactly one place on purpose (`ARCH §7`, `§10.6`): stripping at the
/// four construction sites would be a rule four future callers have to
/// remember, whereas a serializer that ignores its input is a rule the
/// type system applies for them. The field is still *written* — as
/// `null` — because a peer on the pre-strip build requires the key to
/// be present to decode at all.
fn serialize_embedding_as_absent<S>(
    _value: &Option<ExportedNoteEmbedding>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_none()
}

/// T2 entity wire payload (one row per (entity, kind) tuple
/// extracted from the note's content). Empty until GLiNER
/// extraction lands.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportedNoteEntity {
    pub entity: String,
    pub kind: String,
}


/// Fire-and-forget callback the daemon installs to publish
/// propagation events. The closure adapts the event into whatever
/// transport the caller owns (most commonly
/// `MeshStore::put(app_id="notes", key=content_hash, value=event)`,
/// occasionally a channel for tests).
///
/// Sync because `MeshStore` writes are SQLite + LWW — microsecond
/// fast. NoteStore stays dep-free per `ARCH §5.4` — the closure
/// hides the transport.
/// Outbound propagation callback. Returns whether the event was
/// accepted for publication (`true` = the mesh `set()` succeeded).
/// The store stamps the local row's `sent_at` from the return value,
/// so a failed publish never claims a receipt (order `commons-fluency`
/// fix 3).
pub type PropagationSinkFn = Arc<dyn Fn(&NotePropagationEvent) -> bool + Send + Sync>;


/// GLiNER entity-extraction function injected by the caller.
///
/// Returns `Vec<(entity, kind)>` per text — `entity` is the raw
/// surface form found in the content, `kind` is the GLiNER label
/// (e.g. `"Person"`, `"Organization"`, `"Symbol"`, `"File"`).
/// The set of admissible kinds is the caller's concern; NoteStore
/// stores whatever it's handed.
///
/// Sovereign passes its loaded GLiNER session (the same one
/// `chunk_entity_extractor` uses for the corpus pipeline).
/// Commonwealth passes an HTTP shim. Tests pass a deterministic
/// mock that emits known labels per substring.
///
/// Async because GLiNER inference is non-trivial; tens of ms
/// even on a small model. The closure copies `&str` internally.
pub type GlinerFn = Arc<
    dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<(String, String)>>> + Send>>
        + Send
        + Sync,
>;


/// One member of the mesh, as far as note attribution is concerned.
#[derive(Debug, Clone)]
pub struct RosterEntry {
    /// Full 32-char lowercase hex of the node id (`NodeId::to_hex`), NOT
    /// the truncated `Display` form. Stored full so a truncated note
    /// origin can be prefix-matched against it.
    pub id_hex: String,
    /// Human-facing mesh name, e.g. `"RuggedFox"`.
    pub name: String,
}

/// Who's who on the mesh, for turning a note's `origin_node_id` into a
/// name a reader recognises.
///
/// INJECTED, never self-loaded. This crate is the knowledge layer and
/// holds no mesh types; the host that owns the mesh identity (the
/// daemon) builds this from `mesh.json` and calls
/// [`NoteStore::set_node_roster`]. That mirrors how `sovereign-work-atlas`
/// receives its `NodeId` by constructor injection rather than reading
/// the roster file itself, and keeps `mesh.json` with exactly one
/// reader (`sovereign_mesh::persist::load`).
///
/// When no roster is wired — a bare CLI `NoteStore::open` — attribution
/// degrades to [`NodeAttribution::Unknown`] carrying the raw id. It never
/// degrades to "assume it's us".
#[derive(Debug, Clone, Default)]
pub struct NodeRoster {
    self_node: Option<RosterEntry>,
    peers: Vec<RosterEntry>,
}

impl NodeRoster {
    /// Build from the local node plus every other known member.
    ///
    /// `self_node` is `None` when the host knows the mesh membership but
    /// cannot identify itself within it; every id then resolves as a
    /// peer or as unknown, which is the honest reading.
    pub fn new(self_node: Option<RosterEntry>, peers: Vec<RosterEntry>) -> Self {
        Self { self_node, peers }
    }

    /// Name of the local node, when known.
    pub fn self_name(&self) -> Option<&str> {
        self.self_node.as_ref().map(|e| e.name.as_str())
    }

    /// Resolve a note origin to an attribution.
    ///
    /// Accepts either the truncated `Display` form (`node-` + 16 hex
    /// chars, which is what notes actually store) or a full 32-hex id.
    /// Because the stored form is lossy, matching is by prefix — with
    /// more than one candidate reported as [`NodeAttribution::Ambiguous`]
    /// rather than resolved to whichever came first. At mesh sizes where
    /// an 8-byte prefix collides, guessing would be the bug.
    pub fn resolve(&self, origin: &str) -> NodeAttribution {
        let needle = origin
            .trim()
            .trim_start_matches("node-")
            .to_ascii_lowercase();
        if needle.is_empty() {
            return NodeAttribution::Unattributed;
        }

        let matches =
            |e: &RosterEntry| e.id_hex.starts_with(&needle) || needle.starts_with(&e.id_hex);

        if let Some(me) = self.self_node.as_ref().filter(|e| matches(e)) {
            return NodeAttribution::SelfNode {
                name: me.name.clone(),
            };
        }

        let hits: Vec<&RosterEntry> = self.peers.iter().filter(|e| matches(e)).collect();
        match hits.as_slice() {
            [] => NodeAttribution::Unknown {
                id: origin.to_string(),
            },
            [one] => NodeAttribution::Peer {
                name: one.name.clone(),
                id: origin.to_string(),
            },
            many => NodeAttribution::Ambiguous {
                id: origin.to_string(),
                candidates: many.iter().map(|e| e.name.clone()).collect(),
            },
        }
    }
}

/// Where a note came from, from the reading node's point of view.
///
/// The whole point of this type is the self-vs-peer distinction: a note
/// saying "holding the daemon for a 4h soak" is an instruction on the
/// box that wrote it and noise everywhere else, and a reader cannot tell
/// which without knowing the author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeAttribution {
    /// The note carries no origin — pre-column rows, and any write
    /// through a store with no mesh identity. Reported, never defaulted.
    Unattributed,
    /// Written on the machine doing the reading.
    SelfNode {
        /// Local node's mesh name.
        name: String,
    },
    /// Written on a different machine.
    Peer {
        /// That machine's mesh name.
        name: String,
        /// The raw origin id, kept so the label stays traceable.
        id: String,
    },
    /// An origin id that matches no roster member — a departed node, or
    /// no roster wired at all.
    Unknown {
        /// The raw origin id.
        id: String,
    },
    /// The truncated id prefix-matched more than one member. Surfaced
    /// rather than guessed.
    Ambiguous {
        /// The raw origin id.
        id: String,
        /// Names of every member the prefix matched.
        candidates: Vec<String>,
    },
}

impl NodeAttribution {
    /// The one rendering of an author, shared by every note surface so
    /// they cannot drift apart (ARCH_PRINCIPLES §10.6).
    pub fn label(&self) -> String {
        match self {
            Self::Unattributed => "unknown origin".to_string(),
            Self::SelfNode { name } => format!("{name} (this machine)"),
            Self::Peer { name, .. } => format!("{name} (peer)"),
            Self::Unknown { id } => format!("{id} (unrecognised node)"),
            Self::Ambiguous { id, candidates } => {
                format!("{id} (ambiguous: {})", candidates.join(", "))
            }
        }
    }

    /// Same judgement as [`label`](Self::label), rendered for surfaces on a
    /// token budget (the boot brief, digests). The self/peer decision is
    /// still made once, in [`NodeRoster::resolve`] — this is a second
    /// density, not a second decider.
    pub fn label_compact(&self) -> String {
        match self {
            Self::Unattributed => "?".to_string(),
            Self::SelfNode { name } => name.clone(),
            Self::Peer { name, .. } => format!("{name}⟵peer"),
            // Keep the id. A caller rendering an entry that HAS an origin
            // (a work-in-flight claim always does) still needs something
            // to cross-reference against `sovereign mesh status`; a bare
            // "?" there is strictly less useful than the raw id this
            // replaced.
            Self::Unknown { id } => format!("{}…", id.chars().take(13).collect::<String>()),
            Self::Ambiguous { .. } => "?ambiguous".to_string(),
        }
    }

    /// The compact label, but ONLY when it would change what the reader
    /// does — `None` means "render nothing here".
    ///
    /// [`label_compact`](Self::label_compact) answers "how do I write this
    /// attribution down?"; this answers the prior question, "is there an
    /// attribution worth writing down at all?", and per-note surfaces want
    /// the second one.
    ///
    /// WHY THIS EXISTS. A per-note marker is only information when the
    /// note might be about a machine that ISN'T the reader's — that is the
    /// whole point of the feature. Self is the common case and the boot
    /// hook has already told the session which machine it is; unattributed
    /// and unrecognised origins carry nothing a reader can act on. Rendering
    /// those anyway inverts the feature: shipped in `a8e10be6`,
    /// `render_notes` printed `label_compact` unconditionally and
    /// `attribution()` returns `Unknown` for EVERY row when no roster is
    /// wired — so on the CLI brief path (which structurally has no roster;
    /// `sovereign-cli` cannot depend on `sovereign-mesh`) all 15 note lines
    /// read `_?_`. Measured 2026-08-07 via `sovereign code brief --hours 48`:
    /// 100% of lines, including peer-written notes whose `origin_node_id`
    /// was populated and correct.
    ///
    /// `Ambiguous` DOES render. It is not missing provenance — it is a
    /// roster that cannot answer, which is a real warning and rare enough
    /// to be worth the tokens.
    ///
    /// This is the same rule `.claude/hooks/inject-notes.py::author_tag`
    /// applies to the notes-injection surface. The rule lives here so the
    /// two cannot drift; that hook reads the `author_relation` discriminant
    /// (see [`as_str`](Self::as_str)) rather than re-deciding.
    pub fn marker_compact(&self) -> Option<String> {
        match self {
            Self::Peer { .. } | Self::Ambiguous { .. } => Some(self.label_compact()),
            Self::SelfNode { .. } | Self::Unattributed | Self::Unknown { .. } => None,
        }
    }

    /// True only when the note is known to have been written here.
    /// Unattributed and unknown origins are NOT this machine — a reader
    /// deciding whether a machine-state note applies to it must not have
    /// missing provenance read as "yes".
    pub fn is_this_machine(&self) -> bool {
        matches!(self, Self::SelfNode { .. })
    }

    /// Short machine-readable discriminant for JSON surfaces:
    /// `"self"` | `"peer"` | `"unknown"` | `"ambiguous"` | `"unattributed"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unattributed => "unattributed",
            Self::SelfNode { .. } => "self",
            Self::Peer { .. } => "peer",
            Self::Unknown { .. } => "unknown",
            Self::Ambiguous { .. } => "ambiguous",
        }
    }
}

