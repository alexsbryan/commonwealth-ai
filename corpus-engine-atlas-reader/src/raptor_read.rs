// SPDX-License-Identifier: AGPL-3.0-or-later
//! READ half of the derived `raptor_summaries.lance` table.
//!
//! Carved out of corpus-engine's `index::raptor` (FIVE_PROGRAMS §12 decision
//! 1, 2026-09-21): the ground walk's whole-work summaries read this table
//! through `ground/summaries`, so the reads move with the walk. It takes a
//! PATH — no engine handle, no daemon — and the write half (the index build)
//! stays in corpus-engine, which re-imports these names at their historical
//! paths (ARCH §10.6 — a re-export, never a twin).

use std::path::{Path, PathBuf};

use arrow_array::{Array, FixedSizeListArray, Float32Array, Int32Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use corpus_index::error::{Error, Result};

/// The Lance table name inside the connected corpus dir.
pub const RAPTOR_TABLE: &str = "raptor_summaries";
pub const RAPTOR_LANCE_DIR: &str = "raptor_summaries.lance";
pub const RAPTOR_META_FILE: &str = "raptor_summaries.meta.json";

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

pub fn meta_path(corpus_dir: &Path) -> PathBuf {
    corpus_dir.join(RAPTOR_META_FILE)
}

pub fn lance_path(corpus_dir: &Path) -> PathBuf {
    corpus_dir.join(RAPTOR_LANCE_DIR)
}

/// Read the freshness sidecar, if present and parseable. `None` when the
/// table has never been built or the sidecar is missing/corrupt — the caller
/// treats that as "no index, use the scan."
pub fn read_raptor_meta(corpus_dir: &Path) -> Option<RaptorIndexMeta> {
    let s = std::fs::read_to_string(meta_path(corpus_dir)).ok()?;
    serde_json::from_str(&s).ok()
}

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
/// (the retired injector over-fetched `fetch_m = top_m * K`, then filtered
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
