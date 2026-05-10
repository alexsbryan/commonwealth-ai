//! Read helpers for the desktop reading surface.
//!
//! The glass-box reading UI clicks a citation and expects to see the
//! cited chunk plus its immediate textual neighbors. This module
//! provides [`CorpusIndex::neighbors`] — a small primitive that
//! returns `(prev, center, next)` rows by querying chunk ids in a
//! bounded range around the requested id, optionally filtered by
//! `source_doc_id` so that the "previous chunk" doesn't accidentally
//! belong to a different document in a multi-doc corpus.
//!
//! Ordering contract (v1): chunk `id` is monotonically increasing in
//! ingest order within a `source_doc_id`. Validated on
//! `brothers_karamazov` (single-doc literary) and the structured
//! Wikipedia / SEP extractors which chunk one source doc at a time
//! and write in order. Re-ingestion / resharding could in principle
//! perturb this; see ENRICHMENT_V2 deferred §"Layer-2 section
//! reading" for the path that supersedes id-ordering with explicit
//! section anchors once chunk metadata grows a queryable
//! `section_id` column.

use std::collections::{HashMap, HashSet};

use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use super::{CorpusIndex, EnrichmentChunkRow};
use crate::error::{Error, Result};

/// A center chunk plus its immediate textual neighbors.
#[derive(Debug, Clone)]
pub struct NeighborWindow {
    pub center: EnrichmentChunkRow,
    pub prev: Vec<EnrichmentChunkRow>,
    pub next: Vec<EnrichmentChunkRow>,
    /// Discriminator describing how neighbors were resolved. Future
    /// section-anchored ordering will set this to a different value
    /// so callers can branch on the contract in effect.
    pub ordering: &'static str,
}

impl CorpusIndex {
    /// Look up `chunk_id` and return it along with up to `radius`
    /// previous and next chunks, in source order.
    ///
    /// Neighbors are constrained to share the center's
    /// `source_doc_id` when present (to avoid bleeding across
    /// document boundaries in multi-doc corpora). When the center has
    /// no `source_doc_id`, neighbors are returned by id only — which
    /// is the right behavior for legacy single-doc indexes that never
    /// stamped a doc id.
    ///
    /// Returns `Ok(None)` when `chunk_id` is not present in the index.
    pub async fn neighbors(
        &self,
        chunk_id: u64,
        radius: usize,
    ) -> Result<Option<NeighborWindow>> {
        // Look up the center first via the existing chunks_by_ids
        // path so we get the same row shape the HTTP layer wants.
        let center_rows = self.chunks_by_ids(&[chunk_id]).await?;
        let Some(center) = center_rows.into_iter().next() else {
            return Ok(None);
        };

        if radius == 0 {
            return Ok(Some(NeighborWindow {
                center,
                prev: Vec::new(),
                next: Vec::new(),
                ordering: "id_within_source_doc",
            }));
        }

        // Scan a bounded id window on each side. SAFETY_FACTOR > 1
        // accommodates small gaps that can arise from re-ingestion or
        // dedup; for tightly-packed contiguous corpora the actual
        // results we keep will still be the immediate neighbors.
        const SAFETY_FACTOR: u64 = 4;
        let span = (radius as u64).saturating_mul(SAFETY_FACTOR).max(1);

        let center_id = center.id;
        let lower_min = center_id.saturating_sub(span);
        let upper_max = center_id.saturating_add(span);

        let mut prev_predicate = format!(
            "id >= {lower_min} AND id < {center_id}"
        );
        let mut next_predicate = format!(
            "id > {center_id} AND id <= {upper_max}"
        );
        if let Some(sd) = &center.source_doc_id {
            // Escape single quotes — same defense as
            // fetch_chunks_by_title.
            let safe = sd.replace('\'', "''");
            prev_predicate.push_str(&format!(" AND source_doc_id = '{safe}'"));
            next_predicate.push_str(&format!(" AND source_doc_id = '{safe}'"));
        }

        let prev_rows = self.scan_rows(&prev_predicate).await?;
        let next_rows = self.scan_rows(&next_predicate).await?;

        let mut prev_sorted = prev_rows;
        prev_sorted.sort_by_key(|r| std::cmp::Reverse(r.id));
        prev_sorted.truncate(radius);
        // Reverse back so callers receive prev in ascending order
        // (oldest → newest), matching natural reading order.
        prev_sorted.reverse();

        let mut next_sorted = next_rows;
        next_sorted.sort_by_key(|r| r.id);
        next_sorted.truncate(radius);

        Ok(Some(NeighborWindow {
            center,
            prev: prev_sorted,
            next: next_sorted,
            ordering: "id_within_source_doc",
        }))
    }

