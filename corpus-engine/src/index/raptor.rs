// SPDX-License-Identifier: AGPL-3.0-or-later
//! RAPTOR summary-node ANN index — a derived, per-corpus LanceDB table over
//! RAPTOR collapsed-tree *summary* embeddings, replacing the brute-force
//! cosine scan in `apply_raptor_grounding`. Kept **separate** from the leaf
//! `chunks.lance` so summary retrieval stays a distinct top-M (mixing them
//! would let leaf retrieval surface summaries organically and re-introduce
//! the displacement the late-injection design engineered out).
//!
//! **Layering.** This module is PURE LanceDB over plain row structs — it
//! imports nothing from the sovereign crates. The `conv_raptor_nodes` SQLite
//! read and the build-version stamp live on the sovereign side
//! (`sovereign_tools::raptor_index::build_corpus_raptor_index`), which maps
//! rows into [`RaptorSummaryRow`] and supplies `source_version`. Mirrors how
//! the RAPTOR *tree* builder (`build_raptor_atlas`) is injected from
//! sovereign-tools to avoid a cyclic dep (corpus-engine has no sovereign dep).
//!
//! **On disk**, under each corpus index dir `<index_root>/<corpus_id>/`:
//! ```text
//!   raptor_summaries.lance/      the LanceDB table (vector column `embedding`)
//!   raptor_summaries.meta.json   freshness sidecar (RaptorIndexMeta)
//! ```
//! The table dir is a sibling of `chunks.lance`. `installed_indexes()` walks
//! corpus *directories* keyed on `chunks.lance` + `_corpus_meta.json`, so this
//! sibling is invisible to that walk (no spurious `IndexInfo`, no
//! `CorpusKind` misclassification) and is opened DIRECTLY by path here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{
    types::Float32Type, Array, FixedSizeListArray, Float32Array, Int32Array, RecordBatch,
    StringArray,
};
use arrow_schema::SchemaRef;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::error::{Error, Result};

/// LanceDB table name; the on-disk dir is `<name>.lance`.
const RAPTOR_TABLE: &str = "raptor_summaries";
/// On-disk table dir under the corpus index dir.
pub const RAPTOR_LANCE_DIR: &str = "raptor_summaries.lance";
/// Freshness sidecar filename under the corpus index dir.
pub const RAPTOR_META_FILE: &str = "raptor_summaries.meta.json";
/// Bump when the table column layout changes.
pub const RAPTOR_SCHEMA_VERSION: u32 = 1;
/// Below this row count we skip the (lossy) IVF-PQ index entirely and let
/// LanceDB brute-force `nearest_to` — which is EXACT and, for the small
/// summary-node counts here, still sub-millisecond. Deliberately HIGHER than
/// the leaf `CorpusIndex::search` threshold (10k): leaf corpora are millions
/// of rows, but a corpus's RAPTOR *summary* nodes are far fewer, and the spec
/// pins the index pay-off at ~30–40k ("SEP's ~11k is fine as-is, no index").
/// So every current corpus stays on the exact flat path; IVF-PQ engages only
/// for genuinely wiki-scale summary trees, where the recall gate applies.
const FLAT_SCAN_THRESHOLD: usize = 30_000;

/// One RAPTOR summary node, as the index builder consumes it. The caller
/// (sovereign-tools) fills this from `ConvRaptorNodeRow`; this crate never
/// sees the SQLite type.
#[derive(Clone, Debug)]
pub struct RaptorSummaryRow {
    pub node_id: String,
    pub conv_uuid: String,
    pub level: i64,
    pub summary: String,
    pub embedding: Vec<f32>,
}

/// One search hit. `score` is the EXACT cosine similarity, recomputed from the
/// stored embedding rather than LanceDB's reported `_distance` (which carries
/// enough error on near-parallel vectors — ~5e-3 — to perturb both the score
/// and the boundary ranking). This makes it bit-comparable to the
/// `crate::atlas_context::cosine` values the brute-force scan produces, so the
/// index is a true drop-in. Downstream reweight/sort assume cosine-like scores.
#[derive(Clone, Debug)]
pub struct RaptorHit {
    pub node_id: String,
    pub conv_uuid: String,
    pub level: i64,
    pub summary: String,
    pub score: f32,
}

