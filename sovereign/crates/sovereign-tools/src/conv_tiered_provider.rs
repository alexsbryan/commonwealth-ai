//! Conversation tiered-retrieval provider — the concrete impl of
//! `corpus_engine::enrichment::tiered::TieredEnrichmentProvider` that
//! the daemon injects into `CorpusEngine` at startup.
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md`.
//!
//! Architecture rationale (why this lives in sovereign-tools, not
//! corpus-engine):
//! - `build_raptor_atlas` lives in `sovereign-tools::raptor_atlas`
//!   (corpus-agnostic builder; same one attached docs use).
//! - `SqliteStateStore` lives in `sovereign-store`, which
//!   `sovereign-tools` depends on but `corpus-engine` does not.
//! - corpus-engine therefore owns only the trait + the dispatch
//!   loop; sovereign-tools owns the heavy work + persistence.
//!
//! ## v0 scope (this session)
//!
//! - **Tiny bucket (<8 chunks)**: synthesize a single
//!   `ConvRaptorNodeRow` from `chunk.title` (the conversation title
//!   from the claude.ai export) + mean(chunk_embeddings). No LLM
//!   call. Spec opt-2.
//! - **Non-Tiny buckets (Small/Medium/Large/LongTail)**: call
//!   `build_raptor_atlas` with `Speed::Slow` (Phase A default), convert
//!   each `RaptorNode` to `ConvRaptorNodeRow`, persist via
//!   `save_conv_raptor_nodes`.
//! - **State machine**: write a `conv_skeletons` row stamped `Ready`
//!   when the per-conv pass succeeds, `Failed` if anything errored.
//!
//! ## Deferred to next session
//!
//! - Opt-1 (Fast slot routing for ≤30-chunk convs) — needs an
//!   InferenceProvider wrapper that rewrites `preferred_speed`.
//! - T2 entity graph via `extract_action_atoms` + `entity_graph::build`.
//! - T3 motif extraction + classification.
//! - T3 TextTiling segments.
//!
//! Today's landing gives operators a working baseline: re-run the
//! conv-anthropic install and the `conv_raptor_nodes` table fills up
//! with real per-conversation trees (Slow on every bucket — slower
//! than the optimized budget, but it works end-to-end).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use corpus_engine::enrichment::tiered::{
    ChunkEntityExtractor, ConvBucket, TieredEnrichmentProvider,
};
use corpus_engine::error::{Error, Result};
use corpus_engine::index::{CorpusIndex, EnrichmentChunkRow};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{DocumentTypeTag, QuoteSpan, RaptorNode};
// `DocumentTypeTag::Unknown` is the closest neutral tag; conversation
// data does not match any of the per-genre variants (Narrative /
// Argument / Evidence / Chronicle / Technical), and `Unknown`'s
// prompt steering already says "Document" generically. A dedicated
// `Conversation` variant lands next session when the briefing layer
// needs to render conv-specific prompt framing.
use sovereign_store::sqlite::{
    ConvRaptorNodeRow, ConvSkeletonRow, ConvTieredState, SqliteStateStore,
};
use uuid::Uuid;

use crate::raptor_atlas::{build_raptor_atlas, ChunkInput};

/// Concrete provider wiring the corpus-engine dispatch trait to the
/// real RAPTOR builder + SQLite persistence.
///
/// ## Folder-corpus reuse convention
///
/// Watched-folder corpora reuse this provider's persistence shape
/// (`conv_raptor_nodes` / `conv_motifs` / `conv_skeletons`) by calling
/// `enrich_conversation` with `conv_uuid = corpus_id`. The bucket
/// classification (`ConvBucket::classify(chunks.len())`) handles
/// folder size variability — a 5-file folder takes the Tiny synthetic
/// path; a 5000-chunk vault takes the LongTail RAPTOR path. The
/// `FolderTieredProvider` sibling below wraps this with the folder
/// runner from `corpus-engine::enrichment::tiered::run_folder_tiered_enrichment`.
pub struct ConvTieredProvider {
    store: Arc<SqliteStateStore>,
    inference: Arc<dyn InferenceProvider>,
}

/// Concrete `ChunkEntityExtractor` impl for the daemon ingest path.
/// Wraps a single `GlinerExtractor` + `Arc<SqliteStateStore>` and
/// persists rows into `chunk_entities` per-conversation. Fires
/// from `corpus_engine::enrichment::tiered::run_tiered_enrichment`
/// ahead of the LLM-heavy `TieredEnrichmentProvider` call.
///
/// Feature-gated under `gliner-ner` like the underlying extractor.
#[cfg(feature = "gliner-ner")]
pub struct GlinerChunkExtractor {
    store: Arc<SqliteStateStore>,
    extractor: Arc<crate::gliner_ner::GlinerExtractor>,
}

#[cfg(feature = "gliner-ner")]
impl GlinerChunkExtractor {
    pub fn new(
        store: Arc<SqliteStateStore>,
        extractor: Arc<crate::gliner_ner::GlinerExtractor>,
    ) -> Self {
        Self { store, extractor }
    }

    pub fn into_handle(
        self,
    ) -> Arc<dyn ChunkEntityExtractor> {
        Arc::new(self)
    }
}

#[cfg(feature = "gliner-ner")]
impl GlinerChunkExtractor {
    /// Phase B incremental hook (spec
    /// `sovereign/docs/specs/PROGRESSIVE_ENRICHMENT.md` §"Incremental
    /// update strategy"). Scans `index_path` for chunks NOT yet in
    /// `chunk_entities` for `corpus_id` and runs GliNER only on the
    /// delta. Non-destructive: writes via `save_chunk_entities`
    /// (bulk-insert with REPLACE-on-conflict) so existing rows for
    /// untouched chunks survive.
    ///
    /// Updates `chunk_entity_progress` with `state = "incremental"`
    /// once the corpus has graduated from a one-shot Phase A backfill
    /// into the live-corpus mode. Always recomputes `chunks_total`
    /// against the current Lance set so the UI's progress bar
    /// reflects the growing corpus rather than the snapshot Phase A
    /// finished against.
    ///
    /// Best-effort by design: a missing index, an empty extractor
    /// model, or a transient store error logs + returns Ok(0). The
    /// caller (debouncer / sweep completion) treats Phase B as a
    /// nice-to-have on top of Phase A's snapshot.
    pub async fn extract_delta_for_corpus(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> Result<usize> {
        let index = CorpusIndex::open(index_path).await?;
        let groups = index.group_chunks_by_source_doc().await?;
        let total_chunks: usize = groups.values().map(|v| v.len()).sum();
        let already = self
            .store
            .list_extracted_chunk_ids_for_corpus(corpus_id)
            .await
            .map_err(|e| {
                Error::Database(format!(
                    "list_extracted_chunk_ids_for_corpus({corpus_id}): {e}"
                ))
            })?;

        let now = crate::gliner_ner::now_unix();
        let mut new_chunks_processed = 0usize;
        let mut new_mentions = 0usize;
        let mut high_chunk_id: Option<i64> = None;

        for (conv_uuid, chunk_ids) in groups.iter() {
            // Skip convs whose chunks are already fully covered to
            // avoid the per-conv index fetch on the steady-state
            // happy path (most convs unchanged between sweeps).
            if chunk_ids.iter().all(|id| already.contains(id)) {
                continue;
            }
            let rows = match index
                .chunks_for_source_doc_with_embeddings(conv_uuid)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        corpus = corpus_id,
                        conv = %conv_uuid,
                        error = %e,
                        "extract_delta_for_corpus: chunk fetch failed; skipping conv"
                    );
                    continue;
                }
            };
            let delta: Vec<EnrichmentChunkRow> = rows
                .into_iter()
                .map(|(row, _emb)| row)
                .filter(|row| !already.contains(&row.id))
                .collect();
            if delta.is_empty() {
                continue;
            }
            let texts: Vec<&str> = delta.iter().map(|c| c.content.as_str()).collect();
            let mention_batches = match self.extractor.extract_batch(&texts) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(
                        corpus = corpus_id,
                        conv = %conv_uuid,
                        error = %e,
                        "extract_delta_for_corpus: extract_batch failed; skipping conv"
                    );
                    continue;
                }
            };
            let mut conv_rows: Vec<sovereign_core::conv_tiered::ChunkEntityRow> =
                Vec::new();
            for (chunk, mentions) in delta.iter().zip(mention_batches.into_iter()) {
                for m in mentions {
                    conv_rows.push(m.into_row(
                        corpus_id,
                        chunk.id,
                        Some(conv_uuid),
                        now,
                    ));
                }
                let chunk_id = chunk.id as i64;
                high_chunk_id = Some(high_chunk_id.map_or(chunk_id, |p| p.max(chunk_id)));
            }
            new_chunks_processed += delta.len();
            new_mentions += conv_rows.len();
            if let Err(e) = self.store.save_chunk_entities(&conv_rows).await {
                tracing::warn!(
                    corpus = corpus_id,
                    conv = %conv_uuid,
                    error = %e,
                    "extract_delta_for_corpus: save_chunk_entities failed; continuing"
                );
            }
        }

        // Reconcile the progress row in either branch: when delta > 0
        // we bump counters + flip state; when delta == 0 but the prior
        // row says "complete" we still need to flip to "incremental"
        // + refresh chunks_total so the UI stops showing the
        // snapshot-time number forever on a live corpus.
        let existing = self
            .store
            .get_chunk_entity_progress(corpus_id)
            .await
            .map_err(|e| {
                Error::Database(format!("get_chunk_entity_progress({corpus_id}): {e}"))
            })?;
        let needs_write = new_chunks_processed > 0
            || existing
                .as_ref()
                .map(|p| p.state != "incremental" || p.chunks_total != total_chunks as i64)
                .unwrap_or(false);
        if needs_write {
            let labels_json = serde_json::to_string(&self.extractor.labels).ok();
            let prior_processed = existing.as_ref().map(|p| p.chunks_processed).unwrap_or(0);
            let prior_mentions = existing.as_ref().map(|p| p.mentions_extracted).unwrap_or(0);
            let started_at = existing.as_ref().map(|p| p.started_at).unwrap_or(now);
            let last_chunk_id = high_chunk_id
                .or_else(|| existing.as_ref().and_then(|p| p.last_chunk_id));
            let row = sovereign_core::conv_tiered::ChunkEntityProgressRow {
                corpus_id: corpus_id.to_string(),
                chunks_processed: prior_processed + new_chunks_processed as i64,
                chunks_total: total_chunks as i64,
                mentions_extracted: prior_mentions + new_mentions as i64,
                last_chunk_id,
                started_at,
                updated_at: now,
                // Clear `finished_at` — an incremental corpus is
                // never "done" until uninstalled. The UI distinguishes
                // "complete + finished" from "incremental +
                // auto-updating" via the state field alone.
                finished_at: None,
                state: "incremental".to_string(),
                model_id: Some(self.extractor.model_id.clone()),
                threshold: Some(self.extractor.threshold as f64),
                labels_json,
                error_msg: None,
            };
            if let Err(e) = self.store.upsert_chunk_entity_progress(&row).await {
                tracing::warn!(
                    corpus = corpus_id,
                    error = %e,
                    "extract_delta_for_corpus: progress upsert failed"
                );
            }
        }

        if new_chunks_processed > 0 {
            tracing::info!(
                corpus = corpus_id,
                new_chunks = new_chunks_processed,
                new_mentions,
                total_chunks,
                "extract_delta_for_corpus: incremental NER pass complete"
            );
        }
        Ok(new_mentions)
    }
}

#[cfg(feature = "gliner-ner")]
#[async_trait]
impl ChunkEntityExtractor for GlinerChunkExtractor {
    async fn extract_delta_for_corpus(
        &self,
        corpus_id: &str,
        index_path: &Path,
    ) -> Result<usize> {
        // Delegate to the inherent method so the trait + inherent
        // entry points stay bit-identical. Keeping the inherent
        // method also lets callers in sovereign-tools call directly
        // without paying for `Arc<dyn>` indirection.
        GlinerChunkExtractor::extract_delta_for_corpus(self, corpus_id, index_path).await
    }

    async fn extract_for_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
    ) -> Result<usize> {
        if chunks.is_empty() {
            return Ok(0);
        }
        let extracted_at = crate::gliner_ner::now_unix();
        let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let mention_batches = match self.extractor.extract_batch(&texts) {
            Ok(b) => b,
            Err(e) => {
                return Err(Error::Database(format!(
                    "GlinerChunkExtractor.extract_batch: {e}"
                )));
            }
        };
        let mut rows: Vec<sovereign_core::conv_tiered::ChunkEntityRow> = Vec::new();
        for (chunk, mentions) in chunks.iter().zip(mention_batches.into_iter()) {
            for m in mentions {
                rows.push(m.into_row(
                    corpus_id,
                    chunk.id,
                    Some(conv_uuid),
                    extracted_at,
                ));
            }
        }
        let count = rows.len();
        if let Err(e) = self
            .store
            .save_chunk_entities_for_conv(corpus_id, conv_uuid, &rows)
            .await
        {
            return Err(Error::Database(format!(
                "save_chunk_entities_for_conv({corpus_id}, {conv_uuid}): {e}"
            )));
        }
        Ok(count)
    }
}

impl ConvTieredProvider {
    pub fn new(store: Arc<SqliteStateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }

    pub fn into_handle(self) -> Arc<dyn TieredEnrichmentProvider> {
        Arc::new(self)
    }
}

#[async_trait]
impl TieredEnrichmentProvider for ConvTieredProvider {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> Result<()> {
        let chunk_count = chunks.len();
        let updated_at = Utc::now().timestamp();
        let title = conv_title_from_chunks(&chunks);

        let result: std::result::Result<Vec<ConvRaptorNodeRow>, Error> = match bucket {
            ConvBucket::Tiny => Ok(synthesize_tiny_node(
                corpus_id, conv_uuid, &title, &chunks, &embeddings, updated_at,
            )),
            ConvBucket::Small
            | ConvBucket::Medium
            | ConvBucket::Large
            | ConvBucket::LongTail => {
                build_raptor_rows(
                    corpus_id,
                    conv_uuid,
                    &chunks,
                    &embeddings,
                    self.inference.clone(),
                    updated_at,
                )
                .await
            }
        };

        match result {
            Ok(nodes) => {
                if let Err(e) = self
                    .store
                    .save_conv_raptor_nodes(corpus_id, conv_uuid, &nodes)
                    .await
                {
                    persist_state(
                        &self.store,
                        corpus_id,
                        conv_uuid,
                        ConvTieredState::Failed,
                        chunk_count,
                        Some(title.clone()),
                        updated_at,
                    )
                    .await;
                    return Err(Error::Database(format!(
                        "conv_tiered: save_conv_raptor_nodes({corpus_id}, {conv_uuid}): {e}"
                    )));
                }
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Ready,
                    chunk_count,
                    Some(title),
                    updated_at,
                )
                .await;
                Ok(())
            }
            Err(e) => {
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Failed,
                    chunk_count,
                    Some(title),
                    updated_at,
                )
                .await;
                Err(e)
            }
        }
    }
}

async fn persist_state(
    store: &SqliteStateStore,
    corpus_id: &str,
    conv_uuid: &str,
    state: ConvTieredState,
    chunk_count: usize,
    overview: Option<String>,
    updated_at: i64,
) {
    let row = ConvSkeletonRow {
        corpus_id: corpus_id.to_string(),
        conv_uuid: conv_uuid.to_string(),
        state: state.as_str().to_string(),
        skeleton_json: None,
        overview,
        segments_json: None,
        chunk_count: chunk_count as i64,
        updated_at,
    };
    if let Err(e) = store.save_conv_skeleton(&row).await {
        tracing::warn!(
            corpus = corpus_id,
            conv = conv_uuid,
            error = %e,
            state = state.as_str(),
            "conv_tiered: save_conv_skeleton failed"
        );
    }
}

/// Pull a representative conv title for opt-3 (reuse instead of an
/// LLM-generated overview). The threaded_turns chunker writes the
/// truncated first-message-of-conversation into the Lance `title`
/// column on every chunk in the conv, so we just take the first
/// non-empty value.
fn conv_title_from_chunks(chunks: &[EnrichmentChunkRow]) -> String {
    chunks
        .iter()
        .find_map(|c| c.title.as_deref().filter(|s| !s.trim().is_empty()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(untitled conversation)".to_string())
}

/// Opt-2: tiny conversations get a single synthetic node carrying the
/// conv title as `summary` and the mean of member embeddings as both
/// `summary_embedding` and `centroid_embedding`. Briefing layer can
/// still render a signpost — it just won't carry LLM paraphrasing.
fn synthesize_tiny_node(
    corpus_id: &str,
    conv_uuid: &str,
    title: &str,
    chunks: &[EnrichmentChunkRow],
    embeddings: &[Vec<f32>],
    updated_at: i64,
) -> Vec<ConvRaptorNodeRow> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let mean = mean_vector(embeddings);
    let chunk_ids: Vec<u64> = chunks.iter().map(|c| c.id).collect();
    let direct_members_json = serde_json::to_string(&chunk_ids).unwrap_or_else(|_| "[]".into());
    let evidence_json = direct_members_json.clone();
    let children_json = "[]".to_string();
    let quotes_json = "[]".to_string();
    let entities_json = "[]".to_string();
    let row = ConvRaptorNodeRow {
        node_id: Uuid::new_v4().to_string(),
        corpus_id: corpus_id.to_string(),
        conv_uuid: conv_uuid.to_string(),
        level: 0,
        summary: title.to_string(),
        summary_embedding: mean.clone(),
        centroid_embedding: mean,
        children_node_ids_json: children_json,
        direct_member_chunk_ids_json: Some(direct_members_json),
        evidence_chunk_ids_json: evidence_json,
        quote_spans_json: quotes_json,
        primary_entities_json: entities_json,
        cluster_coherence: 1.0,
        created_at: updated_at,
    };
    vec![row]
}

/// Non-Tiny path: call the corpus-agnostic `build_raptor_atlas` then
/// convert each `RaptorNode` to the conv-scoped `ConvRaptorNodeRow`
/// shape.
///
/// Note on `ChunkInput.chunk_id` (u32) vs Lance row `id` (u64): the
/// builder uses `chunk_id` as a position handle internally and to
/// index into `embeddings`. For conv corpora this is the Lance row
/// id, which historically fits in u32 well under the 4G ceiling.
/// On the day a single conv corpus crosses that line, we'll need to
/// widen ChunkInput; not load-bearing today.
async fn build_raptor_rows(
    corpus_id: &str,
    conv_uuid: &str,
    chunks: &[EnrichmentChunkRow],
    embeddings: &[Vec<f32>],
    inference: Arc<dyn InferenceProvider>,
    updated_at: i64,
) -> std::result::Result<Vec<ConvRaptorNodeRow>, Error> {
    let raptor_chunks: Vec<ChunkInput> = chunks
        .iter()
        .map(|c| ChunkInput {
            chunk_id: c.id as u32,
            content: c.content.clone(),
        })
        .collect();

    let nodes = build_raptor_atlas(
        &inference,
        &raptor_chunks,
        embeddings,
        DocumentTypeTag::Unknown,
    )
    .await
    .map_err(|e| {
        Error::Database(format!(
            "conv_tiered: build_raptor_atlas({corpus_id}, {conv_uuid}): {e}"
        ))
    })?;

    let mut rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        rows.push(raptor_node_to_row(node, corpus_id, conv_uuid, updated_at)?);
    }
    Ok(rows)
}

fn raptor_node_to_row(
    node: RaptorNode,
    corpus_id: &str,
    conv_uuid: &str,
    fallback_ts: i64,
) -> std::result::Result<ConvRaptorNodeRow, Error> {
    let children_json = serde_json::to_string(&node.children_node_ids)
        .map_err(|e| Error::Database(format!("children_node_ids serialize: {e}")))?;
    let direct_members_json: Option<String> = if node.direct_member_chunk_ids.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(&node.direct_member_chunk_ids)
                .map_err(|e| Error::Database(format!("direct_member_chunk_ids serialize: {e}")))?,
        )
    };
    let evidence_json = serde_json::to_string(&node.evidence_chunk_ids)
        .map_err(|e| Error::Database(format!("evidence_chunk_ids serialize: {e}")))?;
    let quotes_json = serde_json::to_string(&serialize_quote_spans(&node.quote_spans))
        .map_err(|e| Error::Database(format!("quote_spans serialize: {e}")))?;
    let entities_json = serde_json::to_string(&node.primary_entities)
        .map_err(|e| Error::Database(format!("primary_entities serialize: {e}")))?;
    let created_at = if node.created_at.timestamp() > 0 {
        node.created_at.timestamp()
    } else {
        fallback_ts
    };
    Ok(ConvRaptorNodeRow {
        node_id: node.node_id,
        corpus_id: corpus_id.to_string(),
        conv_uuid: conv_uuid.to_string(),
        level: node.level as i64,
        summary: node.summary,
        summary_embedding: node.summary_embedding,
        centroid_embedding: node.centroid_embedding,
        children_node_ids_json: children_json,
        direct_member_chunk_ids_json: direct_members_json,
        evidence_chunk_ids_json: evidence_json,
        quote_spans_json: quotes_json,
        primary_entities_json: entities_json,
        cluster_coherence: node.cluster_coherence as f64,
        created_at,
    })
}

/// Serialize `QuoteSpan` to the same JSON shape attached-doc
/// `raptor_nodes.quote_spans` uses, so a future briefing builder can
/// read both tables with one decoder.
fn serialize_quote_spans(spans: &[QuoteSpan]) -> Vec<serde_json::Value> {
    spans
        .iter()
        .map(|s| {
            serde_json::json!({
                "chunk_id": s.chunk_id,
                "char_start": s.char_start,
                "char_end": s.char_end,
                "text": s.text,
            })
        })
        .collect()
}

fn mean_vector(vectors: &[Vec<f32>]) -> Vec<f32> {
    if vectors.is_empty() {
        return Vec::new();
    }
    let dim = vectors[0].len();
    let mut out = vec![0f32; dim];
    let n = vectors.len() as f32;
    for v in vectors {
        for (i, val) in v.iter().enumerate().take(dim) {
            out[i] += *val;
        }
    }
    for slot in out.iter_mut() {
        *slot /= n;
    }
    out
}

// ─── Folder-corpus tiered provider ──────────────────────────────────
//
// Watched-folder corpora go through the same `conv_*` table shape but
// with `conv_uuid = corpus_id` and motif extraction enabled (the
// briefing layer surfaces motifs alongside RAPTOR signposts). Dispatch
// is via `corpus_engine::enrichment::tiered::run_folder_tiered_enrichment`,
// which collapses all source_doc_ids into one bag before calling
// `enrich_conversation`.

use sovereign_core::conv_tiered::ConvMotifRow;

/// Concrete tiered provider for watched-folder corpora. Persists into
/// the same SQLite tables as `ConvTieredProvider` (conv_raptor_nodes /
/// conv_motifs / conv_skeletons) under `conv_uuid = corpus_id`, plus
/// builds + saves the TF-IDF motif index that conversation provider
/// skips (folder briefings surface motifs as recurring-vocabulary
/// anchors per `TIERED_RETRIEVAL.md`).
pub struct FolderTieredProvider {
    store: Arc<SqliteStateStore>,
    inference: Arc<dyn InferenceProvider>,
    /// Resolves the index directory for a given corpus id. Required
    /// for the generic `_enrichment_state.json` sink so the daemon
    /// (and any restart's stall sweeper) can see progress without the
    /// provider hard-coding `~/.sovereign/indexes/`. Set when
    /// constructed from a daemon that knows its index root; left
    /// `None` in unit tests that don't need durable state.
    index_dir_resolver: Option<Arc<dyn IndexDirResolver>>,
}

/// Indirection so the provider can locate the per-corpus index dir
/// without importing `corpus_engine::CorpusEngine` (which already
/// depends on this crate via the conv tiered store).
pub trait IndexDirResolver: Send + Sync {
    fn resolve(&self, corpus_id: &str) -> Option<std::path::PathBuf>;
}

/// Concrete resolver that joins `corpus_id` onto a fixed root —
/// what the daemon installs.
pub struct StaticIndexDirResolver {
    pub indexes_root: std::path::PathBuf,
}

impl IndexDirResolver for StaticIndexDirResolver {
    fn resolve(&self, corpus_id: &str) -> Option<std::path::PathBuf> {
        let canonical = self.indexes_root.join(corpus_id);
        if canonical.exists() {
            Some(canonical)
        } else {
            None
        }
    }
}

impl FolderTieredProvider {
    pub fn new(store: Arc<SqliteStateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            store,
            inference,
            index_dir_resolver: None,
        }
    }

    /// Wire the per-corpus index-dir resolver so this provider
    /// publishes `_enrichment_state.json` while it runs. Daemons
    /// should always set this; tests can skip it.
    pub fn with_index_dir_resolver(
        mut self,
        resolver: Arc<dyn IndexDirResolver>,
    ) -> Self {
        self.index_dir_resolver = Some(resolver);
        self
    }

    pub fn into_handle(self) -> Arc<dyn TieredEnrichmentProvider> {
        Arc::new(self)
    }

    /// Stamp the `_enrichment_state.json` for a corpus to the given
    /// phase. Best-effort: a missing resolver or write failure logs
    /// but never short-circuits the enrichment body — the durable
    /// outcome lives in the SQLite store regardless.
    fn stamp_state(
        &self,
        corpus_id: &str,
        phase: corpus_engine::enrichment::state::EnrichmentPhase,
        step_current: u64,
        step_total: u64,
        message: Option<&str>,
    ) {
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            return;
        };
        let Some(index_dir) = resolver.resolve(corpus_id) else {
            return;
        };
        if let Err(e) = corpus_engine::enrichment::state::EnrichmentStateFile::stamp(
            &index_dir,
            corpus_id,
            Some("folder_tiered"),
            phase,
            step_current,
            step_total,
            message,
        ) {
            tracing::warn!(
                corpus = corpus_id,
                phase = phase.label(),
                error = %e,
                "folder_tiered: enrichment state stamp failed"
            );
        }
    }

    /// Build the per-cluster RAPTOR checkpoint and the
    /// state-file-backed progress sink for one enrich call, when the
    /// daemon wired an index-dir resolver. Returns `(None, None)` for
    /// resolver-less paths (unit tests) — the build path tolerates
    /// missing checkpoints + sinks by skipping the durable bits.
    fn build_checkpoint_and_sink(
        &self,
        corpus_id: &str,
        chunks: &[EnrichmentChunkRow],
        embeddings: &[Vec<f32>],
    ) -> (
        Option<crate::raptor_checkpoint::RaptorCheckpointHandle>,
        Option<Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    ) {
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            return (None, None);
        };
        let Some(index_dir) = resolver.resolve(corpus_id) else {
            return (None, None);
        };
        let chunk_ids: Vec<u32> = chunks.iter().map(|c| c.id as u32).collect();
        let embedding_dim = embeddings.first().map(|e| e.len()).unwrap_or(0);
        let input_hash =
            crate::raptor_checkpoint::RaptorCheckpointHandle::compute_input_hash(
                &chunk_ids,
                embedding_dim,
            );
        let checkpoint =
            crate::raptor_checkpoint::RaptorCheckpointHandle::at(&index_dir, input_hash);
        let sink: Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink> =
            Arc::new(corpus_engine::enrichment::state::StateFileSink::new(
                index_dir,
                corpus_id.to_string(),
                Some("folder_tiered".into()),
            ));
        (Some(checkpoint), Some(sink))
    }

    fn fail_state(&self, corpus_id: &str, error: &str) {
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            return;
        };
        let Some(index_dir) = resolver.resolve(corpus_id) else {
            return;
        };
        if let Err(e) = corpus_engine::enrichment::state::EnrichmentStateFile::fail(
            &index_dir, corpus_id, error,
        ) {
            tracing::warn!(
                corpus = corpus_id,
                error = %e,
                "folder_tiered: enrichment state fail-stamp failed"
            );
        }
    }

    /// Re-run per-source RAPTOR for only the source_doc_ids supplied.
    /// Called by the watched-folder sweeper after `apply_watched_diff`
    /// lands new chunks: instead of rebuilding the whole vault's
    /// RAPTOR forest (cost grows linearly with vault size), we
    /// re-enrich only the notes whose chunk set changed.
    ///
    /// Cheap-on-unchanged: the RAPTOR checkpoint inside
    /// `enrich_conversation` hashes the input chunk-id set + embedding
    /// dim. When the hash matches the prior run's checkpoint, the
    /// per-cluster LLM summarisation short-circuits (`Resume` rather
    /// than `Fresh`). So a sweeper that fires this for a note that
    /// only had a whitespace edit pays the cost of one Lance read +
    /// one checkpoint compare, no LLM work.
    ///
    /// After per-doc work completes the synthesis pass re-runs so
    /// `vault_themes` reflects the post-edit state.
    pub async fn reenrich_changed_sources(
        &self,
        corpus_id: &str,
        source_doc_ids: &[String],
    ) -> Result<()> {
        use corpus_engine::enrichment::tiered::ConvBucket;
        use corpus_engine::enrichment::tiered::TieredEnrichmentProvider;
        use corpus_engine::index::CorpusIndex;

        if source_doc_ids.is_empty() {
            return Ok(());
        }
        let Some(resolver) = self.index_dir_resolver.as_ref() else {
            tracing::warn!(
                corpus = corpus_id,
                "folder_tiered: reenrich_changed_sources called without index_dir_resolver; skipping"
            );
            return Ok(());
        };
        let Some(index_path) = resolver.resolve(corpus_id) else {
            tracing::warn!(
                corpus = corpus_id,
                "folder_tiered: reenrich_changed_sources could not resolve index dir; skipping"
            );
            return Ok(());
        };

        let index = CorpusIndex::open(&index_path).await.map_err(|e| {
            Error::Database(format!(
                "folder_tiered: open corpus index for incremental reenrich ({corpus_id}): {e}"
            ))
        })?;

        let mut reenriched = 0usize;
        let mut skipped_empty = 0usize;
        for doc_id in source_doc_ids {
            let rows = match index.chunks_for_source_doc_with_embeddings(doc_id).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        corpus = corpus_id,
                        doc = %doc_id,
                        error = %e,
                        "folder_tiered: incremental reenrich chunk fetch failed; skipping doc"
                    );
                    continue;
                }
            };
            if rows.is_empty() {
                // Deleted note — wipe its sidecar RAPTOR + themes
                // references will be reconciled next finalize.
                let _ = self
                    .store
                    .delete_conv_raptor_nodes_for_source(corpus_id, doc_id)
                    .await;
                skipped_empty += 1;
                continue;
            }
            let (chunks, embeddings): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
            let bucket = ConvBucket::classify(chunks.len());
            if let Err(e) = self
                .enrich_conversation(corpus_id, doc_id, chunks, embeddings, bucket)
                .await
            {
                tracing::warn!(
                    corpus = corpus_id,
                    doc = %doc_id,
                    error = %e,
                    "folder_tiered: incremental enrich_conversation failed for source_doc"
                );
                continue;
            }
            reenriched += 1;
        }

        tracing::info!(
            corpus = corpus_id,
            reenriched,
            skipped_empty,
            "folder_tiered: incremental reenrich done; re-running vault synthesis"
        );

        // Re-fire synthesis so vault_themes reflects post-edit state.
        // finalize_corpus tolerates failure (best-effort logging).
        let _ = self.finalize_corpus(corpus_id).await;
        Ok(())
    }

    /// Vault-wide RAPTOR synthesis pass. Implementation behind
    /// `finalize_corpus`. Public-ish (crate-private) so unit tests
    /// can exercise the synthesis output without going through the
    /// trait surface.
    ///
    /// Algorithm:
    /// 1. Enumerate all source_doc_ids whose `conv_skeletons.state`
    ///    is Ready for this corpus.
    /// 2. For each, fetch level-0 RAPTOR leaves; flatten into a
    ///    cross-vault input list of `(content, embedding)` pairs.
    ///    Track which source_doc each input came from.
    /// 3. Below the minimum-input-count gate (<8 leaves total across
    ///    the vault), skip — there isn't enough cross-note signal to
    ///    cluster usefully. Empty `vault_themes` is the right answer.
    /// 4. Run `build_raptor_atlas` over the flattened input. The
    ///    builder ascends levels until the cluster count drops to
    ///    `<= 4`; we take the highest level as the "themes".
    /// 5. For each theme node, project its `evidence_chunk_ids`
    ///    (which index back into our flattened input order) through
    ///    the source-doc sidecar to recover the contributing notes.
    /// 6. Persist as `VaultThemeRow`s.
    ///
    /// Returns the number of themes persisted (or 0 on a no-op skip).
    pub(crate) async fn run_vault_synthesis(
        &self,
        corpus_id: &str,
    ) -> Result<usize> {
        use crate::raptor_atlas::{build_raptor_atlas, ChunkInput};
        use sovereign_core::conv_tiered::VaultThemeRow;

        // Bound below which synthesis is pointless. 8 = a vault with
        // <2 notes (typical 4-5 leaves each) or one single
        // many-clustered note — neither has cross-note structure to
        // synthesise.
        const MIN_LEAVES_FOR_SYNTHESIS: usize = 8;

        let source_doc_ids = self
            .store
            .list_ready_source_doc_ids_for_corpus(corpus_id)
            .await
            .map_err(|e| {
                Error::Database(format!(
                    "vault_synthesis: list ready source_doc_ids ({corpus_id}): {e}"
                ))
            })?;
        if source_doc_ids.is_empty() {
            tracing::debug!(
                corpus = corpus_id,
                "vault_synthesis: no Ready conv_skeletons; skipping"
            );
            // Wipe any stale themes from a prior state where docs
            // were ready but have since been removed.
            let _ = self.store.delete_vault_themes_for_corpus(corpus_id).await;
            return Ok(0);
        }

        let mut chunks: Vec<ChunkInput> = Vec::new();
        let mut embeddings: Vec<Vec<f32>> = Vec::new();
        // Sidecar mapping: index into `chunks` -> source_doc_id that
        // contributed it. Used after RAPTOR returns to project
        // evidence_chunk_ids (which are u32 indices in 0..chunks.len())
        // back to the originating notes.
        let mut source_for_input: Vec<String> = Vec::new();
        for doc_id in &source_doc_ids {
            let nodes = match self
                .store
                .list_conv_raptor_nodes(corpus_id, doc_id)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        corpus = corpus_id,
                        doc = %doc_id,
                        error = %e,
                        "vault_synthesis: per-doc raptor fetch failed; excluding from synthesis"
                    );
                    continue;
                }
            };
            for node in nodes.iter().filter(|n| n.level == 0) {
                let next_id = chunks.len() as u32;
                chunks.push(ChunkInput {
                    chunk_id: next_id,
                    content: node.summary.clone(),
                });
                embeddings.push(node.summary_embedding.clone());
                source_for_input.push(doc_id.clone());
            }
        }

        if chunks.len() < MIN_LEAVES_FOR_SYNTHESIS {
            tracing::debug!(
                corpus = corpus_id,
                leaves = chunks.len(),
                min = MIN_LEAVES_FOR_SYNTHESIS,
                "vault_synthesis: too few level-0 leaves across vault; skipping"
            );
            let _ = self.store.delete_vault_themes_for_corpus(corpus_id).await;
            return Ok(0);
        }

        tracing::info!(
            corpus = corpus_id,
            notes = source_doc_ids.len(),
            leaves = chunks.len(),
            "vault_synthesis: building cross-note RAPTOR tree"
        );

        let nodes = build_raptor_atlas(
            &self.inference,
            &chunks,
            &embeddings,
            DocumentTypeTag::Unknown,
        )
        .await
        .map_err(|e| {
            Error::Database(format!(
                "vault_synthesis: build_raptor_atlas ({corpus_id}): {e}"
            ))
        })?;

        let max_level = nodes.iter().map(|n| n.level).max().unwrap_or(0);
        let now = Utc::now().timestamp();
        let themes: Vec<VaultThemeRow> = nodes
            .iter()
            .filter(|n| n.level == max_level)
            .enumerate()
            .map(|(idx, node)| {
                let mut members: Vec<String> = node
                    .evidence_chunk_ids
                    .iter()
                    .filter_map(|cid| source_for_input.get(*cid as usize).cloned())
                    .collect();
                members.sort();
                members.dedup();
                VaultThemeRow {
                    corpus_id: corpus_id.to_string(),
                    theme_id: format!("theme-{idx:03}"),
                    summary: node.summary.clone(),
                    summary_embedding: node.summary_embedding.clone(),
                    member_source_doc_ids_json: serde_json::to_string(&members)
                        .unwrap_or_else(|_| "[]".to_string()),
                    cluster_coherence: node.cluster_coherence,
                    created_at: now,
                }
            })
            .filter(|t| t.member_source_doc_ids_json != "[]")
            .collect();

        let theme_count = themes.len();
        self.store
            .save_vault_themes(corpus_id, &themes)
            .await
            .map_err(|e| {
                Error::Database(format!(
                    "vault_synthesis: save_vault_themes ({corpus_id}): {e}"
                ))
            })?;
        Ok(theme_count)
    }
}

#[async_trait]
impl TieredEnrichmentProvider for FolderTieredProvider {
    async fn enrich_conversation(
        &self,
        corpus_id: &str,
        conv_uuid: &str,
        chunks: Vec<EnrichmentChunkRow>,
        embeddings: Vec<Vec<f32>>,
        bucket: ConvBucket,
    ) -> Result<()> {
        use corpus_engine::enrichment::state::EnrichmentPhase;
        let chunk_count = chunks.len();
        let updated_at = Utc::now().timestamp();
        // Publish the entry point so the UI flips off the
        // indistinguishable "starting…" state the moment the
        // provider claims the corpus. Without this stamp, daemons
        // that restart mid-Scan look identical to daemons that never
        // dispatched at all.
        self.stamp_state(
            corpus_id,
            EnrichmentPhase::Scanning,
            0,
            chunk_count as u64,
            Some(&format!("loaded {chunk_count} chunks; bucket {}", bucket.label())),
        );
        // Folder corpora often have chunks tagged with the source file
        // name as `title`; reuse that as the overview when present,
        // otherwise stamp a stable "Folder: <id>" fallback so the
        // briefing renderer doesn't ship "(untitled conversation)" in
        // a watched-folder context.
        let chunk_title = conv_title_from_chunks(&chunks);
        let overview_title = if chunk_title == "(untitled conversation)" {
            format!("Folder: {corpus_id}")
        } else {
            chunk_title
        };

        let result: std::result::Result<(Vec<ConvRaptorNodeRow>, Vec<ConvMotifRow>), Error> =
            match bucket {
                ConvBucket::Tiny => Ok((
                    synthesize_tiny_node(
                        corpus_id,
                        conv_uuid,
                        &overview_title,
                        &chunks,
                        &embeddings,
                        updated_at,
                    ),
                    Vec::new(),
                )),
                ConvBucket::Small
                | ConvBucket::Medium
                | ConvBucket::Large
                | ConvBucket::LongTail => {
                    // Coarse phase stamp before the LLM-heavy build.
                    // build_atlas_artifacts is one big call today; we
                    // can't surface per-leaf progress without
                    // refactoring it. The Stalled sweeper at daemon
                    // start treats a stuck RaptorLeaves phase as
                    // interrupted, so even this coarse stamp is the
                    // difference between "stuck forever" and "shows
                    // 'interrupted, retry'".
                    self.stamp_state(
                        corpus_id,
                        EnrichmentPhase::RaptorLeaves,
                        0,
                        chunk_count as u64,
                        Some(&format!(
                            "building RAPTOR tree over {chunk_count} chunks ({} bucket)",
                            bucket.label()
                        )),
                    );
                    // Construct the per-cluster checkpoint handle +
                    // progress sink if we know where the index dir is.
                    // The handle is shaped against the input chunk IDs
                    // + embedding dim so re-runs after the chunk set
                    // changes invalidate cleanly.
                    let (checkpoint_owned, progress_sink_owned) = self
                        .build_checkpoint_and_sink(corpus_id, &chunks, embeddings.as_slice());
                    let checkpoint_ref = checkpoint_owned.as_ref();
                    let progress_ref = progress_sink_owned.as_ref();
                    build_folder_artifacts(
                        corpus_id,
                        conv_uuid,
                        &chunks,
                        &embeddings,
                        self.inference.clone(),
                        updated_at,
                        checkpoint_ref,
                        progress_ref,
                    )
                    .await
                }
            };

        match result {
            Ok((nodes, motifs)) => {
                self.stamp_state(
                    corpus_id,
                    EnrichmentPhase::Persisting,
                    0,
                    nodes.len() as u64,
                    Some(&format!("saving {} RAPTOR nodes", nodes.len())),
                );
                if let Err(e) = self
                    .store
                    .save_conv_raptor_nodes(corpus_id, conv_uuid, &nodes)
                    .await
                {
                    persist_state(
                        &self.store,
                        corpus_id,
                        conv_uuid,
                        ConvTieredState::Failed,
                        chunk_count,
                        Some(overview_title.clone()),
                        updated_at,
                    )
                    .await;
                    self.fail_state(
                        corpus_id,
                        &format!("save_conv_raptor_nodes: {e}"),
                    );
                    return Err(Error::Database(format!(
                        "folder_tiered: save_conv_raptor_nodes({corpus_id}, {conv_uuid}): {e}"
                    )));
                }
                if !motifs.is_empty() {
                    if let Err(e) = self
                        .store
                        .save_conv_motifs(corpus_id, conv_uuid, &motifs)
                        .await
                    {
                        // Best-effort. Motif failure degrades briefing
                        // signposts but the RAPTOR tree already
                        // persisted is the load-bearing retrieval
                        // signal.
                        tracing::warn!(
                            corpus = corpus_id,
                            conv = conv_uuid,
                            error = %e,
                            "folder_tiered: save_conv_motifs failed; continuing without motif index"
                        );
                    }
                }
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Ready,
                    chunk_count,
                    Some(overview_title),
                    updated_at,
                )
                .await;
                self.stamp_state(
                    corpus_id,
                    EnrichmentPhase::Complete,
                    nodes.len() as u64,
                    nodes.len() as u64,
                    Some(&format!(
                        "complete — {} nodes, {} motifs",
                        nodes.len(),
                        motifs.len()
                    )),
                );
                Ok(())
            }
            Err(e) => {
                persist_state(
                    &self.store,
                    corpus_id,
                    conv_uuid,
                    ConvTieredState::Failed,
                    chunk_count,
                    Some(overview_title),
                    updated_at,
                )
                .await;
                self.fail_state(corpus_id, &e.to_string());
                Err(e)
            }
        }
    }

    /// After every per-source `enrich_conversation` for this corpus
    /// has settled, run the vault-wide synthesis pass: cluster the
    /// per-note level-0 RAPTOR summaries into ~10-20 cross-note
    /// themes and persist them to `vault_themes`. The retrieval
    /// briefing surfaces these alongside per-note signposts to give
    /// the synth model "what does my whole vault say about X"
    /// context the per-note view alone doesn't carry.
    ///
    /// Best-effort: a failure here only kills the cross-note briefing
    /// block — the per-note tiered retrieval surface is fully
    /// functional regardless. We log + return Ok unless the failure
    /// is catastrophic enough to indicate a bug worth bubbling.
    async fn finalize_corpus(&self, corpus_id: &str) -> Result<()> {
        match self.run_vault_synthesis(corpus_id).await {
            Ok(theme_count) => {
                tracing::info!(
                    corpus = corpus_id,
                    themes = theme_count,
                    "folder_tiered: vault synthesis complete"
                );
            }
            Err(e) => {
                tracing::warn!(
                    corpus = corpus_id,
                    error = %e,
                    "folder_tiered: vault synthesis failed; cross-note briefing block will be empty until next enrichment"
                );
            }
        }

        // Typed-extension pass over RAPTOR leaves + vault_themes →
        // atoms.json. Bench-side concern (per
        // `sovereign/docs/specs/TYPED_EXTENSION_PASS.md`). Best-effort:
        // a failure here only loses bench-side typed atoms — chat-side
        // retrieval is unaffected. Skipped when the resolver doesn't
        // know this corpus's index dir (unit tests, transient bring-up).
        if let Some(resolver) = self.index_dir_resolver.as_ref() {
            if let Some(index_dir) = resolver.resolve(corpus_id) {
                let atlas_dir = index_dir.join("atlas");
                match crate::typed_extension::run_typed_extension(
                    corpus_id,
                    &self.store,
                    &self.inference,
                    &atlas_dir,
                )
                .await
                {
                    Ok(report) => {
                        tracing::info!(
                            corpus = corpus_id,
                            status = ?report.status,
                            pass_a = report.pass_a_calls,
                            pass_b = report.pass_b_calls,
                            soft_failures = report.soft_failures.len(),
                            "folder_tiered: typed extension complete"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            corpus = corpus_id,
                            error = %e,
                            "folder_tiered: typed extension failed; bench atoms.json will be stale until next enrichment"
                        );
                    }
                }
            } else {
                tracing::debug!(
                    corpus = corpus_id,
                    "folder_tiered: typed extension skipped — index dir resolver returned None"
                );
            }
        } else {
            tracing::debug!(
                corpus = corpus_id,
                "folder_tiered: typed extension skipped — no index dir resolver wired"
            );
        }

        Ok(())
    }
}

/// Folder Non-Tiny path: call the corpus-free `build_atlas_artifacts`
/// (sovereign-tools/src/document_asset.rs Move 3 helper) for both
/// RAPTOR nodes and TF-IDF motif index, then convert each to the
/// conv-scoped row shapes for persistence.
async fn build_folder_artifacts(
    corpus_id: &str,
    conv_uuid: &str,
    chunks: &[EnrichmentChunkRow],
    embeddings: &[Vec<f32>],
    inference: Arc<dyn InferenceProvider>,
    updated_at: i64,
    checkpoint: Option<&crate::raptor_checkpoint::RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
) -> std::result::Result<(Vec<ConvRaptorNodeRow>, Vec<ConvMotifRow>), Error> {
    let raptor_chunks: Vec<ChunkInput> = chunks
        .iter()
        .map(|c| ChunkInput {
            chunk_id: c.id as u32,
            content: c.content.clone(),
        })
        .collect();

    let (nodes, motifs) = crate::document_asset::build_atlas_artifacts_with_checkpoint(
        &inference,
        &raptor_chunks,
        embeddings,
        DocumentTypeTag::Unknown,
        checkpoint,
        progress,
    )
    .await
    .map_err(|e| {
        Error::Database(format!(
            "folder_tiered: build_atlas_artifacts({corpus_id}, {conv_uuid}): {e}"
        ))
    })?;

    let mut node_rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        node_rows.push(raptor_node_to_row(node, corpus_id, conv_uuid, updated_at)?);
    }

    let motif_rows: Vec<ConvMotifRow> = motifs
        .into_iter()
        .map(|m| {
            let occ_json = serde_json::to_string(&m.occurrence_chunk_ids)
                .unwrap_or_else(|_| "[]".into());
            ConvMotifRow {
                corpus_id: corpus_id.to_string(),
                conv_uuid: conv_uuid.to_string(),
                term: m.term,
                tf_idf_score: m.tf_idf_score as f64,
                occurrence_chunk_ids_json: occ_json,
                is_distinctive: m.is_distinctive,
            }
        })
        .collect();

    Ok((node_rows, motif_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_chunk(id: u64, content: &str, title: Option<&str>) -> EnrichmentChunkRow {
        EnrichmentChunkRow {
            id,
            content: content.to_string(),
            title: title.map(|s| s.to_string()),
            url: None,
            metadata_raw: None,
            source_doc_id: Some("conv-uuid-x".into()),
        }
    }

    #[test]
    fn title_pulled_from_first_non_empty_chunk_title() {
        let chunks = vec![
            mk_chunk(1, "msg 1", None),
            mk_chunk(2, "msg 2", Some("   ")),
            mk_chunk(3, "msg 3", Some("Real Conv Title")),
            mk_chunk(4, "msg 4", Some("Later Title")),
        ];
        assert_eq!(conv_title_from_chunks(&chunks), "Real Conv Title");
    }

    #[test]
    fn untitled_fallback_when_no_chunk_carries_title() {
        let chunks = vec![mk_chunk(1, "msg", None), mk_chunk(2, "msg", None)];
        assert_eq!(conv_title_from_chunks(&chunks), "(untitled conversation)");
    }

    #[test]
    fn mean_vector_averages_componentwise() {
        let vs = vec![vec![1.0, 2.0, 3.0], vec![3.0, 4.0, 5.0]];
        let m = mean_vector(&vs);
        assert_eq!(m, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn mean_vector_empty_returns_empty() {
        assert!(mean_vector(&[]).is_empty());
    }

    #[test]
    fn synthesize_tiny_node_uses_title_and_mean() {
        let chunks = vec![
            mk_chunk(10, "hi", Some("Convo About React")),
            mk_chunk(11, "more", None),
        ];
        let embeds = vec![vec![1.0, 0.0], vec![3.0, 0.0]];
        let rows =
            synthesize_tiny_node("corpus-x", "conv-y", "Convo About React", &chunks, &embeds, 42);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.summary, "Convo About React");
        assert_eq!(row.summary_embedding, vec![2.0, 0.0]);
        assert_eq!(row.centroid_embedding, vec![2.0, 0.0]);
        assert_eq!(row.level, 0);
        assert_eq!(row.corpus_id, "corpus-x");
        assert_eq!(row.conv_uuid, "conv-y");
        assert_eq!(row.created_at, 42);
        assert!(row.direct_member_chunk_ids_json.is_some());
        // Member chunk ids must round-trip the Lance row ids.
        let parsed: Vec<u64> = serde_json::from_str(
            row.direct_member_chunk_ids_json.as_ref().unwrap(),
        )
        .unwrap();
        assert_eq!(parsed, vec![10u64, 11]);
    }

    #[test]
    fn synthesize_tiny_node_empty_chunks_returns_empty() {
        let rows = synthesize_tiny_node("c", "u", "t", &[], &[], 0);
        assert!(rows.is_empty());
    }
}