    /// Resolve a set of `section_id`s (as written into chunk
    /// `metadata.section_id` by the sectioned chunker) to a
    /// representative chunk per section.
    ///
    /// Returns a map `section_id → first_chunk_id_in_section`, with
    /// "first" defined as ascending chunk id within the section.
    /// Sections with no matching chunk in the index are simply
    /// absent from the map.
    ///
    /// Used by the desktop reading surface's atom panel: an atom's
    /// evidence carries section_ids; this projects those to
    /// chunk_ids so the user can click "appears in section X" and
    /// land on a real chunk.
    ///
    /// Implementation note: scans `all_chunks_full` once and
    /// filters in Rust. Acceptable for sectioned literary /
    /// philosophy corpora (BK, SEP) which fit comfortably in
    /// memory; large multi-million-row corpora will need a
    /// dedicated `metadata.section_id` index pushed down to
    /// LanceDB. Callers should treat this as O(corpus_size).
    pub async fn resolve_sections_to_chunks(
        &self,
        section_ids: &[String],
    ) -> Result<HashMap<String, u64>> {
        if section_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let wanted: HashSet<&str> = section_ids.iter().map(String::as_str).collect();
        let all = self.all_chunks_full().await?;
        let mut by_section: HashMap<String, u64> = HashMap::new();
        for row in all {
            let Some(meta_raw) = row.metadata_raw.as_deref() else { continue };
            let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_raw) else {
                continue;
            };
            let Some(section_id) = meta
                .as_object()
                .and_then(|obj| obj.get("section_id"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            if !wanted.contains(section_id) {
                continue;
            }
            // First chunk wins (lowest id within the section).
            by_section
                .entry(section_id.to_string())
                .and_modify(|existing| {
                    if row.id < *existing {
                        *existing = row.id;
                    }
                })
                .or_insert(row.id);
        }
        Ok(by_section)
    }

    /// Internal: run a scalar predicate scan and rehydrate
    /// `EnrichmentChunkRow` from the columns the reading surface
    /// needs. Mirrors the column set in
    /// [`CorpusIndex::chunks_by_ids`] so the two functions return
    /// rows with the same shape.
    async fn scan_rows(
        &self,
        predicate: &str,
    ) -> Result<Vec<EnrichmentChunkRow>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(predicate.to_string())
            .select(Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
                "title".to_string(),
                "url".to_string(),
                "metadata".to_string(),
                "source_doc_id".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("neighbors scan: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("neighbors collect: {e}")))?;

        let mut out: Vec<EnrichmentChunkRow> = Vec::new();
        for batch in &batches {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing content column".into()))?;
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let urls = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadatas = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let source_doc_ids = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                out.push(EnrichmentChunkRow {
                    id: id_col.value(i) as u64,
                    content: contents.value(i).to_string(),
                    title: titles.and_then(|t| {
                        if t.is_null(i) { None } else { Some(t.value(i).to_string()) }
                    }),
                    url: urls.and_then(|u| {
                        if u.is_null(i) { None } else { Some(u.value(i).to_string()) }
                    }),
                    metadata_raw: metadatas.and_then(|m| {
                        if m.is_null(i) { None } else { Some(m.value(i).to_string()) }
                    }),
                    source_doc_id: source_doc_ids.and_then(|s| {
                        if s.is_null(i) { None } else { Some(s.value(i).to_string()) }
                    }),
                });
            }
        }
        Ok(out)
    }

    /// Folder-ingest v1 §3.7: small read used by the per-document
    /// inspector. Returns `(chunk_count, first_chunk_preview)` for
    /// a given `source_doc_id`. The preview is truncated to
    /// `preview_chars` so the wire payload stays small even on
    /// large corpora — the UI paginates if the user wants more.
    ///
    /// Returns `Ok((0, None))` when no chunks exist for `doc_id`,
    /// which is the right answer for a freshly-registered watched
    /// folder (the first sweep hasn't ingested the file yet) and
    /// for files that failed extraction.
    pub async fn doc_summary(
        &self,
        doc_id: &str,
        preview_chars: usize,
    ) -> Result<(usize, Option<String>)> {
        let safe_id = doc_id.replace('\'', "''");
        let filter = format!("source_doc_id = '{safe_id}'");
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("doc_summary query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("doc_summary collect: {e}")))?;
        let mut count = 0usize;
        let mut min_id: Option<i64> = None;
        let mut min_content: Option<String> = None;
        for batch in &batches {
            count += batch.num_rows();
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let (Some(ids), Some(contents)) = (ids, contents) else {
                continue;
            };
            for i in 0..batch.num_rows() {
                let id = ids.value(i);
                if min_id.map_or(true, |m| id < m) {
                    min_id = Some(id);
                    let content = if contents.is_null(i) {
                        String::new()
                    } else {
                        contents.value(i).to_string()
                    };
                    let preview: String = content
                        .chars()
                        .take(preview_chars.max(1))
                        .collect();
                    min_content = Some(preview);
                }
            }
        }
        Ok((count, min_content))
    }
}