/// Freshness sidecar written next to the table. `source_version` is an opaque
/// monotonic build-version the caller supplies (sovereign passes
/// `max(created_at)` of the source `conv_raptor_nodes` rows); the query-time
/// freshness probe compares it against the live SQLite max to detect a table
/// built before the source rows last changed.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RaptorIndexMeta {
    pub source_version: i64,
    pub row_count: usize,
    pub dim: usize,
    pub schema_version: u32,
}

fn meta_path(corpus_dir: &Path) -> PathBuf {
    corpus_dir.join(RAPTOR_META_FILE)
}

fn lance_path(corpus_dir: &Path) -> PathBuf {
    corpus_dir.join(RAPTOR_LANCE_DIR)
}

/// Arrow schema for the derived table. The vector column is named `embedding`
/// (matching the leaf convention) so `create_index(&["embedding"], …)` is
/// uniform. Only the columns `apply_raptor_grounding` needs to rebuild the
/// virtual `ScoredChunk` — no `centroid_embedding` or JSON columns.
fn raptor_summary_schema(dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("node_id", DataType::Utf8, false),
        Field::new("conv_uuid", DataType::Utf8, false),
        Field::new("level", DataType::Int32, false),
        Field::new("summary", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim as i32,
            ),
            false,
        ),
    ]))
}

/// Read the freshness sidecar, if present and parseable. `None` when the
/// table has never been built or the sidecar is missing/corrupt — the caller
/// treats that as "no index, use the scan."
pub fn read_raptor_meta(corpus_dir: &Path) -> Option<RaptorIndexMeta> {
    let s = std::fs::read_to_string(meta_path(corpus_dir)).ok()?;
    serde_json::from_str(&s).ok()
}

/// Build (or rebuild) the derived `raptor_summaries.lance` table for one
/// corpus from `rows`, plus the freshness sidecar. **Idempotent**: any
/// existing table dir is removed first, so this is a full rebuild every call
/// (the table is a pure derivative of `conv_raptor_nodes`).
///
/// `source_version` is stamped into the sidecar verbatim (the caller's build
/// version — sovereign passes `max(created_at)` of the source rows). Returns
/// the number of rows written. Empty `rows` writes nothing and returns 0.
///
/// The embedding dimension is DERIVED from the data (`rows[0].embedding`),
/// never hardcoded — the embed model owns it. Rows whose embedding length
/// differs from the first row's are dropped with a warning (corruption guard;
/// in practice all rows share one model's dimension).
pub async fn build_raptor_index(
    corpus_dir: &Path,
    rows: &[RaptorSummaryRow],
    source_version: i64,
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let dim = rows[0].embedding.len();
    if dim == 0 {
        return Err(Error::Database(
            "raptor index: first row has a zero-length embedding".into(),
        ));
    }
    // Corruption guard — keep only rows matching the model's dimension.
    let kept: Vec<&RaptorSummaryRow> = rows.iter().filter(|r| r.embedding.len() == dim).collect();
    let dropped = rows.len() - kept.len();
    if dropped > 0 {
        tracing::warn!(
            dropped,
            dim,
            "raptor index: dropped rows with mismatched embedding dim"
        );
    }
    if kept.is_empty() {
        return Ok(0);
    }

    // Idempotent rebuild: remove only OUR table dir (the `ends_with` guard
    // ensures we never touch the sibling `chunks.lance`), then create fresh.
    let lance_dir = lance_path(corpus_dir);
    if lance_dir.ends_with(RAPTOR_LANCE_DIR) && lance_dir.exists() {
        std::fs::remove_dir_all(&lance_dir)
            .map_err(|e| Error::Database(format!("raptor index: clear old table: {e}")))?;
    }
    std::fs::create_dir_all(corpus_dir)?;

    let db = lancedb::connect(corpus_dir.to_str().ok_or_else(|| {
        Error::Database("raptor index: corpus dir path is not valid UTF-8".into())
    })?)
    .execute()
    .await
    .map_err(|e| Error::Database(format!("raptor index: connect: {e}")))?;

    let schema = raptor_summary_schema(dim);
    let table = db
        .create_empty_table(RAPTOR_TABLE, schema.clone())
        .execute()
        .await
        .map_err(|e| Error::Database(format!("raptor index: create table: {e}")))?;

    // Assemble one RecordBatch (column order MUST match `schema`).
    let node_ids: Vec<&str> = kept.iter().map(|r| r.node_id.as_str()).collect();
    let conv_uuids: Vec<&str> = kept.iter().map(|r| r.conv_uuid.as_str()).collect();
    let levels: Vec<i32> = kept.iter().map(|r| r.level as i32).collect();
    let summaries: Vec<&str> = kept.iter().map(|r| r.summary.as_str()).collect();
    let embedding_array = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        kept.iter()
            .map(|r| Some(r.embedding.iter().map(|&v| Some(v)))),
        dim as i32,
    );
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(node_ids)),
            Arc::new(StringArray::from(conv_uuids)),
            Arc::new(Int32Array::from(levels)),
            Arc::new(StringArray::from(summaries)),
            Arc::new(embedding_array),
        ],
    )
    .map_err(|e| Error::Serialization(format!("raptor index: record batch: {e}")))?;

    table
        .add(vec![batch])
        .execute()
        .await
        .map_err(|e| Error::Database(format!("raptor index: add rows: {e}")))?;

    let n = kept.len();
    // No-index-under-threshold + IVF-PQ-above. Below `FLAT_SCAN_THRESHOLD`,
    // LanceDB's `nearest_to` brute-forces exactly (no PQ quantization loss) —
    // a quantization miss on top-8 whole-doc-summary retrieval would drop the
    // exact relevant summary raptor adds, so we only accept PQ at wiki-scale
    // where memory forces it (gated by the recall test). IVF-FLAT is not used
    // (unverified availability in lancedb 0.27).
    let indexed = n >= FLAT_SCAN_THRESHOLD;
    if indexed {
        let num_partitions = ((n as f64).sqrt() as u32).clamp(8, 4096);
        table
            .create_index(
                &["embedding"],
                lancedb::index::Index::IvfPq(
                    lancedb::index::vector::IvfPqIndexBuilder::default()
                        .num_partitions(num_partitions)
                        .distance_type(lancedb::DistanceType::Cosine),
                ),
            )
            .replace(true)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("raptor index: create IVF-PQ: {e}")))?;
    }

    // Stamp the freshness sidecar (single writer for the whole file).
    let meta = RaptorIndexMeta {
        source_version,
        row_count: n,
        dim,
        schema_version: RAPTOR_SCHEMA_VERSION,
    };
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| Error::Serialization(format!("raptor index: meta: {e}")))?;
    std::fs::write(meta_path(corpus_dir), json)?;

    tracing::info!(
        rows = n,
        dim,
        indexed,
        source_version,
        "raptor index: built"
    );
    Ok(n)
}

