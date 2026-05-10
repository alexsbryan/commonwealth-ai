//! Hybrid search (vector + FTS) logic for CorpusIndex.

use std::collections::HashMap;

use arrow_array::{Array, FixedSizeListArray, Float32Array, Int64Array, StringArray};

/// Compute cosine distance (`1 - cos_sim`) between a row in a
/// `FixedSizeListArray<Float32>` (the chunk's stored embedding) and
/// the query embedding. Returns `None` when the row is null, the
/// dimensions don't match, or either vector is zero-norm. The
/// non-zero-norm guard matters because zero-vector embeddings have
/// occasionally appeared in older ingest paths (qwen-embedding-0.6b
/// returns zeros for empty content); cosine is undefined there and
/// the caller should fall back to RRF score.
fn cosine_distance_from_fixed_list(
    list: &FixedSizeListArray,
    row: usize,
    query: &[f32],
) -> Option<f32> {
    if list.is_null(row) || query.is_empty() {
        return None;
    }
    let value = list.value(row);
    let arr = value.as_any().downcast_ref::<Float32Array>()?;
    if arr.len() != query.len() {
        return None;
    }
    let chunk_vec = arr.values();
    let mut dot = 0.0f32;
    let mut q_norm = 0.0f32;
    let mut c_norm = 0.0f32;
    for (q, c) in query.iter().zip(chunk_vec.iter()) {
        dot += q * c;
        q_norm += q * q;
        c_norm += c * c;
    }
    let denom = (q_norm.sqrt()) * (c_norm.sqrt());
    if denom <= 0.0 || !denom.is_finite() {
        return None;
    }
    let sim = (dot / denom).clamp(-1.0, 1.0);
    Some(1.0 - sim)
}
use futures::TryStreamExt;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};

use super::sanitize_fts_query;
use crate::error::{Error, Result};
use crate::types::ScoredChunk;

use super::CorpusIndex;

