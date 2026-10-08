// SPDX-License-Identifier: AGPL-3.0-or-later
//! Write operations — insert, delete, re-embed, and rebuild.

use std::path::Path;
use std::sync::Arc;

use arrow_array::{
    types::Float32Type, Array, FixedSizeListArray, Int32Array, Int64Array, RecordBatch, StringArray,
};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::error::{Error, Result};

use super::{
    corpus_schema, now_unix, read_meta, write_meta, CorpusIndex, EmbeddedChunk, InsertChunk,
    StoredChunk,
};

impl CorpusIndex {
    /// Maximum `id` currently stored (0 when empty). A one-time full scan
    /// of the `id` column — used only to SEED the allocation high-water
    /// for legacy indexes that pre-date `IndexMeta::next_chunk_id`. Not
    /// on any query hot path.
    pub async fn max_chunk_id(&self) -> Result<u64> {
        use lancedb::query::Select;
        let batches: Vec<RecordBatch> = self
            .table()
            .query()
            .select(Select::Columns(vec!["id".into()]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("max_chunk_id query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("max_chunk_id collect: {e}")))?;
        let mut max_id = 0u64;
        for batch in &batches {
            if let Some(ids) = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            {
                for i in 0..ids.len() {
                    max_id = max_id.max(ids.value(i).max(0) as u64);
                }
            }
        }
        Ok(max_id)
    }

    /// Reserve `n` contiguous chunk ids and return the first. Allocates
    /// from the persisted `next_chunk_id` high-water mark (seeded lazily
    /// from `max(id) + 1` for legacy indexes), so allocated ids are
    /// unique and strictly monotonic — never reused after a delete,
    /// dedupe, or delta append. This is the fix for the duplicate-id
    /// citation corruption; see `IndexMeta::next_chunk_id`.
    ///
    /// The high-water is persisted to `_corpus_meta.json`. If the meta is
    /// absent (rare — every created index writes it), we fall back to a
    /// fresh `max(id)` scan each call: still correct (committed rows are
    /// visible to the scan), just without the O(1) cache.
    async fn allocate_chunk_ids(&self, n: u64) -> Result<u64> {
        let index_dir = std::path::Path::new(self.connection().uri());
        let mut meta = read_meta(index_dir).ok();
        let first = match meta.as_ref().and_then(|m| m.next_chunk_id) {
            Some(hw) if hw > 0 => hw,
            // Legacy / missing high-water: seed from the actual max id so
            // we never collide with rows already on disk.
            _ => self.max_chunk_id().await?.saturating_add(1),
        };
        if let Some(m) = meta.as_mut() {
            m.next_chunk_id = Some(first + n);
            // Best-effort persist: even if this fails, correctness holds
            // because the next `max(id)` seed will see these rows.
            if let Err(e) = write_meta(index_dir, m) {
                tracing::warn!(error = %e, "allocate_chunk_ids: failed to persist next_chunk_id high-water");
            }
        }
        Ok(first)
    }

    /// Insert a batch of chunks (with pre-computed embeddings).
    pub async fn insert_batch(&self, chunks: &[(InsertChunk, Vec<f32>)]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        // Row count changes — drop the cached search gate (see gate_cache).
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = None;
        }

        // Allocate ids from the persisted high-water mark, NOT the row
        // count. `chunk_count()` diverges from the max id after any
        // delete/dedupe/delta, so count-based ids silently REUSE existing
        // ids → ambiguous `neighbors(id)` → wrong-chunk citations. See
        // `IndexMeta::next_chunk_id`.
        let first_id = self.allocate_chunk_ids(chunks.len() as u64).await?;

        let ids: Vec<i64> = (0..chunks.len())
            .map(|i| (first_id + i as u64) as i64)
            .collect();
        let contents: Vec<&str> = chunks.iter().map(|(c, _)| c.content.as_str()).collect();
        let titles: Vec<Option<&str>> = chunks.iter().map(|(c, _)| c.title.as_deref()).collect();
        let urls: Vec<Option<&str>> = chunks.iter().map(|(c, _)| c.url.as_deref()).collect();
        let metadatas: Vec<Option<&str>> =
            chunks.iter().map(|(c, _)| c.metadata.as_deref()).collect();
        let content_hashes: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.content_hash.as_deref())
            .collect();
        let source_doc_ids: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.source_doc_id.as_deref())
            .collect();

        // Code-intelligence columns. Non-code chunks leave every field
        // None → stored as Null. No JSON parsing at query time.
        let symbol_names: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.code.symbol_name.as_deref())
            .collect();
        let symbol_kinds: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.code.symbol_kind.as_deref())
            .collect();
        let file_paths: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.code.file_path.as_deref())
            .collect();
        let line_starts: Vec<Option<i32>> = chunks.iter().map(|(c, _)| c.code.line_start).collect();
        let line_ends: Vec<Option<i32>> = chunks.iter().map(|(c, _)| c.code.line_end).collect();
        let languages: Vec<Option<&str>> = chunks
            .iter()
            .map(|(c, _)| c.code.language.as_deref())
            .collect();
        let mtimes: Vec<Option<i64>> = chunks.iter().map(|(c, _)| c.code.mtime).collect();

        // Pull-based work queue column: `u32` cast to `i32` (Arrow has no
        // unsigned 32-bit). Unit IDs are small indices into the coordinator's
        // queue, so the cast is lossless in practice.
        let unit_ids: Vec<Option<i32>> = chunks
            .iter()
            .map(|(c, _)| c.unit_id.map(|u| u as i32))
            .collect();

        let text_sha256s: Vec<Option<String>> = chunks
            .iter()
            .map(|(c, _)| c.text_sha256.map(|h| h.to_hex()))
            .collect();

        // Build the embedding FixedSizeList array.
        let dim = self.embedding_dimensions as i32;
        let embedding_array = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            chunks.iter().map(|(_, e)| Some(e.iter().map(|&v| Some(v)))),
            dim,
        );

        let schema = corpus_schema(self.embedding_dimensions);
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(contents)),
                Arc::new(StringArray::from(titles)),
                Arc::new(StringArray::from(urls)),
                Arc::new(embedding_array),
                Arc::new(StringArray::from(metadatas)),
                Arc::new(StringArray::from(content_hashes)),
                Arc::new(StringArray::from(source_doc_ids)),
                Arc::new(StringArray::from(symbol_names)),
                Arc::new(StringArray::from(symbol_kinds)),
                Arc::new(StringArray::from(file_paths)),
                Arc::new(Int32Array::from(line_starts)),
                Arc::new(Int32Array::from(line_ends)),
                Arc::new(StringArray::from(languages)),
                Arc::new(Int64Array::from(mtimes)),
                Arc::new(Int32Array::from(unit_ids)),
                Arc::new(StringArray::from(text_sha256s)),
            ],
        )
        .map_err(|e| Error::Serialization(format!("record batch: {e}")))?;

        self.table
            .add(vec![batch])
            .execute()
            .await
            .map_err(|e| Error::Database(e.to_string()))?;

        // Update last_updated in metadata.
        let index_dir = Path::new(self.db.uri());
        if let Ok(mut meta) = read_meta(index_dir) {
            meta.last_updated = now_unix();
            let _ = write_meta(index_dir, &meta);
        }

        Ok(())
    }

    /// Insert pre-embedded chunks into the index.
    pub async fn insert_chunks(&self, chunks: &[EmbeddedChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let pairs: Vec<(InsertChunk, Vec<f32>)> = chunks
            .iter()
            .map(|c| (c.insert.clone(), c.embedding.clone()))
            .collect();
        self.insert_batch(&pairs).await
    }

    /// Delete all chunks whose `source_doc_id` matches `doc_id`.
    pub async fn delete_chunks_by_source_doc(&self, doc_id: &str) -> Result<()> {
        // Escape single quotes to prevent filter injection.
        let safe_id = doc_id.replace('\'', "''");
        self.table
            .delete(&format!("source_doc_id = '{safe_id}'"))
            .await
            .map_err(|e| Error::Database(format!("delete_chunks_by_source_doc: {e}")))?;
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = None;
        }
        Ok(())
    }

    /// Delete chunks whose row id is in `ids`. Move 6 P6 pairs this
    /// with `chunk_delta` so the watcher's reindex hot path drops
    /// only the chunks whose content actually changed, instead of
    /// nuking and re-embedding the whole file.
    ///
    /// Empty `ids` is a no-op (no database round-trip).
    pub async fn delete_chunks_by_ids(&self, ids: &[u64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let list: String = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        self.table
            .delete(&format!("id IN ({list})"))
            .await
            .map_err(|e| Error::Database(format!("delete_chunks_by_ids: {e}")))?;
        if let Ok(mut g) = self.gate_cache.lock() {
            *g = None;
        }
        Ok(())
    }

    /// Compute the stable content fingerprint for this index.
    ///
    /// Algorithm: walk every row's `content_hash`, collect into a
    /// sorted `Vec<&str>`, BLAKE3 over the sorted lines (each
    /// terminated with `\n`), return hex.
    ///
    /// Properties:
    /// - **Deterministic across nodes.** Two nodes that have the
    ///   same set of `content_hash` values produce byte-identical
    ///   fingerprints regardless of insertion order or row layout.
    /// - **Cheap-to-recompute.** One full table scan for
    ///   `content_hash` (already optimised in LanceDB) + a sort +
    ///   one BLAKE3 stream. ~5-10s for a Wikipedia-scale 2.7M-chunk
    ///   index; trivial for smaller corpora.
    /// - **Fails closed.** Rows missing `content_hash` (legacy
    ///   corpora pre-dating that column being populated) are
    ///   skipped silently and the resulting fingerprint *only
    ///   reflects the hashed subset*. We log a warning so callers
    ///   can spot the case; the alternative — returning an error —
    ///   would block canonical sync for any corpus that ever held
    ///   an unhashed row.
    ///
    /// Used by:
    /// - `merge_partitions_into_canonical` to stamp a new canonical
    ///   on completion.
    /// - The auto-recover path before pulling from a peer (verifies
    ///   the local canonical is byte-identical to what gossip
    ///   advertised before deciding it's worth pulling a fresh
    ///   copy).
    /// - `sovereign corpus diag` to surface fingerprint mismatch
    ///   between local and gossip-advertised values.
    pub async fn compute_canonical_fingerprint(&self) -> Result<String> {
        let batches: Vec<arrow_array::RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "content_hash".to_string()
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("compute_canonical_fingerprint query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("compute_canonical_fingerprint collect: {e}")))?;

        let mut hashes: Vec<String> = Vec::new();
        let mut hashless_rows: u64 = 0;
        for batch in &batches {
            let arr = batch
                .column_by_name("content_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing content_hash column".into()))?;
            for i in 0..batch.num_rows() {
                if arr.is_null(i) {
                    hashless_rows += 1;
                } else {
                    hashes.push(arr.value(i).to_string());
                }
            }
        }

        if hashless_rows > 0 {
            tracing::warn!(
                hashless_rows,
                hashed_rows = hashes.len(),
                "compute_canonical_fingerprint: skipping rows without content_hash; \
                 fingerprint reflects only hashed subset"
            );
        }

        // Sort lexicographically so insertion-order doesn't change
        // the fingerprint. dedup() afterwards guards against the
        // same content_hash appearing twice (should not happen on a
        // properly-deduped canonical, but cheap defence).
        hashes.sort();
        hashes.dedup();

        let mut hasher = blake3::Hasher::new();
        for h in &hashes {
            hasher.update(h.as_bytes());
            hasher.update(b"\n");
        }
        Ok(hasher.finalize().to_hex().to_string())
    }

    /// Persist a precomputed `canonical_fingerprint` into the
    /// on-disk meta. Idempotent — safe to call repeatedly with the
    /// same value, or to update with a recomputed value after the
    /// chunk set legitimately changed.
    pub fn set_canonical_fingerprint(&self, fingerprint: &str) -> Result<()> {
        let index_dir = std::path::Path::new(self.connection().uri());
        let mut meta = super::read_meta(index_dir)?;
        meta.canonical_fingerprint = Some(fingerprint.to_string());
        meta.last_updated = super::now_unix();
        super::write_meta(index_dir, &meta)
    }

    /// Convenience: compute the fingerprint and stamp it. Used by
    /// the canonical-finalize hook (post-merge, post-build_indexes)
    /// and by lazy stamping of legacy canonicals on the daemon's
    /// next-info read.
    pub async fn compute_and_stamp_fingerprint(&self) -> Result<String> {
        let fp = self.compute_canonical_fingerprint().await?;
        self.set_canonical_fingerprint(&fp)?;
        Ok(fp)
    }

    /// Return the set of distinct `source_doc_id` values currently in
    /// the index.
    ///
    /// Used by `expand_corpus` to identify already-indexed documents so
    /// the expansion run can skip them — the cheap delta path that
    /// avoids walking through `ManifestDiff`. For Wikipedia Core
    /// (~150K accepted articles, ~5M chunks) this returns ~150K
    /// strings (~5–10 MB) and runs in seconds; the cost is amortised
    /// over the multi-minute expansion.
    pub async fn list_indexed_source_doc_ids(&self) -> Result<std::collections::HashSet<String>> {
        use futures::TryStreamExt;
        let batches: Vec<arrow_array::RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "source_doc_id".to_string()
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("list_indexed_source_doc_ids query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("list_indexed_source_doc_ids collect: {e}")))?;

        let mut out = std::collections::HashSet::new();
        for batch in &batches {
            let arr = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing source_doc_id column".into()))?;
            for i in 0..batch.num_rows() {
                if !arr.is_null(i) {
                    out.insert(arr.value(i).to_string());
                }
            }
        }
        Ok(out)
    }

    /// Load specific chunks by their IDs.
    /// Re-acquire stored chunks by row id, as `ScoredChunk`s stamped by this
    /// index.
    ///
    /// The door for "I have a chunk id and I need the passage back" — the
    /// atlas-grounding path had been calling [`Self::get_chunks`] and
    /// assembling a `ScoredChunk` itself, which meant real index content
    /// entered the pool with no provenance at all (TOPOLOGY §10 rung 9.1,
    /// hazard 1).
    ///
    /// Custody is [`Custody::Unknown`], and that is a statement rather than a
    /// gap: [`StoredChunk`] carries no metadata column, so custody is not a
    /// fact this door can read. Unknown REFUSES downstream, which is the
    /// correct posture — the alternative is a fabricated class (ARCH §18.3).
    /// `score` is left at 0.0 for the caller to rank.
    pub async fn acquire_chunks(
        &self,
        chunk_ids: &[u64],
    ) -> Result<Vec<crate::types::ScoredChunk>> {
        let stored = self.get_chunks(chunk_ids).await?;
        Ok(stored
            .into_iter()
            .map(|s| crate::types::ScoredChunk {
                content: s.content,
                title: s.title,
                url: None,
                corpus_id: self.corpus_id().to_string(),
                score: 0.0,
                metadata: std::collections::HashMap::new(),
                chunk_id: Some(s.id),
                source_doc_id: None,
                vector_distance: None,
                provenance: crate::index::ChunkProvenance::Acquired(
                    crate::index::Acquisition::stamped(
                        self.corpus_id(),
                        kernel_types::Custody::Unknown,
                        kernel_types::Grain::Leaf,
                    ),
                ),
            })
            .collect())
    }

    pub async fn get_chunks(&self, chunk_ids: &[u64]) -> Result<Vec<StoredChunk>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        // Build a filter expression: id IN (1, 2, 3, ...)
        let id_list = chunk_ids
            .iter()
            .map(|id| format!("{}", *id as i64))
            .collect::<Vec<_>>()
            .join(", ");
        let filter = format!("id IN ({id_list})");

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
                "title".to_string(),
                "source_doc_id".to_string(),
            ]))
            .only_if(filter)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("get_chunks query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("get_chunks collect: {e}")))?;

        let mut out = Vec::new();
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
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing title column".into()))?;
            let doc_ids = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .ok_or_else(|| Error::Serialization("missing source_doc_id column".into()))?;

            for i in 0..batch.num_rows() {
                out.push(StoredChunk {
                    id: ids.value(i) as u64,
                    content: contents.value(i).to_string(),
                    title: if titles.is_null(i) {
                        None
                    } else {
                        Some(titles.value(i).to_string())
                    },
                    source_doc_id: if doc_ids.is_null(i) {
                        None
                    } else {
                        Some(doc_ids.value(i).to_string())
                    },
                });
            }
        }
        Ok(out)
    }

    /// Re-embed the specified chunks with a fresh embedding call and update them in place.
    pub async fn re_embed_chunks(
        &self,
        chunk_ids: &[u64],
        _embed_fn: &crate::types::EmbedFn,
    ) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        // Fetch content for the given chunk IDs.
        let id_filter = chunk_ids
            .iter()
            .map(|id| format!("id = {id}"))
            .collect::<Vec<_>>()
            .join(" OR ");

        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(id_filter)
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("re_embed fetch: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("re_embed collect: {e}")))?;

        for batch in &batches {
            // Per-chunk re-embed is unsupported: re-inserting a row needs every
            // column, which a content-only fetch doesn't carry. Error cleanly if
            // any target rows exist — WITHOUT the previous impl's destructive
            // delete-then-bail (it dropped the first matched row before erroring,
            // which clippy flagged via `never_loop`).
            if batch.num_rows() > 0 {
                return Err(Error::Extraction(
                    "Per-chunk re-embed requires full row data; use schedule_enrichment_full instead".into(),
                ));
            }
        }
        Ok(())
    }

    /// Rebuild both FTS indexes (content + title) from current data.
    /// This drops the existing indexes and recreates them.
    pub async fn rebuild_fts(&self) -> Result<()> {
        // Clear sub-phase flags so build_indexes() actually rebuilds the FTS indexes.
        let dir = Path::new(self.db.uri());
        if let Ok(mut meta) = read_meta(dir) {
            meta.content_fts_built = false;
            meta.title_fts_built = false;
            let _ = write_meta(dir, &meta);
        }
        self.build_indexes(false, true, None).await
    }

    /// Rebuild the IVF-PQ vector index from current data.
    ///
    /// Called by [`crate::CorpusEngine::expand_corpus`] after a
    /// scope-relaxing expansion adds millions of new vectors. The IVF
    /// centroids trained at the original (smaller) scale become
    /// suboptimal at the new scale; rebuilding picks fresh
    /// `optimal_partitions(num_chunks)` centroids and re-trains.
    ///
    /// LanceDB tolerates concurrent reads while the index is being
    /// rebuilt — search continues to work, just falls back to a flat
    /// scan over recently-added vectors until the new index is
    /// committed.
    pub async fn rebuild_vector_index(&self) -> Result<()> {
        let dir = Path::new(self.db.uri());
        if let Ok(mut meta) = read_meta(dir) {
            meta.vector_index_built = false;
            let _ = write_meta(dir, &meta);
        }
        self.build_indexes(true, false, None).await
    }
}
