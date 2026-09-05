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
///
/// Since ei-3c freshness is TWO questions, not one. mtime answers "was it
/// embedded from these atoms"; it cannot answer "was it embedded from these
/// KINDS", and that is the one that mattered — the seed population moved from
/// the retrieval filter's Entity-only admission to the corpus's navigation map
/// (`super::seed_population`), so every table on disk is fresh by mtime and
/// wrong by population. A table whose recorded population is not the one this
/// build derives is stale, and the check is deliberately cheap (one small read,
/// two mtimes, no `ontology.json` parse) because the daemon runs it once per
/// installed atlas at boot.
pub fn ann_table_is_fresh(atlas_dir: &Path) -> bool {
    if !super::seed_population::population_marker_is_current(atlas_dir) {
        return false;
    }
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

/// How a lifecycle point that writes an atlas supplies the atom embeddings the
/// seed table needs.
///
/// There is no default and no `Option`. `atoms_ann.lance` is a MANDATORY ingest
/// artifact beside `atoms.lance` and `edges.csr` (EPISTEMIC_INDEX §1, Ideas
/// row), and the failure this type exists to make impossible is the quiet one:
/// an atlas written with a store and no seed table loads, enumerates, and
/// reports no error, while every grounded answer over it silently falls back to
/// cosine over chunks. Before this, seeding was a separate step three surfaces
/// remembered to call and every other write forgot — SEP is the evidence, 22 of
/// 1,770 atlases seeded (ARCH §7: make it structural, not remembered).
///
/// So every caller of [`super::writer::write_atlas_full`] states which of the
/// two this write is, both are traced, and [`SeedOutcome`] carries the answer
/// back so no caller can mistake one for the other (ARCH §18.3).
#[derive(Clone)]
pub enum AtlasSeeding {
    /// Embed the atoms and write the seed table in this same write, through
    /// [`super::context_loader::backfill_ann`] — the ONE writer (ARCH §10.6).
    /// A failure here fails the atlas write, exactly as an unwritable
    /// `atoms.lance` does.
    ///
    /// The embedder MUST be the query-side one: the table and the queries that
    /// search it share one vector space, and a document-side embed degrades
    /// grounding without failing anything.
    With(crate::types::EmbedFn),
    /// This lifecycle point has no embedder, and says why. The reason is
    /// traced and rides back in [`SeedOutcome::Deferred`]; the atlas is left
    /// seedless and `svrn atlas backfill-ann <id>` completes it.
    ///
    /// Legitimate cases: a partial step-3a write whose step-3b sibling seeds, a
    /// test fixture with no inference, and a host that seeds on its own async
    /// runtime after the write returns. Not a way to skip the artifact.
    Deferred(&'static str),
}

impl std::fmt::Debug for AtlasSeeding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::With(_) => write!(f, "AtlasSeeding::With(<embed>)"),
            Self::Deferred(why) => write!(f, "AtlasSeeding::Deferred({why:?})"),
        }
    }
}

/// What an atlas write did about the seed table. Three outcomes, each in its
/// own words (ARCH §18.3) so a caller can report the absence rather than print
/// a zero: the table was written; the grounding filter admitted no atom, so
/// there was nothing to seed from; or the write deferred and named why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// `atoms_ann.lance` written with `rows` of `of` bag entries resolved to
    /// atom ids.
    Written { rows: usize, of: usize },
    /// The production grounding filter admitted nothing — a real shape for an
    /// atlas carrying only structural or Claims-only atoms. No table.
    NoSeedableAtoms { min_description_chars: usize },
    /// No embedder at this lifecycle point; the reason as given.
    Deferred(&'static str),
}

impl SeedOutcome {
    /// One line for an operator surface. Never renders an absence as a zero.
    pub fn describe(&self) -> String {
        match self {
            Self::Written { rows, of } => {
                format!("{ANN_TABLE_DIRNAME}: {rows}/{of} atoms embedded")
            }
            Self::NoSeedableAtoms {
                min_description_chars,
            } => format!(
                "{ANN_TABLE_DIRNAME} not written — the grounding filter \
                 (min_chars={min_description_chars}) admitted no atom"
            ),
            Self::Deferred(why) => {
                format!("{ANN_TABLE_DIRNAME} deferred — {why}")
            }
        }
    }
}

