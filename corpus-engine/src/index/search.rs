// SPDX-License-Identifier: AGPL-3.0-or-later
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
/// Unit-normalized embedding for one row of a `FixedSizeListArray`.
/// `None` on null rows / downcast failure / zero norm — callers treat a
/// missing embedding as "row not eligible for coverage selection".
fn embedding_from_fixed_list(list: &FixedSizeListArray, row: usize) -> Option<Vec<f32>> {
    if list.is_null(row) {
        return None;
    }
    let value = list.value(row);
    let arr = value.as_any().downcast_ref::<Float32Array>()?;
    let mut v: Vec<f32> = arr.values().to_vec();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= 0.0 || !norm.is_finite() {
        return None;
    }
    for x in &mut v {
        *x /= norm;
    }
    Some(v)
}

/// Greedy facility-location selection: pick `k` of `n` candidates
/// maximizing `Σ_c rel[c] · max_{s∈S} sim(c, s)` — every candidate in the
/// pool wants a nearby selected representative, weighted by its own
/// relevance. Redundant near-duplicates (same article/section) add almost
/// no marginal coverage after the first is picked, so the budget spreads
/// across distinct regions of the pool instead of stacking one article.
/// This is the set-composition objective the per-chunk heuristics
/// (noise floor / per-article caps) approximated by side effect — see
/// sovereign/docs/RETRIEVAL_REDESIGN.md §3-4 and GeoRAG
/// (arXiv:2606.29328) for the measured basis. (1−1/e) greedy guarantee;
/// cost k·n² ≤ 30·200² — sub-millisecond-to-few-ms, no model calls.
///
/// `embs` must be unit-normalized. Returns selected indices in
/// descending-`rel` order so downstream score-ordering assumptions hold.
fn facility_location_select(embs: &[Vec<f32>], rel: &[f32], k: usize) -> Vec<usize> {
    let n = embs.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
    let mut sim = vec![0f32; n * n];
    for i in 0..n {
        for j in i..n {
            let d = dot(&embs[i], &embs[j]).max(0.0);
            sim[i * n + j] = d;
            sim[j * n + i] = d;
        }
    }
    let mut covered = vec![0f32; n];
    let mut in_set = vec![false; n];
    let mut selected = Vec::with_capacity(k.min(n));
    for _ in 0..k.min(n) {
        let mut best: Option<usize> = None;
        let mut best_gain = f32::NEG_INFINITY;
        for s in 0..n {
            if in_set[s] {
                continue;
            }
            let mut gain = 0f32;
            for c in 0..n {
                let sv = sim[c * n + s];
                if sv > covered[c] {
                    gain += rel[c] * (sv - covered[c]);
                }
            }
            if gain > best_gain {
                best_gain = gain;
                best = Some(s);
            }
        }
        let Some(s) = best else { break };
        in_set[s] = true;
        selected.push(s);
        for c in 0..n {
            let sv = sim[c * n + s];
            if sv > covered[c] {
                covered[c] = sv;
            }
        }
    }
    selected.sort_by(|a, b| rel[*b].partial_cmp(&rel[*a]).unwrap_or(std::cmp::Ordering::Equal));
    selected
}

use futures::TryStreamExt;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};

