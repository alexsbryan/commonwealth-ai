//! Enrichment data access — embedding streams, bulk updates, field skeleton I/O.

use std::collections::HashMap;

use arrow_array::{
    Array, Float32Array, Int64Array, RecordBatch, StringArray,
    FixedSizeListArray,
};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::error::{Error, Result};

use super::{CorpusIndex, StoredChunk, StoredChunkWithMetadata};

impl CorpusIndex {
    /// Sample up to `n` chunk embeddings for integrity checking.
    /// Returns `(chunk_id, embedding)` pairs.
    pub async fn sample_embeddings(&self, n: usize) -> Result<Vec<(u64, Vec<f32>)>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "embedding".to_string(),
            ]))
            .limit(n)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("sample_embeddings query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("sample_embeddings collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let ids = match batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            {
                Some(a) => a,
                None => continue,
            };
            let embeddings = match batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
            {
                Some(a) => a,
                None => continue,
            };
            for i in 0..batch.num_rows() {
                let id = ids.value(i) as u64;
                let values = embeddings.value(i);
                let floats = values
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .map(|a| (0..a.len()).map(|j| a.value(j)).collect::<Vec<_>>())
                    .unwrap_or_default();
                out.push((id, floats));
            }
        }
        Ok(out)
    }

    /// Stream all chunk embeddings from the index.
    /// Returns `(chunk_ids, embeddings)` — columnar projection, does not
    /// load chunk text.
    pub async fn stream_embedding_column(&self) -> Result<(Vec<u64>, Vec<Vec<f32>>)> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "embedding".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("stream_embedding_column query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("stream_embedding_column collect: {e}")))?;

        let mut ids = Vec::new();
        let mut embeddings = Vec::new();

        for batch in &batches {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;

            let emb_col = batch
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>())
                .ok_or_else(|| Error::Serialization("missing embedding column".into()))?;

            for i in 0..batch.num_rows() {
                ids.push(id_col.value(i) as u64);

                let values = emb_col
                    .value(i)
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .map(|a| a.values().to_vec())
                    .unwrap_or_default();
                embeddings.push(values);
            }
        }

        Ok((ids, embeddings))
    }

    /// Read every chunk in the index. Used by the enrichment pipeline
    /// to feed claim extraction prompts.
    ///
    /// Materializes all chunks into memory. For very large corpora this
    /// is significant — but enrichment runs offline as a one-time job
    /// and the chunks are reasonably bounded (a few hundred bytes each
    /// of content + title; embeddings are not loaded here).
    pub async fn all_chunks(&self) -> Result<Vec<StoredChunk>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "content".to_string(),
                "title".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("all_chunks query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("all_chunks collect: {e}")))?;

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

            for i in 0..batch.num_rows() {
                out.push(StoredChunk {
                    id: ids.value(i) as u64,
                    content: contents.value(i).to_string(),
                    title: if titles.is_null(i) {
                        None
                    } else {
                        Some(titles.value(i).to_string())
                    },
                });
            }
        }
        Ok(out)
    }

    /// Like `all_chunks` but also returns the raw `metadata` JSON string and
    /// the URL, for use by the structural enrichment pipeline (link graph
    /// builder and article profile builder).
    pub async fn all_chunks_with_raw_metadata(&self) -> Result<Vec<StoredChunkWithMetadata>> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".to_string(),
                "title".to_string(),
                "url".to_string(),
                "metadata".to_string(),
            ]))
            .execute()
            .await
            .map_err(|e| Error::Database(format!("all_chunks_with_raw_metadata query: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("all_chunks_with_raw_metadata collect: {e}")))?;

        let mut out = Vec::new();
        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| Error::Serialization("missing id column".into()))?;
            let titles = batch
                .column_by_name("title")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let urls = batch
                .column_by_name("url")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let metadatas = batch
                .column_by_name("metadata")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            for i in 0..batch.num_rows() {
                out.push(StoredChunkWithMetadata {
                    id: ids.value(i) as u64,
                    title: titles.and_then(|t| {
                        if t.is_null(i) { None } else { Some(t.value(i).to_string()) }
                    }),
                    url: urls.and_then(|u| {
                        if u.is_null(i) { None } else { Some(u.value(i).to_string()) }
                    }),
                    metadata_raw: metadatas.and_then(|m| {
                        if m.is_null(i) { None } else { Some(m.value(i).to_string()) }
                    }),
                });
            }
        }
        Ok(out)
    }

    /// Bulk-update an Int32 column on the chunks table.
    /// Used by the clustering phase to write `cluster_id`.
    pub async fn bulk_update_i32_column(
        &self,
        _col_name: &str,
        _assignments: &HashMap<u64, i32>,
    ) -> Result<()> {
        // TODO: Implement via LanceDB merge or update API.
        // For now, this is a placeholder that will be filled in during
        // the CorpusIndex extension phase.
        Ok(())
    }

    /// Bulk-update a Utf8 column on the chunks table.
    /// Used by the labeling phase to write `chunk_role`.
    pub async fn bulk_update_str_column(
        &self,
        _col_name: &str,
        _assignments: &HashMap<u64, &str>,
    ) -> Result<()> {
        // TODO: Implement via LanceDB merge or update API.
        Ok(())
    }

    /// True if this index has field model tables (from the new enrichment pipeline).
    pub async fn has_field_model_tables(&self) -> bool {
        self.has_table("field_questions").await
    }

    /// Write a field skeleton JSON file to the index directory.
    pub fn write_field_skeleton(
        &self,
        skeleton: &crate::enrichment::skeleton::FieldSkeleton,
    ) -> Result<()> {
        let path = self.path().join("field_skeleton.json");
        let json = serde_json::to_string_pretty(skeleton)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load the field skeleton JSON file if it exists.
    pub fn load_field_skeleton(
        &self,
    ) -> Result<Option<crate::enrichment::skeleton::FieldSkeleton>> {
        let path = self.path().join("field_skeleton.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        let skeleton = serde_json::from_str(&raw)
            .map_err(|e| Error::Serialization(format!("Bad field_skeleton.json: {e}")))?;
        Ok(Some(skeleton))
    }
}