/// Rows in a corpus's `atoms_ann.lance`, i.e. how many atoms carry an embedding
/// the walk can seed on. `None` when there is no table (absence, which is what
/// [`AtlasSeeding::Deferred`] leaves behind) or when the table is unreadable —
/// never `Some(0)` for a missing table, because "no table" and "a table of no
/// atoms" are different facts (ARCH §18.3).
///
/// The one derivation of the coverage numerator: `atlas::summary` reports it and
/// `corpus_list` prints what `summary` recorded (ARCH §10.6).
pub async fn ann_table_rows(atlas_dir: &Path) -> Option<u64> {
    if !ann_table_present(atlas_dir) {
        return None;
    }
    match AnnSeedTable::open_for_atlas(atlas_dir).await {
        Ok(t) => match t.table.count_rows(None).await {
            Ok(n) => Some(n as u64),
            Err(e) => {
                tracing::warn!(
                    atlas = %atlas_dir.display(),
                    error = %e,
                    "ann seed table: present but row count failed; coverage reported as unknown"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                atlas = %atlas_dir.display(),
                error = %e,
                "ann seed table: present but unreadable; coverage reported as unknown"
            );
            None
        }
    }
}

/// `atoms_ann.lance` mtime in ms since the epoch, or `0` when there is no
/// table. The atlas summary's second cache key: the summary is derived from
/// `atoms.json` AND this table, so a summary computed before the seed landed
/// must not stay "current" after it (`summary::read_current_summary`).
pub fn ann_table_mtime_ms(atlas_dir: &Path) -> u64 {
    std::fs::metadata(ann_table_dir(atlas_dir))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

    /// Add `rows` to the table under `dir`, creating it when there is none.
    ///
    /// The seed table is written ONCE at backfill from an embedder
    /// ([`build_persistent_ann_seed_table`](super::context::build_persistent_ann_seed_table)),
    /// which is right for atoms whose vector is produced at write time. ei-7a's
    /// `Summary` atoms are the case that does not fit: their vectors already
    /// exist in `raptor_summaries.lance` and re-embedding them would be a
    /// SECOND decider for the seed space (§10.6) — so they are added to
    /// whatever table the atlas already has instead of forcing a full re-embed
    /// of every Entity beside them.
    ///
    /// Appending, not replacing: an existing table keeps every row it had. The
    /// caller owns de-duplication — a `key` written twice is two rows, and the
    /// walk would then see the same atom twice. `write_summary_atoms` gets this
    /// for free by refusing to re-project a node whose atom is already in
    /// `atoms.json`.
    pub async fn append_rows(dir: &Path, rows: &[(String, Vec<f32>)]) -> Result<usize, String> {
        if rows.is_empty() {
            return Ok(0);
        }
        if !dir.is_dir() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create ANN table dir: {e}"))?;
            Self::build(dir, rows).await?;
            return Ok(rows.len());
        }
        let existing = match Self::open(dir).await {
            Ok(t) => t,
            // A directory with no committed `seeds` table (a torn or aborted
            // build) is not an append target — say so rather than silently
            // creating a second table beside the wreckage (§18.3).
            Err(e) => return Err(format!("ANN table dir exists but is not readable ({e})")),
        };
        let dim = rows[0].1.len();
        if dim == 0 {
            return Err("append_rows: zero-dim embedding".into());
        }
        if rows.iter().any(|(_, e)| e.len() != dim) {
            return Err("append_rows: rows do not share one embedding dimension".into());
        }
        let schema = existing
            .table
            .schema()
            .await
            .map_err(|e| format!("append_rows: read schema: {e}"))?;
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        let emb_arr = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            rows.iter().map(|(_, e)| Some(e.iter().map(|&v| Some(v)))),
            dim as i32,
        );
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(keys)), Arc::new(emb_arr)],
        )
        .map_err(|e| format!("append_rows record batch (dim mismatch with the table?): {e}"))?;
        existing
            .table
            .add(vec![batch])
            .execute()
            .await
            .map_err(|e| format!("append_rows add: {e}"))?;
        Ok(rows.len())
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

    /// `append_rows` on a directory that has no table CREATES one, and on one
    /// that does ADDS to it — keeping every row it had.
    ///
    /// The second half is the load-bearing one. ei-7a adds a `Summary` seed to
    /// an atlas whose Entity seeds were embedded at backfill; if the append
    /// replaced the table, those Entity seeds would be silently deleted and
    /// the corpus would lose its baseline grounding surface to gain a summary.
    /// Nothing would error and the row count would still look plausible.
    #[tokio::test]
    async fn append_creates_then_adds_without_losing_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let table_dir = dir.path().join("atoms_ann.lance");

        // No table yet: the append creates one.
        let n = AnnSeedTable::append_rows(
            &table_dir,
            &[("entity-1".to_string(), vec![1.0_f32, 0.0, 0.0])],
        )
        .await
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(ann_table_rows(dir.path()).await, Some(1));

        // Table present: the append adds, and the first row survives.
        let n = AnnSeedTable::append_rows(
            &table_dir,
            &[
                ("summary-a".to_string(), vec![0.0, 1.0, 0.0]),
                ("summary-b".to_string(), vec![0.0, 0.0, 1.0]),
            ],
        )
        .await
        .unwrap();
        assert_eq!(n, 2);

        let keys: std::collections::BTreeSet<String> = AnnSeedTable::open(&table_dir)
            .await
            .unwrap()
            .all_rows()
            .await
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            keys,
            ["entity-1", "summary-a", "summary-b"]
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>(),
            "the pre-existing Entity seed must still be there"
        );
    }

    /// A width mismatch REFUSES rather than writing a table the walk cannot
    /// query. Two embed models on one atlas is the failure this guards, and
    /// it is exactly the shape that produces a plausible table and nonsense
    /// rankings (§18.3 — refuse, never substitute).
    #[tokio::test]
    async fn appending_a_different_width_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let table_dir = dir.path().join("atoms_ann.lance");
        AnnSeedTable::append_rows(&table_dir, &[("a".to_string(), vec![1.0_f32, 0.0, 0.0])])
            .await
            .unwrap();
        let err = AnnSeedTable::append_rows(&table_dir, &[("b".to_string(), vec![1.0_f32, 0.0])])
            .await
            .unwrap_err();
        assert!(!err.is_empty(), "the refusal must say something");
        // And the good row is untouched.
        assert_eq!(ann_table_rows(dir.path()).await, Some(1));
    }

    /// Rows that do not share one width are refused as a SET, before anything
    /// is written — a partial append would leave the table half in one space.
    #[tokio::test]
    async fn a_ragged_batch_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let table_dir = dir.path().join("atoms_ann.lance");
        AnnSeedTable::append_rows(&table_dir, &[("a".to_string(), vec![1.0_f32, 0.0, 0.0])])
            .await
            .unwrap();
        AnnSeedTable::append_rows(
            &table_dir,
            &[
                ("b".to_string(), vec![0.0_f32, 1.0, 0.0]),
                ("c".to_string(), vec![0.0_f32, 1.0]),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(ann_table_rows(dir.path()).await, Some(1));
    }

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
            // A table is written WITH its population marker since ei-3c, so a
            // fixture exercising the MTIME clause has to write one too — else
            // every case below short-circuits on the population clause and the
            // mtime rule is never actually tested (§18.1).
            let pop = super::super::seed_population::seed_population(tmp.path());
            super::super::seed_population::write_population_marker(tmp.path(), &pop).unwrap();
            std::fs::File::open(&d)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(t))
                .unwrap();
        }
        tmp
    }

    /// The population clause, watched failing: a table with the right mtime and
    /// NO marker is the state every atlas on disk was in before ei-3c —
    /// Entity-only, and fresh by mtime. It must read as stale.
    #[test]
    fn ann_table_is_fresh_table_without_a_population_marker_is_stale() {
        let tmp = freshness_fixture(Some(1_000), Some(2_000));
        std::fs::remove_file(super::super::seed_population::population_marker_path(
            tmp.path(),
        ))
        .unwrap();
        assert!(!ann_table_is_fresh(tmp.path()));
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
