// SPDX-License-Identifier: AGPL-3.0-or-later
//! What makes two chunk rows one row, and the passes that dedupe by it.
//!
//! A row's text hash alone is not its identity: two documents can say the
//! same words ("closed", "+1", a shared disclaimer) and each must keep its
//! row, or the document with the later row is gone from the index. The
//! duplicate these passes exist for is the same text of the SAME document,
//! written twice by a resume that rewound its cursor. uv-support lost 369 of
//! 2,954 documents (361 timeline events, 8 comments) to a text-only key.

use std::collections::{HashMap, HashSet};

use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use tracing::info;

use crate::error::{Error, Result};

use super::{CorpusIndex, DedupeReport};

/// One row's identity for deduplication: its document and its text hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChunkKey {
    pub source_doc_id: Option<String>,
    pub content_hash: String,
}

impl ChunkKey {
    pub fn new(source_doc_id: Option<&str>, content_hash: &str) -> Self {
        Self {
            source_doc_id: source_doc_id.map(str::to_string),
            content_hash: content_hash.to_string(),
        }
    }
}

/// The two columns a [`ChunkKey`] reads, downcast once per batch.
pub struct ChunkKeyColumns<'a> {
    hash: &'a StringArray,
    doc: Option<&'a StringArray>,
}

impl<'a> ChunkKeyColumns<'a> {
    /// `None` when the batch has no `content_hash` column.
    pub fn of(batch: &'a RecordBatch) -> Option<Self> {
        let string = |name| {
            batch
                .column_by_name(name)
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        };
        Some(Self {
            hash: string("content_hash")?,
            doc: string("source_doc_id"),
        })
    }

    /// The row's key, or `None` for a hashless (legacy) row.
    pub fn key(&self, row: usize) -> Option<ChunkKey> {
        if self.hash.is_null(row) {
            return None;
        }
        let doc = self.doc.filter(|d| !d.is_null(row)).map(|d| d.value(row));
        Some(ChunkKey::new(doc, self.hash.value(row)))
    }
}

impl CorpusIndex {
    async fn scan_key_columns(&self, extra: &[&str]) -> Result<Vec<RecordBatch>> {
        let mut cols = vec!["content_hash".to_string(), "source_doc_id".to_string()];
        cols.extend(extra.iter().map(|c| c.to_string()));
        self.table
            .query()
            .select(Select::Columns(cols))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("chunk key scan query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("chunk key scan collect: {e}")))
    }

    /// Count distinct chunk keys: `(distinct, with_hash, total_chunks)`.
    /// `with_hash - distinct` is how many rows repeat a document's own
    /// text, the duplicates a rewound resume writes; `total_chunks -
    /// with_hash` the hashless legacy rows. Materializes every key, so
    /// only an opt-in diagnostic runs it.
    pub async fn count_distinct_chunk_keys(&self) -> Result<(u64, u64, u64)> {
        let total_chunks = self.chunk_count().await?;
        let mut distinct = HashSet::new();
        let mut with_hash: u64 = 0;
        for batch in &self.scan_key_columns(&[]).await? {
            let Some(cols) = ChunkKeyColumns::of(batch) else {
                continue;
            };
            for key in (0..batch.num_rows()).filter_map(|i| cols.key(i)) {
                with_hash += 1;
                distinct.insert(key);
            }
        }
        Ok((distinct.len() as u64, with_hash, total_chunks))
    }

    /// The keys already in the index: the seen-set the ingest dedup gate
    /// and the canonical append start from, so a resumed run skips what
    /// it already wrote and nothing another document wrote.
    pub async fn list_indexed_chunk_keys(&self) -> Result<HashSet<ChunkKey>> {
        let mut out = HashSet::new();
        for batch in &self.scan_key_columns(&[]).await? {
            let cols = ChunkKeyColumns::of(batch)
                .ok_or_else(|| Error::Serialization("missing content_hash column".into()))?;
            out.extend((0..batch.num_rows()).filter_map(|i| cols.key(i)));
        }
        Ok(out)
    }

    /// Collapse rows that share a [`ChunkKey`], keeping the smallest `id`:
    /// the rescue pass for the resume-cursor-rewind bug, which landed up
    /// to 65% duplicate chunks in the wild, without re-embedding anything.
    /// Hashless rows are kept (no signal to dedupe them). Lance keeps the
    /// vector and FTS indexes valid across the deletes; run
    /// `build_indexes()` after if they were never built. Deletes go out in
    /// batches of `DELETE_BATCH` ids to bound the predicate string.
    pub async fn dedupe_chunk_rows(&self) -> Result<DedupeReport> {
        const DELETE_BATCH: usize = 10_000;
        let rows_before = self.chunk_count().await?;
        let mut winners: HashMap<ChunkKey, i64> = HashMap::new();
        let mut victims: Vec<i64> = Vec::new();
        let mut hashless: u64 = 0;
        for batch in &self.scan_key_columns(&["id"]).await? {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let cols = ChunkKeyColumns::of(batch)
                .ok_or_else(|| Error::Serialization("missing content_hash column".into()))?;
            for i in 0..batch.num_rows() {
                let id = ids.value(i);
                let Some(key) = cols.key(i) else {
                    hashless += 1;
                    continue;
                };
                match winners.get(&key) {
                    Some(&existing) if existing <= id => victims.push(id),
                    _ => {
                        if let Some(prior) = winners.insert(key, id) {
                            victims.push(prior);
                        }
                    }
                }
            }
        }
        let mut deleted: u64 = 0;
        for chunk in victims.chunks(DELETE_BATCH) {
            let id_list = chunk
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.table
                .delete(&format!("id IN ({id_list})"))
                .await
                .map_err(|e| Error::Database(format!("dedupe delete batch: {e}")))?;
            deleted += chunk.len() as u64;
        }
        let rows_after = if deleted == 0 {
            rows_before
        } else {
            self.chunk_count().await?
        };
        info!(
            rows_before,
            rows_after,
            deleted,
            kept = winners.len(),
            hashless,
            "corpus-index/dedupe: rows sharing a (document, text) key collapsed"
        );
        Ok(DedupeReport {
            rows_before,
            rows_after,
            duplicates_deleted: deleted,
            unique_keys_kept: winners.len() as u64,
            hashless_rows_preserved: hashless,
        })
    }
}