use super::sanitize_fts_query;
use crate::error::{Error, Result};
use crate::types::{DedupPicker, RerankConfig, RerankFn, ScoredChunk};

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
            if index_names.is_empty() {
                "  (none)".to_string()
            } else {
                index_names.join("\n")
            },
            if ivf_built { "YES" } else { "NO" },
            if content_fts { "YES" } else { "NO" },
            if title_fts { "YES" } else { "NO" },
        )
    }

    /// Cheap corpus-relevance probe: the smallest cosine distance (`1 -
    /// cos_sim`; LOWER = nearer) between `query_embedding` and this corpus's
    /// `k` nearest chunk vectors. Vector-only, and deliberately WITHOUT the
    /// relevance threshold that [`CorpusIndex::search`] applies in vector-only
    /// mode — this is a *ranking* signal ("does this corpus have ANY region
    /// near the query"), not a retrieval, so every corpus with a usable vector
    /// index returns a real number: a weak-but-present match ranks low instead
    /// of vanishing to empty (which is what made the naive `search(q,"",1)`
    /// probe fail open on unrelated corpora).
    ///
    /// `vector_distance` is the cross-corpus-comparable axis (see
    /// [`ScoredChunk::vector_distance`]); this returns it directly for the
    /// nearest chunk, so results are sortable ACROSS corpora. `None` only on
    /// genuine non-signal: empty query, no vector path (no IVF index and the
    /// table is above the flat-scan threshold), or no scoreable rows — callers
    /// should treat `None` as fail-OPEN (keep the corpus).
    /// ANN query parameters for every IVF-PQ search this index runs.
    ///
    /// `refine_factor` is load-bearing: IVF-PQ scores candidates on
    /// quantized codes, and at 1024d/64-subvector the quantization error
    /// alone costs ~35-40% of the true top-30 (measured 2026-07-16 on the
    /// wikipedia 1.9M-chunk and sep 188k-chunk indexes: recall@30 vs a
    /// near-exact reference was 0.59/0.66 without refinement, 0.98 with
    /// refine_factor=30 at +17-26ms). Refinement re-scores the PQ
    /// shortlist (`limit × refine_factor` rows) against exact vectors.
    /// Env overrides exist for ablation sweeps, not for production tuning.
    fn ann_params() -> (u32, Option<u32>) {
        static PARAMS: std::sync::OnceLock<(u32, Option<u32>)> = std::sync::OnceLock::new();
        *PARAMS.get_or_init(|| {
            let nprobes = std::env::var("SOVEREIGN_ANN_NPROBES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50);
            let refine = match std::env::var("SOVEREIGN_ANN_REFINE_FACTOR") {
                Ok(s) if s == "0" => None, // explicit opt-out for A/B
                Ok(s) => s.parse().ok().or(Some(30)),
                Err(_) => Some(30),
            };
            (nprobes, refine)
        })
    }

    /// Coverage-selection knob (RETRIEVAL_REDESIGN.md S1, experimental).
    /// `SOVEREIGN_COVERAGE_SELECT=1` turns it on: `search()` overfetches
    /// `limit × factor` (capped 200), then greedily composes the final
    /// `limit` by facility-location coverage instead of top-k truncation.
    /// `SOVEREIGN_COVERAGE_POOL_FACTOR` (default 4) tunes the overfetch.
    /// Returns `None` when off (byte-identical baseline behavior).
    fn coverage_params() -> Option<usize> {
        static PARAMS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
        *PARAMS.get_or_init(|| {
            let on = std::env::var("SOVEREIGN_COVERAGE_SELECT")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if !on {
                return None;
            }
            let factor = std::env::var("SOVEREIGN_COVERAGE_POOL_FACTOR")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|f| *f >= 2)
                .unwrap_or(4);
            Some(factor)
        })
    }

    /// Search-gate metadata: `(row_count, ivf_built, fts_built)`, cached
    /// per open dataset version (see the `gate_cache` field doc — the
    /// uncached reads cost ~1.1-2.3s per SEARCH on a 1.9M-row table and
    /// dominated retrieval wall time before 2026-07-16).
    async fn gate_info(&self) -> (usize, bool, bool) {
        if let Some(cached) = self.gate_cache.lock().ok().and_then(|g| *g) {
            return cached;
        }
        let row_count = self.table.count_rows(None).await.unwrap_or(usize::MAX);
        let indices = self.table.list_indices().await.unwrap_or_default();
        let ivf_built = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "embedding"));
        let fts_built = indices
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "content" || c == "title"));
        let info = (row_count, ivf_built, fts_built);
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = Some(info);
        }
        info
    }

    pub async fn nearest_vector_distance(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Option<f32>> {
        if query_embedding.is_empty() {
            return Ok(None);
        }
        const FLAT_SCAN_THRESHOLD: usize = 10_000;
        let (row_count, ivf_built, _) = self.gate_info().await;
        if !(ivf_built || row_count < FLAT_SCAN_THRESHOLD) {
            return Ok(None); // no vector path available for this corpus
        }
        let (nprobes, refine) = Self::ann_params();
        let mut q = self
            .table
            .query()
            .nearest_to(query_embedding.to_vec())
            .map_err(|e| Error::Database(format!("nearest_vector_distance query: {e}")))?
            .nprobes(nprobes as usize);
        if let Some(rf) = refine {
            q = q.refine_factor(rf);
        }
        let batches = q
            .limit(k.max(1))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("nearest_vector_distance exec: {e}")))?
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| Error::Database(format!("nearest_vector_distance collect: {e}")))?;
        let mut best: Option<f32> = None;
        for batch in &batches {
            let Some(fl) = batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
            else {
                continue;
            };
            for i in 0..batch.num_rows() {
                if let Some(d) = cosine_distance_from_fixed_list(fl, i, query_embedding) {
                    best = Some(best.map_or(d, |b| b.min(d)));
                }
            }
        }
        Ok(best)
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
        let t_meta = std::time::Instant::now();
        let (row_count, ivf_built, fts_indexed) = self.gate_info().await;
        let meta_ms = t_meta.elapsed().as_millis() as u64;
        let do_vector =
            !query_embedding.is_empty() && (ivf_built || row_count < FLAT_SCAN_THRESHOLD);
        let fts_built = !sanitized.is_empty() && fts_indexed;
        let do_fts = fts_built;

        // Coverage selection (S1): overfetch a wider pool, compose the
        // final `limit` by facility-location coverage instead of top-k
        // truncation. Vector leg required — selection runs on embeddings.
        let coverage_factor = Self::coverage_params().filter(|_| do_vector);
        let fetch_limit = coverage_factor
            .map(|f| (limit * f).clamp(limit, 200))
            .unwrap_or(limit);

        tracing::info!(
            corpus = %self.corpus_id,
            do_vector,
            do_fts,
            ivf_built,
            fts_built,
            row_count = row_count as u64,
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

        let (nprobes, refine) = Self::ann_params();
        let results = if do_vector && do_fts {
            // Hybrid: vector + FTS combined via reranking.
            let mut q = self
                .table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .full_text_search(FullTextSearchQuery::new(sanitized))
                .nprobes(nprobes as usize);
            if let Some(rf) = refine {
                q = q.refine_factor(rf);
            }
            q.limit(fetch_limit)
                .execute()
                .await
                .map_err(|e| Error::Database(format!("hybrid search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| Error::Database(format!("collect: {e}")))?
        } else if do_vector {
            // Vector-only search.
            let mut q = self
                .table
                .query()
                .nearest_to(query_embedding.to_vec())
                .map_err(|e| Error::Database(format!("vector query: {e}")))?
                .nprobes(nprobes as usize);
            if let Some(rf) = refine {
                q = q.refine_factor(rf);
            }
            q.limit(fetch_limit)
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
                .full_text_search(FullTextSearchQuery::new(sanitized))
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
            let col_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
            // Schema is fully static after index creation — once
            // an operator has confirmed the columns at TRACE on a
            // first run, nothing learns more from seeing them again
            // on every chat turn. Demoted from DEBUG to TRACE.
            tracing::trace!(columns = ?col_names, "CorpusIndex::search result schema");
        }

        let query_ms = t_search.elapsed().as_millis() as u64;
        let t_convert = std::time::Instant::now();

        // Convert Arrow RecordBatches to ScoredChunks.
        let mut scored = Vec::new();
        // Push-order-aligned unit embeddings, populated only when
        // coverage selection is active (they feed the selector below).
        let mut pool_embs: Vec<Option<Vec<f32>>> = Vec::new();
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
                let content = contents.map(|c| c.value(i).to_string()).unwrap_or_default();
                let title = titles.and_then(|t| {
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
                    if s.is_null(i) {
                        None
                    } else {
                        Some(s.value(i).to_string())
                    }
                });
                let vector_distance = if do_vector {
                    embedding_col
                        .and_then(|fl| cosine_distance_from_fixed_list(fl, i, query_embedding))
                } else {
                    None
                };
                if coverage_factor.is_some() {
                    pool_embs.push(embedding_col.and_then(|fl| embedding_from_fixed_list(fl, i)));
                }

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

        let convert_ms = t_convert.elapsed().as_millis() as u64;

        // Sort by score desc; with coverage selection active the
        // embeddings ride along so alignment survives the reorder.
        if coverage_factor.is_some() {
            let mut order: Vec<usize> = (0..scored.len()).collect();
            order.sort_by(|a, b| {
                scored[*b]
                    .score
                    .partial_cmp(&scored[*a].score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut s = Vec::with_capacity(scored.len());
            let mut e = Vec::with_capacity(pool_embs.len());
            for i in order {
                s.push(scored[i].clone());
                e.push(pool_embs[i].clone());
            }
            scored = s;
            pool_embs = e;
        } else {
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        // Apply score threshold only in vector-only mode, where score = 1/(1+cosine_distance)
        // and 0.45 corresponds to cosine_distance ≈ 1.22 (weak semantic match).
        // In hybrid mode, scores are RRF (_relevance_score ≈ 0.016) — incompatible scale,
        // so we let all results through and trust the model to judge relevance.
        let before_threshold = scored.len();
        if do_vector && !do_fts {
            let keep: Vec<bool> = scored.iter().map(|c| c.score >= 0.45).collect();
            let mut i = 0;
            scored.retain(|_| {
                let k = keep[i];
                i += 1;
                k
            });
            if coverage_factor.is_some() {
                let mut i = 0;
                pool_embs.retain(|_| {
                    let k = keep[i];
                    i += 1;
                    k
                });
            }
        }
        tracing::debug!(
            before = before_threshold,
            after = scored.len(),
            dropped = before_threshold - scored.len(),
            threshold_applied = do_vector && !do_fts,
            "CorpusIndex::search: threshold check"
        );

        // Coverage selection (S1): compose the final set by greedy
        // facility-location over the pool instead of top-k truncation.
        // Falls back to plain truncation when any pool row lacks an
        // embedding (degenerate corpora) so the flag can never lose rows.
        let mut select_ms = 0u64;
        let distinct_titles = |chunks: &[ScoredChunk]| -> usize {
            chunks
                .iter()
                .filter_map(|c| c.title.as_deref())
                .collect::<std::collections::HashSet<_>>()
                .len()
        };
        if coverage_factor.is_some() && scored.len() > limit {
            if pool_embs.iter().all(|e| e.is_some()) {
                let t_select = std::time::Instant::now();
                let embs: Vec<Vec<f32>> = pool_embs.iter().map(|e| e.clone().unwrap()).collect();
                let (lo, hi) = scored
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(lo, hi), c| {
                        (lo.min(c.score), hi.max(c.score))
                    });
                let span = (hi - lo).max(f32::EPSILON);
                let rel: Vec<f32> = scored.iter().map(|c| (c.score - lo) / span).collect();
                let selected = facility_location_select(&embs, &rel, limit);
                let titles_pool = distinct_titles(&scored);
                let picked: Vec<ScoredChunk> =
                    selected.iter().map(|&i| scored[i].clone()).collect();
                select_ms = t_select.elapsed().as_millis() as u64;
                tracing::info!(
                    corpus = %self.corpus_id,
                    pool = scored.len(),
                    out = picked.len(),
                    titles_pool,
                    titles_out = distinct_titles(&picked),
                    select_ms,
                    "CorpusIndex::search coverage_select"
                );
                scored = picked;
            } else {
                tracing::warn!(
                    corpus = %self.corpus_id,
                    "coverage_select: pool rows missing embeddings — falling back to top-k"
                );
                scored.truncate(limit);
            }
        } else {
            scored.truncate(limit);
        }
        tracing::info!(
            corpus = %self.corpus_id,
            results = scored.len(),
            meta_ms,
            query_ms,
            convert_ms,
            select_ms,
            total_ms = t_search.elapsed().as_millis() as u64 + meta_ms,
            "CorpusIndex::search complete"
        );
        Ok(scored)
    }

    /// Hybrid search + optional cross-encoder rerank.
    ///
    /// Behaves identically to `search()` when `rerank_fn` is `None`
    /// or `config.enabled` is false. When both are set, this method:
    ///   1. Pulls `config.candidates_k` candidates from LanceDB
    ///      (overfetch — usually 50, vs. the caller's typical limit
    ///      of 5-10);
    ///   2. Scores every candidate's content with the cross-encoder
    ///      in a single batched call (`RerankFn` returns one score
    ///      per doc in order);
    ///   3. Promotes the rerank logit to `ScoredChunk.score`,
    ///      stashes the original hybrid score in
    ///      `metadata["fusion_score"]` and the rerank logit in
    ///      `metadata["rerank_score"]` (both as f32-stringified);
    ///   4. Applies `config.min_score` threshold (if set), sorts by
    ///      rerank score descending, truncates to the caller's
    ///      `limit`.
    ///
    /// On reranker error the method falls back to the un-reranked
    /// hybrid result — observability via the warn-log, never a
    /// silent retrieval failure. The fall-through preserves the
    /// "rerank is purely additive" guarantee: enabling it should
    /// never make retrieval worse than baseline, even when the
    /// reranker model is missing or crashing.
    pub async fn search_with_rerank(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
        rerank_fn: Option<&RerankFn>,
        config: &RerankConfig,
        atlas_article_scores: Option<&std::collections::HashMap<String, f32>>,
    ) -> Result<Vec<ScoredChunk>> {
        // Per-corpus dedup filter — when set, dedup only fires for
        // corpora whose id is in the allowlist. SEP wins from dedup;
        // wiki regresses from it (RERANK_EXPERIMENT.md ablation), so
        // the right default is opt-in per corpus rather than a
        // single global toggle.
        let corpus_eligible_for_dedup = match &config.dedup_corpus_filter {
            Some(allow) => allow.contains(&self.corpus_id),
            None => true,
        };
        let effective_per_article = config.per_article && corpus_eligible_for_dedup;

        let has_reranker = config.enabled && rerank_fn.is_some();
        let dedup_only = config.enabled && effective_per_article && rerank_fn.is_none();
        // Run the overfetch+process path when either:
        //   - a reranker is wired (the normal rerank case), OR
        //   - per-article dedup is requested without a reranker (the
        //     ablation that tests whether the dedup mechanism alone
        //     captures the source-recall lift). With no reranker, the
        //     fusion score from `search()` is the only ordering
        //     signal — overfetch widens the pool so dedup can prune
        //     duplicate-source chunks and surface the next-best
        //     distinct source within the user's `limit`.
        let needs_overfetch = has_reranker || dedup_only;

        if !needs_overfetch {
            return self.search(query_embedding, query_text, limit).await;
        }

        // Overfetch: pull more candidates than the caller asked for so
        // the reranker / dedup has room to re-order. We grow the LanceDB
        // limit to `max(limit, candidates_k)` and re-truncate at the end.
        let overfetch = limit.max(config.candidates_k);
        let candidates = self.search(query_embedding, query_text, overfetch).await?;

        if candidates.is_empty() {
            return Ok(candidates);
        }

        let scores: Option<Vec<f32>> = if has_reranker {
            // Prefix the chunk with its title (when present) so the
            // reranker can see the source identity. SEP / Wikipedia
            // eval banks score by canonical source — without the
            // title, the cross-encoder has only the chunk body to
            // go on and tends to over-promote topical-but-non-canonical
            // chunks.
            let docs: Vec<String> = candidates
                .iter()
                .map(|c| match &c.title {
                    Some(t) => format!("Title: {t}\n\n{}", c.content),
                    None => c.content.clone(),
                })
                .collect();
            let rerank_fn = rerank_fn.expect("has_reranker => Some");
            let t_rerank = std::time::Instant::now();
            let rerank_result = rerank_fn(query_text, docs).await;
            let elapsed_ms = t_rerank.elapsed().as_millis() as u64;
            match rerank_result {
                Ok(s) if s.len() == candidates.len() => {
                    tracing::trace!(
                        corpus = %self.corpus_id,
                        rerank_ms = elapsed_ms,
                        n = s.len(),
                        "rerank scored"
                    );
                    Some(s)
                }
                Ok(s) => {
                    tracing::warn!(
                        corpus = %self.corpus_id,
                        expected = candidates.len(),
                        got = s.len(),
                        "rerank length mismatch — falling back to fusion order"
                    );
                    let mut fallback = candidates;
                    fallback.truncate(limit);
                    return Ok(fallback);
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %self.corpus_id,
                        error = %e,
                        "rerank failed — falling back to fusion order"
                    );
                    let mut fallback = candidates;
                    fallback.truncate(limit);
                    return Ok(fallback);
                }
            }
        } else {
            None
        };

        // Min-max normalise both fusion + rerank within the candidate
        // pool. Without this, the linear blend mixes incompatible
        // scales: fusion is typically [0.0, 0.05] in hybrid mode (RRF)
        // while jina-reranker-v3 emits logits in roughly [3, 6] — a
        // raw average would always be dominated by rerank. Normalising
        // both to [0, 1] inside the pool makes alpha mean what it says.
        // (Dedup-only ablation: skip this and keep fusion scores as-is.)
        let (fusion_min, fusion_max) = candidates
            .iter()
            .map(|c| c.score)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(mn, mx), s| {
                (mn.min(s), mx.max(s))
            });
        let alpha = config.alpha.clamp(0.0, 1.0);

        // Atlas-as-rerank-feature: when the caller has computed a
        // per-article atlas relevance map for this query, and the
        // config opts a non-zero weight on it, we min-max normalise
        // the per-candidate atlas score across the pool the same way
        // we do for rerank/fusion and add it as a third blend term.
        // Candidates whose source title isn't in the map score `0.0`
        // pre-normalisation — the intended bias: the atlas should
        // pull canonical-enriched articles up, not just reorder
        // among them.
        let atlas_active = config.atlas_weight.abs() > f32::EPSILON
            && atlas_article_scores.is_some()
            && has_reranker;
        let atlas_lookup = |chunk: &ScoredChunk| -> f32 {
            atlas_article_scores
                .and_then(|m| chunk.title.as_deref().and_then(|t| m.get(t)).copied())
                .unwrap_or(0.0)
        };
        let (atlas_min, atlas_max) = if atlas_active {
            candidates
                .iter()
                .map(atlas_lookup)
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(mn, mx), s| {
                    (mn.min(s), mx.max(s))
                })
        } else {
            (0.0, 0.0)
        };
        let atlas_span = (atlas_max - atlas_min).max(1e-6);

        // Promote blended score (or keep fusion score if no reranker);
        // preserve raw fusion + rerank in metadata.
        let mut reranked: Vec<ScoredChunk> = match scores {
            Some(scores) => {
                let (rerank_min, rerank_max) = scores
                    .iter()
                    .copied()
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(mn, mx), s| {
                        (mn.min(s), mx.max(s))
                    });
                let fusion_span = (fusion_max - fusion_min).max(1e-6);
                let rerank_span = (rerank_max - rerank_min).max(1e-6);
                candidates
                    .into_iter()
                    .zip(scores)
                    .map(|(mut chunk, logit)| {
                        let fusion_norm = (chunk.score - fusion_min) / fusion_span;
                        let rerank_norm = (logit - rerank_min) / rerank_span;
                        let raw_atlas = atlas_lookup(&chunk);
                        let atlas_norm = if atlas_active {
                            (raw_atlas - atlas_min) / atlas_span
                        } else {
                            0.0
                        };
                        let blended = alpha * rerank_norm
                            + (1.0 - alpha) * fusion_norm
                            + config.atlas_weight * atlas_norm;
                        chunk
                            .metadata
                            .insert("fusion_score".to_string(), format!("{:.6}", chunk.score));
                        chunk
                            .metadata
                            .insert("rerank_score".to_string(), format!("{:.6}", logit));
                        if atlas_active {
                            chunk
                                .metadata
                                .insert("atlas_score".to_string(), format!("{:.6}", raw_atlas));
                            chunk
                                .metadata
                                .insert("atlas_norm".to_string(), format!("{:.6}", atlas_norm));
                        }
                        chunk
                            .metadata
                            .insert("blended_score".to_string(), format!("{:.6}", blended));
                        chunk.score = blended;
                        chunk
                    })
                    .collect()
            }
            None => {
                // Dedup-only ablation: leave fusion score in place, only
                // tag metadata so downstream observability sees the path.
                candidates
                    .into_iter()
                    .map(|mut chunk| {
                        chunk
                            .metadata
                            .insert("fusion_score".to_string(), format!("{:.6}", chunk.score));
                        chunk
                            .metadata
                            .insert("rerank_mode".to_string(), "dedup_only".to_string());
                        chunk
                    })
                    .collect()
            }
        };

        let before = reranked.len();
        if let Some(min) = config.min_score {
            reranked.retain(|c| c.score >= min);
        }
        reranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if effective_per_article {
            // Pick the within-article winner via the configured
            // signal. FusedScore (the default) keeps the existing
            // behaviour — walk the already-score-sorted list and
            // take the first chunk per source. VectorDistance
            // re-orders by cosine-to-query first so the picker
            // selects the chunk whose embedding most resembles the
            // query, regardless of RRF placement.
            match config.dedup_picker {
                DedupPicker::FusedScore => {
                    // already sorted by score desc above
                }
                DedupPicker::VectorDistance => {
                    reranked.sort_by(|a, b| {
                        let av = a.vector_distance.unwrap_or(f32::INFINITY);
                        let bv = b.vector_distance.unwrap_or(f32::INFINITY);
                        av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
            }

            // Walk the (now-appropriately-sorted) list once; keep
            // the first chunk we see for each distinct source.
            // Sources without a `source_doc_id` fall back to
            // `title`; chunks with neither (rare) are bucketed
            // under their chunk_id so they don't collide.
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut deduped: Vec<ScoredChunk> = Vec::with_capacity(limit.max(16));
            for chunk in reranked.into_iter() {
                let key = chunk
                    .source_doc_id
                    .clone()
                    .or_else(|| chunk.title.clone())
                    .unwrap_or_else(|| format!("__chunk_{:?}", chunk.chunk_id));
                if seen.insert(key) {
                    deduped.push(chunk);
                }
            }
            reranked = deduped;

            // Re-sort by final score so the caller's top-K cut is
            // still relevance-ordered. Even when the picker was
            // VectorDistance, the across-article ranking should
            // respect the fused / blended score so chunks compete
            // on the same axis the operator chose.
            reranked.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let after = reranked.len();

        // Multi-article diversity (glassbox, OPT-IN). The regressed bench
        // metric is title-coverage — how many DISTINCT sources reach the
        // top-K. Counting distinct sources separates two failure modes
        // that look identical in the final score: "the right articles
        // never entered the candidate pool" (recall) vs "the rerank/atlas
        // blend collapsed them out of top-K" (diversity collapse).
        //
        // Cost discipline: every line of this is gated on the
        // `retrieval_audit` target being enabled. In production (target
        // off) we pay ONE atomic level-check + a zero-cost closure
        // literal — the HashSet passes and title clones never run. They
        // only execute when an investigator sets `retrieval_audit=info`.
        // No added allocation on the hot retrieval path otherwise.
        let audit_on = tracing::enabled!(target: "retrieval_audit", tracing::Level::INFO);
        let source_key = |c: &ScoredChunk| -> String {
            c.source_doc_id
                .clone()
                .or_else(|| c.title.clone())
                .unwrap_or_else(|| format!("__chunk_{:?}", c.chunk_id))
        };
        // Distinct sources in the pre-truncate scored pool (vs the
        // post-truncate top-K below) is the collapse-vs-recall tell.
        let pool_distinct_sources = if audit_on {
            reranked
                .iter()
                .map(&source_key)
                .collect::<std::collections::HashSet<_>>()
                .len()
        } else {
            0
        };

        reranked.truncate(limit);

        tracing::debug!(
            corpus = %self.corpus_id,
            candidates = before,
            kept = after,
            returned = reranked.len(),
            has_reranker,
            alpha,
            atlas_weight = config.atlas_weight,
            atlas_active,
            atlas_articles_scored = atlas_article_scores.map(|m| m.len()).unwrap_or(0),
            per_article_requested = config.per_article,
            per_article_applied = effective_per_article,
            min_score = ?config.min_score,
            "CorpusIndex::search_with_rerank complete"
        );

        // Sibling event on the shared `retrieval_audit` target so the
        // bench post-mortem can read per-corpus diversity. `returned_titles`
        // is the exact set the title-coverage metric scores against.
        if audit_on {
            let returned_distinct_sources = reranked
                .iter()
                .map(&source_key)
                .collect::<std::collections::HashSet<_>>()
                .len();
            let returned_titles: Vec<String> = reranked
                .iter()
                .map(|c| c.title.clone().unwrap_or_default())
                .collect();
            tracing::info!(
                target: "retrieval_audit",
                event = "rerank_diversity",
                corpus = %self.corpus_id,
                pool_chunks = before,
                pool_distinct_sources,
                returned_chunks = reranked.len(),
                returned_distinct_sources,
                atlas_active,
                returned_titles = ?returned_titles,
                "retrieval_audit: rerank_diversity"
            );
        }
        Ok(reranked)
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
                let content = contents.map(|c| c.value(i).to_string()).unwrap_or_default();
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
                    if s.is_null(i) {
                        None
                    } else {
                        Some(s.value(i).to_string())
                    }
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
