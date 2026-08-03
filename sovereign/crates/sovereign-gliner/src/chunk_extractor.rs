// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-chunk GLiNER entity extractor — the daemon ingest path's
//! [`ChunkEntityExtractor`]. Extracted from sovereign-tools'
//! `conv_tiered_provider` (2026-07-17) so the ONNX stack stays in this crate.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use corpus_engine::enrichment::tiered::ChunkEntityExtractor;
use corpus_engine::error::{Error, Result};
use corpus_engine::index::{CorpusIndex, EnrichmentChunkRow};
use sovereign_store::sqlite::SqliteStateStore;

use crate::labeled::LabeledEntityExtractor;

/// Concrete `ChunkEntityExtractor` impl for the daemon ingest path.
/// Wraps a [`LabeledEntityExtractor`] + `Arc<SqliteStateStore>` and
/// persists rows into `chunk_entities` per-conversation. Fires
/// from `corpus_engine::enrichment::tiered::run_tiered_enrichment`
/// ahead of the LLM-heavy `TieredEnrichmentProvider` call.
///
/// **Generation-agnostic since P2.1.** It holds the trait, not
/// `GlinerExtractor`, so v1 and GLiNER2 reach a corpus over the same
/// persistence, dedup, and progress-provenance code. Which one runs is
/// decided once, in [`crate::load_labeled_extractor`] — never here.
pub struct GlinerChunkExtractor {
    store: Arc<SqliteStateStore>,
    extractor: Arc<dyn LabeledEntityExtractor>,
}

impl GlinerChunkExtractor {
    pub fn new(store: Arc<SqliteStateStore>, extractor: Arc<dyn LabeledEntityExtractor>) -> Self {
        Self { store, extractor }
    }

    pub fn into_handle(self) -> Arc<dyn ChunkEntityExtractor> {
        Arc::new(self)
    }
}

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
        // "Already processed" must include chunks NER ran on but found
        // no entities in — otherwise those entity-less chunks (headers,
        // code, tables) write no `chunk_entities` row, look unprocessed
        // forever, and get re-run on every pass, never letting the delta
        // converge. `list_ner_processed_chunk_ids` unions the entity-
        // bearing chunks with the explicit processed markers.
        let already = self
            .store
            .list_ner_processed_chunk_ids(corpus_id)
            .await
            .map_err(|e| {
                Error::Database(format!("list_ner_processed_chunk_ids({corpus_id}): {e}"))
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
            let rows = match index.chunks_for_source_doc_with_embeddings(conv_uuid).await {
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
            // Capture the ids up front so we can mark them processed
            // after the batch — including chunks GliNER finds nothing in,
            // which is the whole point of the durable marker.
            let delta_ids: Vec<u64> = delta.iter().map(|c| c.id).collect();
            let texts: Vec<&str> = delta.iter().map(|c| c.content.as_str()).collect();
            let mention_batches = match self.extractor.extract_mentions_batch(&texts) {
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
            let mut conv_rows: Vec<sovereign_core::conv_tiered::ChunkEntityRow> = Vec::new();
            for (chunk, mentions) in delta.iter().zip(mention_batches) {
                for m in mentions {
                    conv_rows.push(m.into_row(corpus_id, chunk.id, Some(conv_uuid), now));
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
            // Mark every chunk we just ran NER on as processed — entities
            // or not — so the entity-less ones don't reappear in the delta
            // on the next pass. Recorded per-note (after its batch) so an
            // interrupted sweep still keeps the notes it finished. A
            // failure here only risks re-processing next time, never data
            // loss, so it is logged and swallowed like the save above.
            if let Err(e) = self
                .store
                .record_ner_processed_chunks(corpus_id, &delta_ids)
                .await
            {
                tracing::warn!(
                    corpus = corpus_id,
                    conv = %conv_uuid,
                    error = %e,
                    "extract_delta_for_corpus: record_ner_processed_chunks failed; continuing"
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
            .map_err(|e| Error::Database(format!("get_chunk_entity_progress({corpus_id}): {e}")))?;
        let needs_write = new_chunks_processed > 0
            || existing
                .as_ref()
                .map(|p| p.state != "incremental" || p.chunks_total != total_chunks as i64)
                .unwrap_or(false);
        if needs_write {
            let labels_json = serde_json::to_string(&self.extractor.labels()).ok();
            let prior_processed = existing.as_ref().map(|p| p.chunks_processed).unwrap_or(0);
            let prior_mentions = existing.as_ref().map(|p| p.mentions_extracted).unwrap_or(0);
            let started_at = existing.as_ref().map(|p| p.started_at).unwrap_or(now);
            let last_chunk_id =
                high_chunk_id.or_else(|| existing.as_ref().and_then(|p| p.last_chunk_id));
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
                model_id: Some(self.extractor.model_id().to_string()),
                threshold: Some(self.extractor.threshold() as f64),
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

#[async_trait]
impl ChunkEntityExtractor for GlinerChunkExtractor {
    async fn extract_delta_for_corpus(&self, corpus_id: &str, index_path: &Path) -> Result<usize> {
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
        let mention_batches = match self.extractor.extract_mentions_batch(&texts) {
            Ok(b) => b,
            Err(e) => {
                return Err(Error::Database(format!(
                    "GlinerChunkExtractor.extract_batch: {e}"
                )));
            }
        };
        let mut rows: Vec<sovereign_core::conv_tiered::ChunkEntityRow> = Vec::new();
        for (chunk, mentions) in chunks.iter().zip(mention_batches) {
            for m in mentions {
                rows.push(m.into_row(corpus_id, chunk.id, Some(conv_uuid), extracted_at));
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
