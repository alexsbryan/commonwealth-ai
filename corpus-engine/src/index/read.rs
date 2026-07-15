// SPDX-License-Identifier: AGPL-3.0-or-later
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

use arrow_array::{
    Array, FixedSizeListArray, Float32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
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
    pub async fn neighbors(&self, chunk_id: u64, radius: usize) -> Result<Option<NeighborWindow>> {
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

        let mut prev_predicate = format!("id >= {lower_min} AND id < {center_id}");
        let mut next_predicate = format!("id > {center_id} AND id <= {upper_max}");
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
        self.scan_section_chunk_map(Some(&wanted)).await
    }

    /// Full `section_id → first chunk_id` map for the whole corpus, via
    /// one projected `(id, metadata)` scan. The desktop atom-detail path
    /// builds this ONCE per corpus and caches it, then resolves every
    /// evidence row from memory — instead of rescanning chunks.lance on
    /// every click (a 2.8 GB / ~90s scan on Wikipedia). O(corpus_size);
    /// call it off the interaction path (background warm).
    pub async fn section_chunk_index(&self) -> Result<HashMap<String, u64>> {
        self.scan_section_chunk_map(None).await
    }

    /// Shared projected scan for the two section→chunk resolvers.
    ///
    /// Projects ONLY (id, metadata) — never `content`. `section_id`
    /// lives inside the metadata JSON blob, so there's no scalar column
    /// to push a filter onto and we must scan; but pulling `content`
    /// too (as `all_chunks_full` did) reads the ENTIRE chunk table
    /// (4.4 GB on Wikipedia's 1.9M rows) to map a single section.
    /// With `wanted`, filters to those sections and early-exits once all
    /// are found; with `None`, returns the complete map.
    async fn scan_section_chunk_map(
        &self,
        wanted: Option<&HashSet<&str>>,
    ) -> Result<HashMap<String, u64>> {
        let mut stream = self
            .table
            .query()
            .select(Select::Columns(vec![
                "id".to_string(),
                "metadata".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("resolve_sections scan: {e}")))?;

        let mut by_section: HashMap<String, u64> = HashMap::new();
        'outer: while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|e| Error::Database(format!("resolve_sections collect: {e}")))?
        {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let Some(metadatas) = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            else {
                continue;
            };
            for i in 0..batch.num_rows() {
                if metadatas.is_null(i) {
                    continue;
                }
                let Ok(meta) =
                    serde_json::from_str::<serde_json::Value>(metadatas.value(i))
                else {
                    continue;
                };
                let Some(section_id) = meta
                    .as_object()
                    .and_then(|obj| obj.get("section_id"))
                    .and_then(|v| v.as_str())
                else {
                    continue;
                };
                if let Some(wanted) = wanted {
                    if !wanted.contains(section_id) {
                        continue;
                    }
                }
                let id = id_col.value(i) as u64;
                // First chunk wins (lowest id within the section).
                by_section
                    .entry(section_id.to_string())
                    .and_modify(|existing| {
                        if id < *existing {
                            *existing = id;
                        }
                    })
                    .or_insert(id);
                // Early-exit only when a specific set was requested.
                if let Some(wanted) = wanted {
                    if by_section.len() == wanted.len() {
                        break 'outer;
                    }
                }
            }
        }
        Ok(by_section)
    }

    /// Internal: run a scalar predicate scan and rehydrate
    /// `EnrichmentChunkRow` from the columns the reading surface
    /// needs. Mirrors the column set in
    /// [`CorpusIndex::chunks_by_ids`] so the two functions return
    /// rows with the same shape.
    async fn scan_rows(&self, predicate: &str) -> Result<Vec<EnrichmentChunkRow>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(predicate)
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
                        if t.is_null(i) {
                            None
                        } else {
                            Some(t.value(i).to_string())
                        }
                    }),
                    url: urls.and_then(|u| {
                        if u.is_null(i) {
                            None
                        } else {
                            Some(u.value(i).to_string())
                        }
                    }),
                    metadata_raw: metadatas.and_then(|m| {
                        if m.is_null(i) {
                            None
                        } else {
                            Some(m.value(i).to_string())
                        }
                    }),
                    source_doc_id: source_doc_ids.and_then(|s| {
                        if s.is_null(i) {
                            None
                        } else {
                            Some(s.value(i).to_string())
                        }
                    }),
                });
            }
        }
        Ok(out)
    }

    /// Move 6 P6: read `(id, content_hash)` for every chunk whose
    /// `source_doc_id == doc_id`. Pairs with
    /// [`crate::chunkers::chunk_delta`] so the watcher's
    /// `reindex_file` hot path can drop the nuke-and-re-embed cycle
    /// for files where most chunks survived a single-line edit.
    ///
    /// Returns rows in undefined order. Chunks with a NULL
    /// `content_hash` (legacy ingests before the column was
    /// populated) are skipped — they would never match the
    /// extractor's blake3 hash anyway and would force a full
    /// re-embed if we returned them with an empty hash.
    pub async fn committed_chunks_for_doc(
        &self,
        doc_id: &str,
    ) -> Result<Vec<crate::chunkers::CommittedChunk>> {
        let safe_id = doc_id.replace('\'', "''");
        let filter = format!("source_doc_id = '{safe_id}'");
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(Select::Columns(vec![
                "id".to_string(),
                "content_hash".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("committed_chunks_for_doc query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("committed_chunks_for_doc collect: {e}")))?;

        let mut out: Vec<crate::chunkers::CommittedChunk> = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let hashes = batch
                .column_by_name("content_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            for i in 0..batch.num_rows() {
                let Some(hashes) = hashes else { continue };
                if hashes.is_null(i) {
                    continue;
                }
                out.push(crate::chunkers::CommittedChunk {
                    id: ids.value(i) as u64,
                    content_hash: hashes.value(i).to_string(),
                });
            }
        }
        Ok(out)
    }

    /// Move 6 P5.a.1: fetch every chunk whose `source_doc_id` is in
    /// `doc_ids`. Used by the newsworthy incremental atlas path to
    /// re-extract atoms for just the articles touched by a tick
    /// instead of streaming the whole corpus.
    ///
    /// Returns rows in the same column shape as
    /// [`Self::chunks_by_ids`] so the aggregation helper in
    /// `structure_first` accepts either source. Empty `doc_ids`
    /// returns `Ok(vec![])` without a database round-trip.
    pub async fn chunks_by_source_doc_ids(
        &self,
        doc_ids: &[String],
    ) -> Result<Vec<EnrichmentChunkRow>> {
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }
        // LanceDB's `only_if` accepts a SQL fragment; build an
        // IN (...) list with single-quoted, escape-doubled values.
        let mut parts: Vec<String> = Vec::with_capacity(doc_ids.len());
        for id in doc_ids {
            parts.push(format!("'{}'", id.replace('\'', "''")));
        }
        let predicate = format!("source_doc_id IN ({})", parts.join(","));
        self.scan_rows(&predicate).await
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
                if min_id.is_none_or(|m| id < m) {
                    min_id = Some(id);
                    let content = if contents.is_null(i) {
                        String::new()
                    } else {
                        contents.value(i).to_string()
                    };
                    let preview: String = content.chars().take(preview_chars.max(1)).collect();
                    min_content = Some(preview);
                }
            }
        }
        Ok((count, min_content))
    }

    /// Conv-tiered port (spec `CONV_TIERED_PORT.md`): fetch every
    /// chunk for one `source_doc_id` (= conv_uuid) WITH its embedding
    /// vector, ready to hand to `build_raptor_atlas`. Pairs each
    /// returned row with its f32 embedding in lock-step order.
    ///
    /// Returns rows sorted by `id` ascending so callers building
    /// position-anchored RAPTOR clusters see the chunks in source
    /// order. Chunks with a missing/null embedding are dropped — they
    /// shouldn't exist on a healthy T1-complete index, but the
    /// fallback is "skip" rather than "fail the whole conv".
    pub async fn chunks_for_source_doc_with_embeddings(
        &self,
        doc_id: &str,
    ) -> Result<Vec<(EnrichmentChunkRow, Vec<f32>)>> {
        let safe_id = doc_id.replace('\'', "''");
        let predicate = format!("source_doc_id = '{safe_id}'");
        let mut out = self.select_chunks_with_embeddings(Some(predicate)).await?;
        out.sort_by_key(|(r, _)| r.id);
        Ok(out)
    }

    /// Every chunk in the corpus WITH its embedding, in ONE table scan.
    ///
    /// Callers that need all embeddings (e.g. the DRY / clone report) must NOT
    /// fan out into a per-`source_doc_id` query each: `source_doc_id` is not
    /// indexed, so each such query is a full table scan — thousands of them cost
    /// tens of seconds. This is a single pass. Order is Lance's scan order.
    pub async fn all_chunks_with_embeddings(
        &self,
    ) -> Result<Vec<(EnrichmentChunkRow, Vec<f32>)>> {
        self.select_chunks_with_embeddings(None).await
    }

    /// Shared query + Arrow-parse for the two `*_with_embeddings` readers above.
    /// `predicate` is an optional LanceDB `only_if` filter. Rows with a
    /// missing/null embedding are dropped (defensive — a T1-complete index has
    /// none). Returns rows in Lance's scan order; sort at the call site if the
    /// consumer needs id order.
    async fn select_chunks_with_embeddings(
        &self,
        predicate: Option<String>,
    ) -> Result<Vec<(EnrichmentChunkRow, Vec<f32>)>> {
        let mut query = self.table.query();
        if let Some(predicate) = predicate {
            query = query.only_if(predicate);
        }
        let batches: Vec<RecordBatch> = query
            .select(Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
                "title".to_string(),
                "url".to_string(),
                "metadata".to_string(),
                "source_doc_id".to_string(),
                "embedding".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("chunks_with_embeddings query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("chunks_with_embeddings collect: {e}")))?;

        let mut out: Vec<(EnrichmentChunkRow, Vec<f32>)> = Vec::new();
        for batch in &batches {
            let ids = batch
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
            let embedding_col = batch
                .column_by_name("embedding")
                .ok_or_else(|| Error::Serialization("missing embedding column".into()))?;

            // The embedding column ships as either a FixedSizeList
            // (Lance default for fixed-dim vectors) or a List depending
            // on the schema version this row was written under; handle
            // both shapes.
            let fixed = embedding_col.as_any().downcast_ref::<FixedSizeListArray>();
            let variable = embedding_col.as_any().downcast_ref::<ListArray>();

            for i in 0..batch.num_rows() {
                let embedding_vec: Option<Vec<f32>> = if let Some(fixed) = fixed {
                    if fixed.is_null(i) {
                        None
                    } else {
                        let inner = fixed.value(i);
                        inner
                            .as_any()
                            .downcast_ref::<Float32Array>()
                            .map(|a| a.values().to_vec())
                    }
                } else if let Some(variable) = variable {
                    if variable.is_null(i) {
                        None
                    } else {
                        let inner = variable.value(i);
                        inner
                            .as_any()
                            .downcast_ref::<Float32Array>()
                            .map(|a| a.values().to_vec())
                    }
                } else {
                    None
                };
                let Some(embedding) = embedding_vec else {
                    // Skip chunks lacking an embedding — T1-complete
                    // indexes shouldn't have these, but be defensive.
                    continue;
                };

                let row = EnrichmentChunkRow {
                    id: ids.value(i) as u64,
                    content: contents.value(i).to_string(),
                    title: titles.and_then(|t| {
                        if t.is_null(i) {
                            None
                        } else {
                            Some(t.value(i).to_string())
                        }
                    }),
                    url: urls.and_then(|u| {
                        if u.is_null(i) {
                            None
                        } else {
                            Some(u.value(i).to_string())
                        }
                    }),
                    metadata_raw: metadatas.and_then(|m| {
                        if m.is_null(i) {
                            None
                        } else {
                            Some(m.value(i).to_string())
                        }
                    }),
                    source_doc_id: source_doc_ids.and_then(|s| {
                        if s.is_null(i) {
                            None
                        } else {
                            Some(s.value(i).to_string())
                        }
                    }),
                };
                out.push((row, embedding));
            }
        }
        Ok(out)
    }

    /// Conv-tiered port (spec `CONV_TIERED_PORT.md`): scan the entire
    /// chunks index and return a `source_doc_id → chunk_id list` map.
    ///
    /// Used by `enrichment::tiered::run_tiered_enrichment` to partition
    /// the corpus into per-conversation work units before dispatching
    /// each conv through `build_raptor_atlas`. Rows with a NULL
    /// `source_doc_id` are dropped — for the conversation corpora this
    /// port targets, every row carries a `conv_uuid` in that column.
    ///
    /// Returns chunk ids in the order Lance returns them; callers that
    /// need sorted output should sort post-hoc.
    pub async fn group_chunks_by_source_doc(&self) -> Result<HashMap<String, Vec<u64>>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(Select::Columns(vec![
                "id".to_string(),
                "source_doc_id".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("group_chunks scan: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("group_chunks collect: {e}")))?;

        let mut out: HashMap<String, Vec<u64>> = HashMap::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let docs = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let Some(docs) = docs else { continue };
            for i in 0..batch.num_rows() {
                if docs.is_null(i) {
                    continue;
                }
                let key = docs.value(i).to_string();
                let chunk_id = ids.value(i) as u64;
                out.entry(key).or_default().push(chunk_id);
            }
        }
        Ok(out)
    }
}