/// Exact cosine similarity between `query` and row `row` of a stored
/// `FixedSizeList<Float32>` embedding column. Mirrors the leaf path's
/// `cosine_distance_from_fixed_list` (search.rs) and `atlas_context::cosine`'s
/// semantics (0 on null / dim-mismatch / zero-norm), so the index's score is
/// bit-comparable to the brute-force scan it replaces.
fn cosine_from_list_row(list: &FixedSizeListArray, row: usize, query: &[f32]) -> f32 {
    if list.is_null(row) {
        return 0.0;
    }
    let value = list.value(row);
    let arr = match value.as_any().downcast_ref::<Float32Array>() {
        Some(a) => a,
        None => return 0.0,
    };
    if arr.len() != query.len() {
        return 0.0;
    }
    let v = arr.values();
    let mut dot = 0.0f32;
    let mut nq = 0.0f32;
    let mut nv = 0.0f32;
    for (q, x) in query.iter().zip(v.iter()) {
        dot += q * x;
        nq += q * q;
        nv += x * x;
    }
    let denom = nq.sqrt() * nv.sqrt();
    if denom <= 0.0 || !denom.is_finite() {
        return 0.0;
    }
    (dot / denom).clamp(-1.0, 1.0)
}

/// Search the derived raptor table for one corpus. Opens
/// `<corpus_dir>/raptor_summaries.lance` directly. Returns up to `fetch_m`
/// hits ordered by descending similarity; `score` is the EXACT cosine
/// similarity recomputed from the stored embedding (LanceDB's `nearest_to` is
/// used only as the candidate generator — over-fetch `fetch_m` so the exact
/// re-rank has a wide net).
///
/// `min_level` filtering and dedupe are the **caller's** job
/// (`apply_raptor_grounding` over-fetches `fetch_m = top_m * K`, then filters
/// by level, dedupes by `conv_uuid`, and truncates) — the `only_if` +
/// `nearest_to` push-down is unverified on lancedb 0.27, and M is tiny.
///
/// Returns `Ok(vec![])` (NOT `Err`) when the table is absent or unreadable,
/// so the caller's empty-branch fallback to the brute-force scan fires.
pub async fn search_raptor_summaries(
    corpus_dir: &Path,
    query_emb: &[f32],
    fetch_m: usize,
) -> Result<Vec<RaptorHit>> {
    if !lance_path(corpus_dir).exists() || query_emb.is_empty() || fetch_m == 0 {
        return Ok(Vec::new());
    }
    let db = lancedb::connect(corpus_dir.to_str().ok_or_else(|| {
        Error::Database("raptor search: corpus dir path is not valid UTF-8".into())
    })?)
    .execute()
    .await
    .map_err(|e| Error::Database(format!("raptor search: connect: {e}")))?;
    // Table dir exists but no committed table (mid-rebuild / corrupt) → fall
    // back to the scan rather than erroring the whole grounding pass.
    let table = match db.open_table(RAPTOR_TABLE).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };

    let results: Vec<RecordBatch> = table
        .query()
        .nearest_to(query_emb.to_vec())
        .map_err(|e| Error::Database(format!("raptor search: nearest_to: {e}")))?
        .nprobes(50)
        .limit(fetch_m)
        .execute()
        .await
        .map_err(|e| Error::Database(format!("raptor search: execute: {e}")))?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| Error::Database(format!("raptor search: collect: {e}")))?;

    let mut hits = Vec::new();
    for batch in &results {
        let node_ids = batch
            .column_by_name("node_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let conv_uuids = batch
            .column_by_name("conv_uuid")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let levels = batch
            .column_by_name("level")
            .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
        let summaries = batch
            .column_by_name("summary")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        // Recompute the EXACT cosine from the stored embedding rather than
        // trusting LanceDB's `_distance` (it carries ~5e-3 error on
        // near-parallel vectors — enough to perturb the score and flip
        // boundary ranking). The leaf path does the same for its
        // `vector_distance`. `nearest_to` above is just the candidate
        // generator; this is the authoritative score.
        let emb_col = batch
            .column_by_name("embedding")
            .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());

        for i in 0..batch.num_rows() {
            let node_id = node_ids.map(|c| c.value(i).to_string()).unwrap_or_default();
            let conv_uuid = conv_uuids
                .map(|c| c.value(i).to_string())
                .unwrap_or_default();
            let level = levels.map(|c| c.value(i) as i64).unwrap_or(0);
            let summary = summaries
                .map(|c| c.value(i).to_string())
                .unwrap_or_default();
            let score = emb_col
                .map(|fl| cosine_from_list_row(fl, i, query_emb))
                .unwrap_or(0.0);
            hits.push(RaptorHit {
                node_id,
                conv_uuid,
                level,
                summary,
                score,
            });
        }
    }
    // Exact-cosine descending. LanceDB's candidate order is approximate; the
    // caller re-sorts globally across corpora, but a clean per-corpus order
    // keeps the over-fetch truncation honest.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|y| y * y).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            return 0.0;
        }
        dot / (na * nb)
    }

    /// Deterministic dim-8 vector seeded by `i` (no two seeds collide, so the
    /// cosine ranking has no fp ties at the top-K boundary).
    fn emb(i: usize) -> Vec<f32> {
        (0..8usize)
            .map(|d| {
                let v = i
                    .wrapping_mul(131)
                    .wrapping_add(d.wrapping_mul(977))
                    .wrapping_add(7)
                    % 1000;
                v as f32 / 500.0 - 1.0
            })
            .collect()
    }

    fn rows(n: usize) -> Vec<RaptorSummaryRow> {
        (0..n)
            .map(|i| RaptorSummaryRow {
                node_id: format!("n{i}"),
                conv_uuid: format!("https://plato.stanford.edu/entries/e{i}/"),
                level: (i % 3) as i64,
                summary: format!("summary {i}"),
                embedding: emb(i),
            })
            .collect()
    }

    #[tokio::test]
    async fn build_search_round_trip_and_meta() {
        let dir = tempdir().unwrap();
        let corpus = dir.path();
        let written = build_raptor_index(corpus, &rows(20), 4242).await.unwrap();
        assert_eq!(written, 20);

        // Sidecar round-trips the build version + dims.
        let meta = read_raptor_meta(corpus).expect("sidecar present");
        assert_eq!(meta.source_version, 4242);
        assert_eq!(meta.row_count, 20);
        assert_eq!(meta.dim, 8);
        assert_eq!(meta.schema_version, RAPTOR_SCHEMA_VERSION);

        // Querying with row 5's exact embedding returns it at the top with
        // score ≈ 1.0 (self-cosine), carrying its conv_uuid + level intact.
        let hits = search_raptor_summaries(corpus, &emb(5), 4).await.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].node_id, "n5");
        assert_eq!(hits[0].conv_uuid, "https://plato.stanford.edu/entries/e5/");
        assert_eq!(hits[0].level, (5 % 3) as i64);
        assert!(
            (hits[0].score - 1.0).abs() < 1e-3,
            "self-cosine ≈ 1, got {}",
            hits[0].score
        );
    }

    /// Load-bearing parity test: the search must return the same top-M node
    /// set as a brute-force cosine over the same rows, and the returned `score`
    /// must equal cosine similarity (pinning the `score = 1 - _distance`
    /// mapping). At this scale (<10k) LanceDB flat-scans, so it is exact.
    #[tokio::test]
    async fn parity_with_brute_force_cosine() {
        let dir = tempdir().unwrap();
        let corpus = dir.path();
        let data = rows(40);
        build_raptor_index(corpus, &data, 1).await.unwrap();

        let cos_of = |node: &str, q: &[f32]| -> f32 {
            cosine_sim(
                q,
                &data.iter().find(|r| r.node_id == node).unwrap().embedding,
            )
        };

        const TOP_K: usize = 8;
        for q_seed in [1001usize, 2002, 3003] {
            let q = emb(q_seed);

            // Brute-force top-K node set by exact cosine.
            let mut ranked: Vec<(&str, f32)> = data
                .iter()
                .map(|r| (r.node_id.as_str(), cosine_sim(&q, &r.embedding)))
                .collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let expected: std::collections::HashSet<&str> =
                ranked.iter().take(TOP_K).map(|(n, _)| *n).collect();

            // Fetch a wide net (all rows) so LanceDB's approximate candidate
            // SELECTION never limits the exact re-rank; search sorts by exact
            // cosine, so the first TOP_K are the genuine nearest neighbours.
            let hits = search_raptor_summaries(corpus, &q, data.len())
                .await
                .unwrap();
            let got: std::collections::HashSet<&str> = hits
                .iter()
                .take(TOP_K)
                .map(|h| h.node_id.as_str())
                .collect();
            assert_eq!(
                got, expected,
                "q_seed={q_seed}: ANN exact-rerank top-{TOP_K} != brute-force top-{TOP_K}"
            );

            // Score == exact cosine similarity (pins the recomputed-from-stored
            // -embedding score path; the scan uses the same `cosine`).
            for h in hits.iter().take(TOP_K) {
                let truth = cos_of(&h.node_id, &q);
                assert!(
                    (h.score - truth).abs() < 1e-5,
                    "q_seed={q_seed}: score {} != cosine {} for {}",
                    h.score,
                    truth,
                    h.node_id
                );
            }
        }
    }

    /// Missing table and empty build both yield `Ok(vec![])` / `Ok(0)` so the
    /// caller falls back to the brute-force scan rather than erroring.
    #[tokio::test]
    async fn missing_and_empty_fall_back_to_empty() {
        let dir = tempdir().unwrap();
        let corpus = dir.path();

        // No table built yet.
        assert!(search_raptor_summaries(corpus, &emb(1), 8)
            .await
            .unwrap()
            .is_empty());

        // Empty rows → nothing written, no sidecar, still no table.
        assert_eq!(build_raptor_index(corpus, &[], 1).await.unwrap(), 0);
        assert!(read_raptor_meta(corpus).is_none());
        assert!(search_raptor_summaries(corpus, &emb(1), 8)
            .await
            .unwrap()
            .is_empty());
    }

    /// Rebuild is idempotent: it replaces the row set and re-stamps the version.
    #[tokio::test]
    async fn rebuild_replaces_and_restamps() {
        let dir = tempdir().unwrap();
        let corpus = dir.path();
        build_raptor_index(corpus, &rows(10), 100).await.unwrap();
        let written = build_raptor_index(corpus, &rows(25), 200).await.unwrap();
        assert_eq!(written, 25);
        let meta = read_raptor_meta(corpus).unwrap();
        assert_eq!(meta.row_count, 25);
        assert_eq!(meta.source_version, 200);
        let hits = search_raptor_summaries(corpus, &emb(24), 4).await.unwrap();
        assert_eq!(hits[0].node_id, "n24");
    }
}