impl CorpusIndex {
    /// Dump diagnostic information about this index's search readiness.
    /// Returns a human-readable report for debugging.
    pub async fn diagnose(&self) -> String {
        let row_count = self.table.count_rows(None).await.unwrap_or(0);
        let indices = self.table.list_indices().await.unwrap_or_default();
        let ivf_built = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "embedding"));
        let content_fts = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "content"));
        let title_fts = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "title"));

        let index_names: Vec<String> = indices
            .iter()
            .map(|idx| format!("  {} (columns: {})", idx.name, idx.columns.join(", ")))
            .collect();

        format!(
            "Corpus: {}\n\
             Rows: {}\n\
             Embedding dims: {}\n\
             LanceDB indices ({}):\n{}\n\
             IVF-PQ vector index: {}\n\
             FTS content index: {}\n\
             FTS title index: {}",
            self.corpus_id,
            row_count,
            self.embedding_dimensions,
            indices.len(),
            if index_names.is_empty() { "  (none)".to_string() } else { index_names.join("\n") },
            if ivf_built { "YES" } else { "NO" },
            if content_fts { "YES" } else { "NO" },
            if title_fts { "YES" } else { "NO" },
        )
    }

    /// Hybrid search combining vector similarity and FTS keyword matching.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        let sanitized = sanitize_fts_query(query_text);
        // Single live check — list_indices() only returns COMPLETE indices.
        // Gates both vector (IVF-PQ on "embedding") and FTS (Tantivy on "content"/"title").
        // Avoids 30-second flat scans when either index is absent or stale on large tables.
        // Exception: tables below FLAT_SCAN_THRESHOLD rows use brute-force scans
        // unconditionally — they're too small to warrant an IVF-PQ index and a flat
        // scan completes in milliseconds.
        const FLAT_SCAN_THRESHOLD: usize = 10_000;
        let row_count = self.table.count_rows(None).await.unwrap_or(usize::MAX);
        let indices = self.table.list_indices().await.unwrap_or_default();
        let ivf_built = indices.iter().any(|idx| idx.columns.iter().any(|c| c == "embedding"));
        let do_vector = !query_embedding.is_empty()
            && (ivf_built || row_count < FLAT_SCAN_THRESHOLD);
        let fts_built = !sanitized.is_empty()
            && indices.iter().any(|idx| idx.columns.iter().any(|c| c == "content" || c == "title"));
        let do_fts = fts_built;

        tracing::info!(
            corpus = %self.corpus_id,
            do_vector,
            do_fts,
            ivf_built,
            fts_built,
            row_count = row_count as u64,
            indices_count = indices.len(),
            stored_dims = self.embedding_dimensions,
            query_dims = query_embedding.len(),
            dims_match = (query_embedding.is_empty() || query_embedding.len() == self.embedding_dimensions),
            sanitized_query = %sanitized,
            "CorpusIndex::search gate"
        );

        if !do_vector && !do_fts {
            tracing::warn!(
                corpus = %self.corpus_id,
                "CorpusIndex::search: SKIPPED — no vector index and no FTS index available"
            );
            return Ok(Vec::new());
        }

        let t_search = std::time::Instant::now();

        let results = if do_vector && do_fts {
            // Hybrid: vector + FTS combined via reranking.
            self.table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .full_text_search(
                    FullTextSearchQuery::new(sanitized),
                )
                .nprobes(50)
                .limit(limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("hybrid search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        } else if do_vector {
            // Vector-only search.
            self.table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .nprobes(50)
                .limit(limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("vector search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        } else {
            // FTS-only search.
            self.table
                .query()
                .full_text_search(
                    FullTextSearchQuery::new(sanitized),
                )
                .limit(limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("FTS search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        };

        // Log the result schema once so we can see what score columns LanceDB returns.
        // Hybrid search may return _relevance_score or _score instead of _distance.
        if let Some(first) = results.first() {
            let schema = first.schema();
            let col_names: Vec<&str> = schema.fields().iter()
                .map(|f| f.name().as_str())
                .collect();
            // Schema is fully static after index creation — once
            // an operator has confirmed the columns at TRACE on a
            // first run, nothing learns more from seeing them again
            // on every chat turn. Demoted from DEBUG to TRACE.
            tracing::trace!(columns = ?col_names, "CorpusIndex::search result schema");
        }

        // Convert Arrow RecordBatches to ScoredChunks.
        let mut scored = Vec::new();
        for batch in &results {
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let urls = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadata_col = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let source_doc_id_col = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            // The chunk's stored embedding survives in the result
            // batch (hybrid path retains every base column). When
            // do_vector is true we use it to compute a real cosine
            // distance from the query — see `vector_distance` on
            // ScoredChunk for why this matters for cross-corpus
            // merge.
            let embedding_col = batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
            // LanceDB vector-only → _distance; hybrid → _relevance_score or _score.
            // Try all known column names so we can log which one is actually present.
            let distance_col = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            let relevance_col = batch
                .column_by_name("_relevance_score")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
            let score_col = batch
                .column_by_name("_score")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

            let num_rows = batch.num_rows();
            for i in 0..num_rows {
                let content = contents
                    .map(|c| c.value(i).to_string())
                    .unwrap_or_default();
                let title = titles.and_then(|t| {
                    if t.is_null(i) { None } else { Some(t.value(i).to_string()) }
                });
                let url = urls.and_then(|u| {
                    if u.is_null(i) { None } else { Some(u.value(i).to_string()) }
                });
                let metadata: HashMap<String, String> = metadata_col
                    .and_then(|m| {
                        if m.is_null(i) {
                            None
                        } else {
                            serde_json::from_str(m.value(i)).ok()
                        }
                    })
                    .unwrap_or_default();

                // Convert distance/relevance to a [0,1] score.
                // _distance: lower = better → score = 1/(1+d)
                // _relevance_score / _score: higher = better → use directly (already [0,1])
                let (score, score_source) = if let Some(d) = distance_col {
                    let dist = d.value(i);
                    (1.0_f32 / (1.0 + dist), "_distance")
                } else if let Some(r) = relevance_col {
                    (r.value(i), "_relevance_score")
                } else if let Some(s) = score_col {
                    (s.value(i), "_score")
                } else {
                    (1.0_f32, "none")
                };

                // Char-bounded preview: never slice on byte index 120
                // because a UTF-8 multi-byte char (e.g. `–`, `é`, CJK)
                // straddling that boundary panics with
                // "byte index N is not a char boundary".
                //
                // Per-rank lines run at TRACE: one chat turn fans out
                // across ≥6 corpora × 20 results = 120+ lines per
                // turn, which buries everything else in the log at
                // DEBUG. Operators who actually want to see the
                // ranking can flip the level for this module.
                let preview: String = content.chars().take(120).collect();
                tracing::trace!(
                    rank = i + 1,
                    score,
                    score_source,
                    title = title.as_deref().unwrap_or(""),
                    content_preview = preview.as_str(),
                    "CorpusIndex::search result"
                );

                let chunk_id = id_col.map(|c| c.value(i) as u64);
                let source_doc_id = source_doc_id_col.and_then(|s| {
                    if s.is_null(i) { None } else { Some(s.value(i).to_string()) }
                });
                let vector_distance = if do_vector {
                    embedding_col.and_then(|fl| {
                        cosine_distance_from_fixed_list(fl, i, query_embedding)
                    })
                } else {
                    None
                };

                scored.push(ScoredChunk {
                    content,
                    title,
                    url,
                    corpus_id: self.corpus_id.clone(),
                    score,
                    metadata,
                    chunk_id,
                    source_doc_id,
                    vector_distance,
                });
            }
        }

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        // Apply score threshold only in vector-only mode, where score = 1/(1+cosine_distance)
        // and 0.45 corresponds to cosine_distance ≈ 1.22 (weak semantic match).
        // In hybrid mode, scores are RRF (_relevance_score ≈ 0.016) — incompatible scale,
        // so we let all results through and trust the model to judge relevance.
        let before_threshold = scored.len();
        if do_vector && !do_fts {
            scored.retain(|c| c.score >= 0.45);
        }
        tracing::debug!(
            before = before_threshold,
            after = scored.len(),
            dropped = before_threshold - scored.len(),
            threshold_applied = do_vector && !do_fts,
            "CorpusIndex::search: threshold check"
        );
        scored.truncate(limit);
        tracing::debug!(
            results = scored.len(),
            elapsed_ms = t_search.elapsed().as_millis() as u64,
            "CorpusIndex::search complete"
        );
        Ok(scored)
    }

    /// Fetch every chunk whose `title` matches `title` exactly, up to
    /// `limit`. Unlike `search`, this is a cohesion-based pull — the
    /// caller has already decided this source is relevant and wants
    /// the whole document, not query-ranked hits.
    ///
    /// Returned `ScoredChunk`s carry `score = 1.0` uniformly. They are
    /// NOT query-similarity scores; callers mixing these with search
    /// results must either keep them separate or re-rank.
    ///
    /// Use-case: the "gold-mine source" expansion path in
    /// `handle_knowledge_query`. A hybrid search on "Can you tell me
    /// about X?" returns maybe 3 chunks of an X-titled note at the
    /// score ceiling and leaves the other ~10 chunks of that same note
    /// at the RRF noise floor because they don't vector-match the
    /// question phrasing. Once evidence-shape routing has identified
    /// the note as the dominant source, pulling the whole thing by
    /// title is strictly better than taking two noise chunks from each
    /// of four other corpora.
    pub async fn fetch_chunks_by_title(
        &self,
        title: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        if title.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Escape single quotes — LanceDB's only_if takes a SQL-ish
        // predicate string. Same defense as delete_chunks_by_source_doc.
        let safe = title.replace('\'', "''");
        let filter = format!("title = '{safe}'");
        let t_start = std::time::Instant::now();
        let batches: Vec<_> = self
            .table
            .query()
            .only_if(filter)
            .limit(limit)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("fetch_chunks_by_title: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("fetch_chunks_by_title collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let urls = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadata_col = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let source_doc_id_col = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                let content = contents
                    .map(|c| c.value(i).to_string())
                    .unwrap_or_default();
                let chunk_title = titles.and_then(|t| {
                    if t.is_null(i) {
                        None
                    } else {
                        Some(t.value(i).to_string())
                    }
                });
                let url = urls.and_then(|u| {
                    if u.is_null(i) {
                        None
                    } else {
                        Some(u.value(i).to_string())
                    }
                });
                let metadata: HashMap<String, String> = metadata_col
                    .and_then(|m| {
                        if m.is_null(i) {
                            None
                        } else {
                            serde_json::from_str(m.value(i)).ok()
                        }
                    })
                    .unwrap_or_default();

                let chunk_id = id_col.map(|c| c.value(i) as u64);
                let source_doc_id = source_doc_id_col.and_then(|s| {
                    if s.is_null(i) { None } else { Some(s.value(i).to_string()) }
                });

                out.push(ScoredChunk {
                    content,
                    title: chunk_title,
                    url,
                    corpus_id: self.corpus_id.clone(),
                    // Uniform 1.0 — this is a cohesion pull, not a
                    // similarity score. Callers must not mix with
                    // search-scored chunks in a single rank.
                    score: 1.0,
                    metadata,
                    chunk_id,
                    source_doc_id,
                    // Title-cohesion pulls don't run a vector query,
                    // so there's no comparable distance to record.
                    vector_distance: None,
                });
            }
        }

        tracing::debug!(
            corpus = %self.corpus_id,
            title,
            limit,
            results = out.len(),
            elapsed_ms = t_start.elapsed().as_millis() as u64,
            "CorpusIndex::fetch_chunks_by_title complete"
        );
        Ok(out)
    }
}
