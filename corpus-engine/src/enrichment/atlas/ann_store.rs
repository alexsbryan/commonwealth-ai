// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AnnSeedTable` — a small Lance-backed `key -> vector` ANN table.
//!
//! The storage primitive behind ATLAS_STORAGE_V2's atom-seed ANN: build a flat
//! (no IVF-PQ) Lance vector table from `(key, embedding)` rows and query it for
//! the nearest keys to a query vector. It lives in `corpus-engine` because this
//! is the storage/index domain (alongside `chunks.lance` and the atlas archive)
//! — and it is the embryo of the Stage-B `atoms.lance` embedding-column ANN, so
//! it is reusable rather than throwaway. The caller owns the key semantics
//! (e.g. `"{corpus_id}\u{1f}{atom_id}"`) and the directory lifetime.

use std::path::Path;
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{types::Float32Type, Array, FixedSizeListArray, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

/// A flat Lance vector table over opaque string keys. Cheap to build at the
/// hundreds-to-thousands scale of pooled atlas seeds; `nearest` brute-forces
/// exactly (no quantization), so its ranking equals exact cosine.
pub struct AnnSeedTable {
    table: lancedb::Table,
}

impl AnnSeedTable {
    /// Build the table under `dir` (which must exist and outlive the table —
    /// the caller owns it, e.g. a `tempfile::TempDir`). `rows` are
    /// `(key, embedding)`; all embeddings must share the dimension of the first.
    pub async fn build(dir: &Path, rows: &[(String, Vec<f32>)]) -> Result<Self, String> {
        let dim = rows
            .first()
            .map(|(_, e)| e.len())
            .filter(|&d| d > 0)
            .ok_or_else(|| "AnnSeedTable::build: empty rows / zero-dim".to_string())?;

        let schema = Arc::new(Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new(
                "embedding",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dim as i32,
                ),
                true,
            ),
        ]));
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        let emb_arr = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            rows.iter().map(|(_, e)| Some(e.iter().map(|&v| Some(v)))),
            dim as i32,
        );
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(keys)), Arc::new(emb_arr)],
        )
        .map_err(|e| format!("AnnSeedTable record batch: {e}"))?;

        let db = lancedb::connect(dir.to_str().ok_or("AnnSeedTable: non-utf8 dir")?)
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable connect: {e}"))?;
        let table = db
            .create_empty_table("seeds", schema)
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable create: {e}"))?;
        table
            .add(vec![batch])
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable add: {e}"))?;
        Ok(Self { table })
    }

    /// The `k` nearest keys to `query`, ranked (closest first). No score is
    /// returned — at this scale the search is exact, so a caller wanting
    /// bit-identical scores re-computes cosine against its own vectors.
    pub async fn nearest(&self, query: &[f32], k: usize) -> Result<Vec<String>, String> {
        let stream = self
            .table
            .query()
            .nearest_to(query.to_vec())
            .map_err(|e| format!("AnnSeedTable nearest_to: {e}"))?
            .limit(k)
            .select(Select::Columns(vec!["key".into()]))
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable execute: {e}"))?;
        let batches: Vec<RecordBatch> = stream
            .try_collect()
            .await
            .map_err(|e| format!("AnnSeedTable collect: {e}"))?;
        let mut keys = Vec::new();
        for b in &batches {
            if let Some(col) = b
                .column_by_name("key")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            {
                for i in 0..col.len() {
                    keys.push(col.value(i).to_string());
                }
            }
        }
        Ok(keys)
    }
}
