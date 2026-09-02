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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use arrow_array::{types::Float32Type, Array, FixedSizeListArray, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

/// Directory name (under a corpus's `atlas/` dir) of the persistent ANN seed
/// table — ATLAS_STORAGE_V2 3b. A Lance DB directory holding the `seeds` table,
/// written once by the backfill and reopened read-only at runtime. Shared by
/// the writer (backfill) and the readers (the daemon's `AtlasContextManager`
/// and the eval runner) so the on-disk location can never drift between them.
pub const ANN_TABLE_DIRNAME: &str = "atoms_ann.lance";

/// The persistent ANN seed table directory for a corpus's `atlas/` directory.
pub fn ann_table_dir(atlas_dir: &Path) -> PathBuf {
    atlas_dir.join(ANN_TABLE_DIRNAME)
}

/// Whether a corpus has been backfilled with an ANN seed table. Cheap existence
/// gate (the directory is present); [`AnnSeedTable::open_for_atlas`] does the
/// real validation and is the authority on readability.
pub fn ann_table_present(atlas_dir: &Path) -> bool {
    ann_table_dir(atlas_dir).is_dir()
}

/// Whether the ANN seed table is at least as new as `atoms.json` — the
/// "already built" test the `enrich build` Backfill step and the daemon's
/// post-write hook share (ontology-v1 P0). [`ann_table_present`] answers only
/// "is there a table"; a table older than the atlas it was embedded from
/// seeds grounding with atoms the last resolve renamed or deleted, which is
/// worse than no table (ATLAS_STORAGE_V2 3b keys the table on atom-id). Same
/// mtime idiom as `store::store_needs_build`.
///
/// Absent table → `false`. Table present but no `atoms.json` → `true`
/// (nothing newer exists to embed). An unreadable mtime → `false` (rebuild).
pub fn ann_table_is_fresh(atlas_dir: &Path) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let Some(table) = mtime(&ann_table_dir(atlas_dir)) else {
        return false;
    };
    // `atoms.json` is the file `writer::write_atlas_full` stamps last.
    match mtime(&atlas_dir.join("atoms.json")) {
        Some(atoms) => table >= atoms,
        None => true,
    }
}

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

    /// Open a table previously [`build`](Self::build)t under `dir` — the
    /// production path: the ANN seed table is built ONCE at backfill (so the
    /// `resolve_atom_id_from_entry` join runs at build time, not per query) and
    /// reopened read-only at runtime. ATLAS_STORAGE_V2 step 3b.
    pub async fn open(dir: &Path) -> Result<Self, String> {
        let db = lancedb::connect(dir.to_str().ok_or("AnnSeedTable: non-utf8 dir")?)
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable open connect: {e}"))?;
        let table = db
            .open_table("seeds")
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable open: {e}"))?;
        Ok(Self { table })
    }

    /// Open the persistent ANN seed table living under a corpus's `atlas/`
    /// directory (`<atlas_dir>/atoms_ann.lance`). The runtime convenience over
    /// [`open`](Self::open) — pairs with [`ann_table_present`] for the gate.
    pub async fn open_for_atlas(atlas_dir: &Path) -> Result<Self, String> {
        Self::open(&ann_table_dir(atlas_dir)).await
    }

    /// Like [`nearest`](Self::nearest) but returns each hit's stored vector
    /// alongside its key. The production seed path (ATLAS_STORAGE_V2 3b)
    /// re-scores ANN hits with the canonical `cosine()` so the BFS sees the
    /// same seed scores v1 produced — and this returns the vectors to score
    /// against WITHOUT keeping an in-memory embedding bag resident: only the
    /// `k` hit vectors come back. The ANN supplies the candidate ranking; the
    /// re-score supplies the bit-identical seed weights.
    pub async fn nearest_with_vectors(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<(String, Vec<f32>)>, String> {
        let stream = self
            .table
            .query()
            .nearest_to(query.to_vec())
            .map_err(|e| format!("AnnSeedTable nearest_to: {e}"))?
            .limit(k)
            .select(Select::Columns(vec!["key".into(), "embedding".into()]))
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable execute: {e}"))?;
        let batches: Vec<RecordBatch> = stream
            .try_collect()
            .await
            .map_err(|e| format!("AnnSeedTable collect: {e}"))?;
        let mut out: Vec<(String, Vec<f32>)> = Vec::new();
        for b in &batches {
            let keys = b
                .column_by_name("key")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let embs = b
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
            let (Some(keys), Some(embs)) = (keys, embs) else {
                continue;
            };
            for i in 0..keys.len() {
                if embs.is_null(i) {
                    continue;
                }
                let vec_ref = embs.value(i);
                let vals = vec_ref
                    .as_any()
                    .downcast_ref::<arrow_array::Float32Array>()
                    .map(|a| a.values().to_vec())
                    .unwrap_or_default();
                out.push((keys.value(i).to_string(), vals));
            }
        }
        Ok(out)
    }

    /// Every `(key, embedding)` row in the table, unordered. The bag-build read
    /// path (ATLAS_STORAGE_V2 Phase B): the daemon / eval derive the atlas
    /// embedding bag by scanning the ANN table (`key` == atom-id) and joining
    /// each row to its resident atom for the rendered text — so the atom
    /// embeddings live ONLY here, never re-embedded at load and never in an
    /// `atoms.embeddings.bin` sidecar.
    pub async fn all_rows(&self) -> Result<Vec<(String, Vec<f32>)>, String> {
        let stream = self
            .table
            .query()
            .select(Select::Columns(vec!["key".into(), "embedding".into()]))
            .execute()
            .await
            .map_err(|e| format!("AnnSeedTable all_rows execute: {e}"))?;
        let batches: Vec<RecordBatch> = stream
            .try_collect()
            .await
            .map_err(|e| format!("AnnSeedTable all_rows collect: {e}"))?;
        let mut out: Vec<(String, Vec<f32>)> = Vec::new();
        for b in &batches {
            let keys = b
                .column_by_name("key")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let embs = b
                .column_by_name("embedding")
                .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
            let (Some(keys), Some(embs)) = (keys, embs) else {
                continue;
            };
            for i in 0..keys.len() {
                if embs.is_null(i) {
                    continue;
                }
                let vals = embs
                    .value(i)
                    .as_any()
                    .downcast_ref::<arrow_array::Float32Array>()
                    .map(|a| a.values().to_vec())
                    .unwrap_or_default();
                out.push((keys.value(i).to_string(), vals));
            }
        }
        Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// build -> persist -> reopen -> query: the production lifecycle (ANN table
    /// is written once at backfill, reopened read-only at runtime). Proves
    /// `open` round-trips `build` and both query shapes rank by the stored
    /// vectors. Unit basis vectors make L2-nearest == cosine-nearest, so the
    /// expected order is unambiguous.
    #[tokio::test]
    async fn build_open_roundtrip_ranks_by_vector() {
        let dir = tempfile::tempdir().unwrap();
        let rows = vec![
            ("a".to_string(), vec![1.0_f32, 0.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0, 0.0]),
            ("c".to_string(), vec![0.0, 0.0, 1.0]),
        ];
        AnnSeedTable::build(dir.path(), &rows).await.unwrap();

        // Reopen the persisted table — the runtime path, distinct from the
        // in-process handle `build` returns.
        let table = AnnSeedTable::open(dir.path()).await.unwrap();

        let near = table.nearest(&[0.9, 0.1, 0.0], 1).await.unwrap();
        assert_eq!(near, vec!["a".to_string()]);

        let with_vecs = table
            .nearest_with_vectors(&[0.05, 0.9, 0.05], 2)
            .await
            .unwrap();
        assert_eq!(with_vecs.len(), 2);
        assert_eq!(with_vecs[0].0, "b");
        // The vector comes back verbatim so the caller can re-score with cosine.
        assert_eq!(with_vecs[0].1, vec![0.0, 1.0, 0.0]);
    }

    /// Freshness fixture: an `atlas/` dir with `atoms.json` stamped at
    /// `atoms_secs` and (optionally) an ANN table dir stamped at
    /// `table_secs`, both relative to the epoch so the order is explicit.
    fn freshness_fixture(atoms_secs: Option<u64>, table_secs: Option<u64>) -> tempfile::TempDir {
        use std::time::{Duration, UNIX_EPOCH};
        let tmp = tempfile::tempdir().unwrap();
        if let Some(a) = atoms_secs {
            let p = tmp.path().join("atoms.json");
            std::fs::write(&p, "{}").unwrap();
            std::fs::File::open(&p)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(a))
                .unwrap();
        }
        if let Some(t) = table_secs {
            let d = ann_table_dir(tmp.path());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::File::open(&d)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(t))
                .unwrap();
        }
        tmp
    }

    #[test]
    fn ann_table_is_fresh_absent_table_is_not_fresh() {
        let tmp = freshness_fixture(Some(2_000), None);
        assert!(!ann_table_is_fresh(tmp.path()));
    }

    #[test]
    fn ann_table_is_fresh_table_newer_than_atoms_is_fresh() {
        let tmp = freshness_fixture(Some(1_000), Some(2_000));
        assert!(ann_table_is_fresh(tmp.path()));
    }

    /// The falsifier `ann_table_present` cannot pass: a table that exists
    /// but predates the atoms.json it should have been embedded from.
    #[test]
    fn ann_table_is_fresh_table_older_than_atoms_is_stale() {
        let tmp = freshness_fixture(Some(2_000), Some(1_000));
        assert!(!ann_table_is_fresh(tmp.path()));
    }

    #[test]
    fn ann_table_is_fresh_table_without_atoms_json_is_fresh() {
        let tmp = freshness_fixture(None, Some(1_000));
        assert!(ann_table_is_fresh(tmp.path()));
    }
}
