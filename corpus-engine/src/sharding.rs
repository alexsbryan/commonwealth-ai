//! Shard operations: the three-operation contract between corpus-engine
//! and Commonwealth.
//!
//! - `index_stats`: report chunk ID range, count, and size
//! - `extract_shard`: extract a subset of an index into a new directory
//! - `merge_shards`: merge multiple shard directories into a single index

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{Array, ArrayRef, BooleanArray, Int64Array, RecordBatch, StringArray};
use arrow::compute::filter_record_batch;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::types::{ChunkRange, IndexInfo, IndexStats, ShardInfo};

/// Promote a single partition directory to a canonical corpus index by
/// renaming it into place, then rewriting `_corpus_meta.json` so it no
/// longer carries partition-specific fields (`is_shard`, chunk range,
/// `processed_shards`) and is marked ingestion-complete.
///
/// Zero-copy finalisation for the "no peers ever joined" case — avoids
/// copying tens of gigabytes of chunks into a fresh LanceDB index only
/// to end up with the same content.
///
/// Errors out (without touching either path) when:
///   - the source does not exist, does not contain `_corpus_meta.json`,
///     or is a file rather than a directory;
///   - the output path already exists (caller should use the full
///     `merge_shards` path to fold into existing canonical data).
///
/// The rename falls back to copy-then-remove when source and output are
/// on different filesystems (common when `index_dir` is a bind mount).
pub fn promote_single_shard(source: &Path, output: &Path) -> Result<()> {
    if !source.is_dir() {
        return Err(Error::NoShardsFound(format!(
            "promote_single_shard: source {} is not a directory",
            source.display()
        )));
    }
    let source_meta = source.join("_corpus_meta.json");
    if !source_meta.exists() {
        return Err(Error::NoShardsFound(format!(
            "promote_single_shard: {} has no _corpus_meta.json",
            source.display()
        )));
    }
    if output.exists() {
        return Err(Error::Database(format!(
            "promote_single_shard: refusing to overwrite existing {}",
            output.display()
        )));
    }

    // Same-filesystem rename is atomic. Fall back to recursive copy if
    // rename fails with `EXDEV` or similar.
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Err(e) = std::fs::rename(source, output) {
        tracing::warn!(
            source = %source.display(),
            output = %output.display(),
            error = %e,
            "promote_single_shard: atomic rename failed, falling back to copy"
        );
        copy_dir_recursive(source, output)?;
        std::fs::remove_dir_all(source)?;
    }

    // Rewrite meta to drop partition-specific fields and mark complete.
    let raw = std::fs::read_to_string(output.join("_corpus_meta.json"))?;
    let mut meta: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| Error::Serialization(format!("read meta: {e}")))?;
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("is_shard".into(), serde_json::Value::Bool(false));
        obj.insert("chunk_range_start".into(), serde_json::Value::Null);
        obj.insert("chunk_range_end".into(), serde_json::Value::Null);
        obj.insert(
            "processed_shards".into(),
            serde_json::Value::Array(Vec::new()),
        );
        obj.insert(
            "ingestion_in_progress".into(),
            serde_json::Value::Bool(false),
        );
    }
    std::fs::write(
        output.join("_corpus_meta.json"),
        serde_json::to_string_pretty(&meta)
            .map_err(|e| Error::Serialization(format!("write meta: {e}")))?,
    )?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_child = entry.path();
        let dst_child = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src_child, &dst_child)?;
        } else {
            std::fs::copy(&src_child, &dst_child)?;
        }
    }
    Ok(())
}

/// Report chunk ID range, count, and size for an index.
pub async fn index_stats(index_path: &Path) -> Result<IndexStats> {
    let index = CorpusIndex::open(index_path).await?;
    let info = index.info().await?;

    // Query min/max IDs.
    let batches: Vec<RecordBatch> = index
        .table()
        .query()
        .select(Select::Columns(vec!["id".into()]))
        .execute()
        .await
        .map_err(|e| Error::Database(format!("stats query: {e}")))?
        .try_collect()
        .await
        .map_err(|e| Error::Database(format!("stats collect: {e}")))?;

    let mut min_id = u64::MAX;
    let mut max_id = 0u64;
    for batch in &batches {
        if let Some(ids) = batch
            .column_by_name("id")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        {
            for i in 0..ids.len() {
                let id = ids.value(i) as u64;
                min_id = min_id.min(id);
                max_id = max_id.max(id);
            }
        }
    }

    if min_id == u64::MAX {
        min_id = 0;
    }

    Ok(IndexStats {
        corpus_id: info.corpus_id,
        total_chunks: info.chunk_count,
        min_chunk_id: min_id,
        max_chunk_id: max_id + 1, // exclusive
        index_size_bytes: info.index_size_bytes,
    })
}

/// Extract a subset of an existing index into a new directory.
/// Output contains only chunks with IDs in the given range.
pub async fn extract_shard(
    source_path: &Path,
    chunk_range: ChunkRange,
    output_path: &Path,
) -> Result<ShardInfo> {
    let source = CorpusIndex::open(source_path).await?;
    let source_info = source.info().await?;

    // Create the shard index.
    let shard = CorpusIndex::create(
        output_path,
        &source_info.corpus_id,
        &source_info.corpus_name,
        &source_info.embedding_model,
        source_info.embedding_dimensions,
        source_info.mesh_sharing,
        &"",
    )
    .await?;

    // Mark as shard.
    shard.set_shard_meta(chunk_range)?;

    // Query chunks in range from source.
    let filter = format!(
        "id >= {} AND id < {}",
        chunk_range.start_id, chunk_range.end_id
    );
    let batches: Vec<RecordBatch> = source
        .table()
        .query()
        .only_if(filter)
        .execute()
        .await
        .map_err(|e| Error::Database(format!("shard extract query: {e}")))?
        .try_collect()
        .await
        .map_err(|e| Error::Database(format!("shard extract collect: {e}")))?;

    if !batches.is_empty() {
        shard
            .table()
            .add(batches)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("shard insert: {e}")))?;
    }

    let chunk_count = shard.chunk_count().await?;
    let size_bytes = dir_size(output_path);

    Ok(ShardInfo {
        path: output_path.to_path_buf(),
        chunk_range,
        chunk_count,
        size_bytes,
    })
}

/// Merge multiple shard directories into a single index.
///
/// Chunks are renumbered to form a contiguous ID space. Duplicate chunks
/// are dropped by a two-key lookup:
///
/// 1. **`content_hash`** (primary) — handles legacy in-flight overlap
///    (same file appears in two shards after coordinator/peer reassignment)
///    AND pull-based lease-expiry overlap (two peers process the same
///    `unit_id` → identical chunking → identical content_hashes).
/// 2. **`unit_id`** (secondary, pull-based only) — when a chunk's
///    `content_hash` is NULL (shouldn't happen under the ingest pipeline
///    but defensive), the `(unit_id, source_doc_id)` pair catches
///    duplicates that content_hash dedup would miss.
///
/// Chunks with both keys NULL are always included (conservative: cannot
/// deduplicate without any signal).
pub async fn merge_shards(
    shard_paths: &[PathBuf],
    output_path: &Path,
) -> Result<IndexInfo> {
    if shard_paths.is_empty() {
        return Err(Error::NoShardsFound("no shard paths provided".into()));
    }

    // Single-shard fast path: when the coordinator (or a solo install)
    // has only one input to "merge", copying every chunk into a fresh
    // LanceDB index is pure waste — that's tens of gigabytes of I/O for
    // Wikipedia. Atomic-rename the shard directory into place instead
    // and rewrite `_corpus_meta.json` to shed its is_shard/partition
    // metadata. The fast path refuses when the output path already
    // holds data so we never clobber a prior canonical index; callers
    // that actually do want to fold a partition into an existing
    // canonical fall through to the full merge below (which dedupes
    // via `content_hash`).
    if shard_paths.len() == 1 {
        let source = &shard_paths[0];
        let output_has_data = output_path.join("_corpus_meta.json").exists();
        if !output_has_data && source != output_path {
            promote_single_shard(source, output_path)?;
            return CorpusIndex::open(output_path)
                .await?
                .info()
                .await;
        }
    }

    // Read metadata from first shard.
    let first = CorpusIndex::open(&shard_paths[0]).await?;
    let first_info = first.info().await?;
    drop(first);

    // Validate that all shards share the same embedding model and
    // dimensions. A genuine mismatch would silently produce a merged
    // index where vectors from different embedding spaces are
    // compared directly — nonsense results. We must reject those.
    //
    // BUT: an empty `embedding_model` string ("") is treated as a
    // wildcard, not as a mismatch. Observed in the wild on
    // peer-pulled partition meta where the peer-pull copy path
    // failed to stamp the model name — the chunks themselves are
    // valid (the peer's actual embed function produced them) but
    // the meta wasn't carried over. Treating "" as compatible with
    // any non-empty value lets recovery merges proceed instead of
    // dead-ending an otherwise-fine partition; the canonical output
    // is stamped with the resolved (non-empty) model below.
    //
    // Embedding dimensions are still compared strictly — those come
    // from the actual vector schema, which is impossible to fake or
    // omit, so a dim mismatch is always real.
    let mut total_input_chunks: u64 = first_info.chunk_count;
    let mut resolved_model = first_info.embedding_model.clone();
    for shard_path in shard_paths.iter().skip(1) {
        let shard = CorpusIndex::open(shard_path).await?;
        let shard_info = shard.info().await?;
        drop(shard);

        // Resolve embedding_model across the shards.
        match (resolved_model.is_empty(), shard_info.embedding_model.is_empty()) {
            (true, false) => {
                tracing::info!(
                    shard = %shard_path.display(),
                    resolved = %shard_info.embedding_model,
                    "merge_shards: first shard had empty embedding_model — \
                     adopting '{}' from a later shard for the canonical meta",
                    shard_info.embedding_model,
                );
                resolved_model = shard_info.embedding_model.clone();
            }
            (false, true) => {
                tracing::warn!(
                    shard = %shard_path.display(),
                    expected = %resolved_model,
                    "merge_shards: shard meta is missing embedding_model — \
                     treating as wildcard match for '{}' (typically a \
                     peer-pull copy bug; chunks are still valid)",
                    resolved_model,
                );
            }
            (true, true) => {
                // Both empty so far — keep looking.
            }
            (false, false) => {
                if shard_info.embedding_model != resolved_model {
                    return Err(Error::ShardMismatch(format!(
                        "embedding model mismatch: shard '{}' uses '{}' but resolved model is '{}'",
                        shard_path.display(),
                        shard_info.embedding_model,
                        resolved_model,
                    )));
                }
            }
        }

        if shard_info.embedding_dimensions != first_info.embedding_dimensions {
            return Err(Error::ShardMismatch(format!(
                "embedding dimensions mismatch: shard '{}' has {} dims but first shard has {}",
                shard_path.display(),
                shard_info.embedding_dimensions,
                first_info.embedding_dimensions,
            )));
        }
        total_input_chunks += shard_info.chunk_count;
    }
    if resolved_model.is_empty() {
        return Err(Error::ShardMismatch(format!(
            "every input shard has an empty embedding_model — cannot determine \
             the embedding model to stamp on the merged canonical. Input paths: {}",
            shard_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    tracing::info!(
        corpus_id = %first_info.corpus_id,
        shard_count = shard_paths.len(),
        total_input_chunks,
        embedding_model = %resolved_model,
        embedding_dimensions = first_info.embedding_dimensions,
        output = %output_path.display(),
        "merge_shards: starting — merging {} shard(s) into {}",
        shard_paths.len(), output_path.display(),
    );
    for (i, p) in shard_paths.iter().enumerate() {
        tracing::info!(
            shard_index = i,
            path = %p.display(),
            "merge_shards: shard {}/{}", i + 1, shard_paths.len(),
        );
    }

    // Create output index. Stamp the resolved (non-empty) embedding
    // model so the canonical's meta is authoritative even when a
    // peer-pulled input had blank meta.
    let merged = CorpusIndex::create(
        output_path,
        &first_info.corpus_id,
        &first_info.corpus_name,
        &resolved_model,
        first_info.embedding_dimensions,
        first_info.mesh_sharing,
        "",
    )
    .await?;

    let dim = first_info.embedding_dimensions;
    let mut next_id: i64 = 1;
    // Track seen content_hashes (primary key) and (unit_id, source_doc_id)
    // pairs (secondary key for rows with NULL content_hash).
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_units: std::collections::HashSet<(i32, String)> =
        std::collections::HashSet::new();
    let mut dedup_count: u64 = 0;

    for shard_path in shard_paths {
        let shard = CorpusIndex::open(shard_path).await?;

        let batches: Vec<RecordBatch> = shard
            .table()
            .query()
            .execute()
            .await
            .map_err(|e| Error::Database(format!("shard read: {e}")))?
            .try_collect()
            .await
            .map_err(|e| Error::Database(format!("shard collect: {e}")))?;

        for batch in &batches {
            let num_rows = batch.num_rows();
            if num_rows == 0 {
                continue;
            }

            // ── Two-key deduplication ─────────────────────────────────
            // Primary: content_hash. Secondary: (unit_id, source_doc_id)
            // for rows with NULL content_hash (defensive fallback; under
            // the pull-based queue path every chunk has a populated hash).
            // Build a boolean keep-mask: true for rows whose primary OR
            // secondary key has not been seen in any earlier shard.
            let hash_col = batch
                .column_by_name("content_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let unit_id_col = batch
                .column_by_name("unit_id")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::Int32Array>());
            let source_doc_col = batch
                .column_by_name("source_doc_id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            let keep_mask: BooleanArray = (0..num_rows)
                .map(|row| {
                    // Primary key: content_hash.
                    if let Some(col) = hash_col {
                        if !col.is_null(row) {
                            let h = col.value(row);
                            if seen_hashes.contains(h) {
                                dedup_count += 1;
                                return false;
                            }
                            seen_hashes.insert(h.to_string());
                            return true;
                        }
                    }
                    // Secondary key: (unit_id, source_doc_id) when both exist.
                    if let (Some(u_col), Some(d_col)) = (unit_id_col, source_doc_col) {
                        if !u_col.is_null(row) && !d_col.is_null(row) {
                            let key = (u_col.value(row), d_col.value(row).to_string());
                            if seen_units.contains(&key) {
                                dedup_count += 1;
                                return false;
                            }
                            seen_units.insert(key);
                            return true;
                        }
                    }
                    // Neither key available — include (cannot deduplicate).
                    true
                })
                .collect();

            // Filter the batch to kept rows.
            let filtered = filter_record_batch(batch, &keep_mask)
                .map_err(|e| Error::Serialization(format!("dedup filter: {e}")))?;

            let keep_count = filtered.num_rows();
            if keep_count == 0 {
                continue;
            }

            // Renumber IDs for kept rows only.
            let new_ids: Vec<i64> = (0..keep_count)
                .map(|i| next_id + i as i64)
                .collect();
            next_id += keep_count as i64;

            // Rebuild batch with new IDs and filtered rows.
            // Column order must match corpus_schema() exactly.
            let schema = crate::index::corpus_schema(dim);
            let null_str_col: ArrayRef = Arc::new(
                StringArray::from(vec![Option::<String>::None; keep_count]),
            );
            let null_i32_col: ArrayRef = Arc::new(
                arrow_array::Int32Array::from(vec![Option::<i32>::None; keep_count]),
            );
            let null_i64_col: ArrayRef = Arc::new(
                Int64Array::from(vec![Option::<i64>::None; keep_count]),
            );
            let col_or_null_str = |name: &str| {
                filtered
                    .column_by_name(name)
                    .cloned()
                    .unwrap_or_else(|| null_str_col.clone())
            };
            let col_or_null_i32 = |name: &str| {
                filtered
                    .column_by_name(name)
                    .cloned()
                    .unwrap_or_else(|| null_i32_col.clone())
            };
            let col_or_null_i64 = |name: &str| {
                filtered
                    .column_by_name(name)
                    .cloned()
                    .unwrap_or_else(|| null_i64_col.clone())
            };

            let new_batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int64Array::from(new_ids)),
                    filtered.column_by_name("content").unwrap().clone(),
                    filtered.column_by_name("title").unwrap().clone(),
                    filtered.column_by_name("url").unwrap().clone(),
                    filtered.column_by_name("embedding").unwrap().clone(),
                    filtered.column_by_name("metadata").unwrap().clone(),
                    col_or_null_str("content_hash"),
                    col_or_null_str("source_doc_id"),
                    col_or_null_str("symbol_name"),
                    col_or_null_str("symbol_kind"),
                    col_or_null_str("file_path"),
                    col_or_null_i32("line_start"),
                    col_or_null_i32("line_end"),
                    col_or_null_str("language"),
                    col_or_null_i64("mtime"),
                    // Preserve unit_id through merge so downstream tooling
                    // can attribute each chunk to the pull-queue unit that
                    // produced it. Legacy shards without the column get a
                    // NULL-filled replacement via col_or_null_i32.
                    col_or_null_i32("unit_id"),
                ],
            )
            .map_err(|e| Error::Serialization(format!("merge batch: {e}")))?;

            merged
                .table()
                .add(vec![new_batch])
                .execute()
                .await
                .map_err(|e| Error::Database(format!("merge insert: {e}")))?;
        }
    }

    if dedup_count > 0 {
        tracing::info!(
            dedup_count,
            output = %output_path.display(),
            "merge_shards: deduplication dropped {} duplicate chunks", dedup_count,
        );
    }

    let result = merged.info().await?;
    tracing::info!(
        corpus_id = %result.corpus_id,
        chunks_merged = result.chunk_count,
        chunks_deduped = dedup_count,
        output = %output_path.display(),
        "merge_shards: complete — {} chunks written ({} input, {} deduped)",
        result.chunk_count, total_input_chunks, dedup_count,
    );
    Ok(result)
}

/// Phase signals emitted by `merge_partitions_into_canonical` for
/// callers that want to render progress (CLI status lines, daemon
/// tracing, future progress streams to the UI).
#[derive(Debug, Clone)]
pub enum MergePhaseProgress {
    /// Discovery + preflight finished. `partition_count` is the
    /// number of `<corpus>-partition-*/` dirs that will be merged.
    DiscoveryComplete { partition_count: usize },
    /// Chunk merge phase finished (the `merge_shards` call). Reports
    /// the deduplication that happened during merge.
    MergeComplete { chunks_merged: u64, chunks_deduped: u64 },
    /// Meta-stamping phase finished (scope, processed_shards,
    /// total_shards, provenance).
    MetaStamped,
    /// Sub-phase of `build_indexes` finished — propagates the
    /// `(done, total)` pair from `build_indexes`'s callback.
    BuildSubPhase { done: u64, total: u64 },
    /// `build_indexes` finished and the canonical was marked
    /// `ingestion_complete`. Recovery is finished.
    Complete,
}

/// Result of a partition-merge recovery operation.
#[derive(Debug, Clone)]
pub struct PartitionMergeReport {
    pub partition_paths: Vec<PathBuf>,
    pub canonical_path: PathBuf,
    /// Total chunks across the input partitions before dedup.
    pub chunks_input: u64,
    /// Chunks present in the canonical after dedup + merge.
    pub chunks_merged: u64,
    /// Union of `processed_shards` across all input partitions.
    pub shard_union: std::collections::BTreeSet<usize>,
    /// Resolved `total_shards` (max across inputs, falling back to
    /// `max(union)+1` when none stamped).
    pub total_shards: Option<usize>,
    /// Resolved `embedding_model` stamped on the canonical (treats
    /// empty inputs as wildcard; see `merge_shards`).
    pub embedding_model: String,
    pub embedding_dimensions: usize,
}

/// Discover every `<corpus>-partition-*/` directory under `index_dir`,
/// merge them into the canonical `<corpus>/` index, dedup during
/// merge, stamp the canonical's metadata (scope, processed_shards,
/// total_shards, provenance), build vector + FTS indexes, and mark
/// the corpus ingestion-complete.
///
/// This is the durable recovery primitive shared by:
/// - `sovereign corpus merge-partitions <id>` (one-shot CLI rescue)
/// - The daemon's `auto_recover` loop (reactive recovery when
///   `corpus_collaborate` finds the queue drained with no canonical
///   and no live handoff blob to re-fire).
///
/// Refuses (without touching disk) when:
/// - The canonical `<corpus>/_corpus_meta.json` already exists.
///   Callers must `corpus remove` first if they really want to
///   rebuild from partitions.
/// - No `<corpus>-partition-*/` dirs exist.
/// - Embedding model genuinely conflicts (two distinct non-empty
///   values). An empty value on any input is treated as wildcard
///   (peer-pull stamp omission); see `merge_shards`.
/// - Embedding dimensions differ across any pair of partitions.
///
/// `progress` callback is fired at each phase boundary; pass `None`
/// to skip progress reporting. The callback runs synchronously on
/// this task — keep it cheap.
///
/// Returns a `PartitionMergeReport` describing what was merged.
/// Partition directories are LEFT IN PLACE; the caller decides
/// whether to delete them post-success.
pub async fn merge_partitions_into_canonical(
    index_dir: &Path,
    corpus_id: &str,
    progress: Option<Arc<dyn Fn(MergePhaseProgress) + Send + Sync>>,
) -> Result<PartitionMergeReport> {
    let canonical_path = index_dir.join(corpus_id);
    if canonical_path.join("_corpus_meta.json").exists() {
        return Err(Error::Database(format!(
            "merge_partitions_into_canonical: canonical at {} already exists — \
             refusing to clobber. Remove it first if you really want to rebuild \
             from partitions.",
            canonical_path.display(),
        )));
    }

    // Discovery: walk index_dir for <corpus>-partition-*/ dirs.
    let prefix = format!("{corpus_id}-partition-");
    let mut partition_paths: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(index_dir).map_err(|e| {
        Error::Database(format!(
            "merge_partitions_into_canonical: cannot read {}: {e}",
            index_dir.display()
        ))
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with(&prefix) {
            continue;
        }
        if !path.join("_corpus_meta.json").exists() {
            continue;
        }
        partition_paths.push(path);
    }
    partition_paths.sort();

    if partition_paths.is_empty() {
        return Err(Error::NoShardsFound(format!(
            "merge_partitions_into_canonical: no <{corpus_id}-partition-*> dirs in {}",
            index_dir.display()
        )));
    }

    // Read each partition's metadata: chunk_count, embedding_model/dim,
    // processed_shards, total_shards, scope. Used both for the
    // canonical post-merge stamp and for assembling the report.
    let mut chunks_input: u64 = 0;
    let mut shard_union: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    let mut total_shards_max: Option<usize> = None;
    let mut scope: Option<crate::index::ScopeMeta> = None;
    let mut embedding_model: Option<String> = None;
    let mut embedding_dimensions: Option<usize> = None;

    for path in &partition_paths {
        let idx = CorpusIndex::open(path).await?;
        let info = idx.info().await?;
        chunks_input = chunks_input.saturating_add(info.chunk_count);

        if embedding_dimensions.is_none() {
            embedding_dimensions = Some(info.embedding_dimensions);
        }
        // Resolve embedding_model: first non-empty wins; later
        // mismatches are caught by merge_shards (with the same
        // wildcard logic).
        if !info.embedding_model.is_empty() && embedding_model.is_none() {
            embedding_model = Some(info.embedding_model.clone());
        }

        for s in idx.processed_shards().unwrap_or_default() {
            shard_union.insert(s);
        }
        if let Some(s) = idx.read_scope().ok().flatten() {
            if scope.is_none() {
                scope = Some(s);
            }
        }

        // Read total_shards directly from the meta JSON since it's
        // not exposed via IndexInfo.
        let raw = std::fs::read_to_string(path.join("_corpus_meta.json"))
            .unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(n) = v["total_shards"].as_u64() {
                let n = n as usize;
                total_shards_max = Some(total_shards_max.map_or(n, |m| m.max(n)));
            }
        }
    }

    let total_shards = total_shards_max.or_else(|| {
        shard_union.iter().last().copied().map(|m| m + 1)
    });

    if let Some(cb) = &progress {
        cb(MergePhaseProgress::DiscoveryComplete {
            partition_count: partition_paths.len(),
        });
    }

    // ── Phase 1: chunk merge + dedup ──────────────────────────────
    let merged_info = merge_shards(&partition_paths, &canonical_path).await?;
    let chunks_deduped = chunks_input.saturating_sub(merged_info.chunk_count);
    if let Some(cb) = &progress {
        cb(MergePhaseProgress::MergeComplete {
            chunks_merged: merged_info.chunk_count,
            chunks_deduped,
        });
    }

    // ── Phase 2: stamp metadata that merge_shards doesn't preserve ─
    // CorpusIndex::create() (called inside merge_shards) writes a
    // default meta with empty processed_shards, no scope, no
    // total_shards, and the engine's default provenance. Replay the
    // resolved values from inputs onto the canonical.
    let canonical = CorpusIndex::open(&canonical_path).await?;

    if let Some(scope) = scope {
        canonical.write_scope(Some(scope))?;
    }
    for shard_idx in &shard_union {
        canonical.record_processed_shard(*shard_idx)?;
    }
    if let Some(n) = total_shards {
        canonical.set_total_shards(n)?;
    }
    crate::index::set_provenance(
        &canonical_path,
        crate::index::CorpusProvenance::SelfInitiated,
    )?;
    if let Some(cb) = &progress {
        cb(MergePhaseProgress::MetaStamped);
    }

    // ── Phase 3: build search indexes (vector IVF-PQ + FTS) ───────
    let progress_for_build = progress.clone();
    let sub_phase_cb: Option<Box<dyn Fn(u64, u64) + Send + Sync>> =
        progress_for_build.map(|cb| -> Box<dyn Fn(u64, u64) + Send + Sync> {
            Box::new(move |done, total| {
                cb(MergePhaseProgress::BuildSubPhase { done, total });
            })
        });
    canonical
        .build_indexes(true, true, sub_phase_cb.as_deref())
        .await?;
    canonical.mark_indexes_built()?;
    canonical.mark_ingestion_complete()?;
    if let Some(cb) = &progress {
        cb(MergePhaseProgress::Complete);
    }

    Ok(PartitionMergeReport {
        partition_paths,
        canonical_path,
        chunks_input,
        chunks_merged: merged_info.chunk_count,
        shard_union,
        total_shards,
        embedding_model: merged_info.embedding_model,
        embedding_dimensions: merged_info.embedding_dimensions,
    })
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = p.metadata() {
                total += m.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::InsertChunk;

    fn make_test_embedding(seed: f32) -> Vec<f32> {
        (0..8).map(|i| seed + i as f32 * 0.1).collect()
    }

    async fn create_test_index(path: &Path, chunk_count: u64) -> CorpusIndex {
        let index = CorpusIndex::create(
            path, "test", "Test Corpus", "test-model", 8, true, "MIT",
        )
        .await
        .unwrap();

        let chunks: Vec<_> = (0..chunk_count)
            .map(|i| {
                (
                    InsertChunk {
                        content: format!("Content for chunk {i}"),
                        title: Some(format!("Title {i}")),
                        url: None,
                        metadata: None,
                        content_hash: None,
                        source_doc_id: None,
                        source_file: None,
                        code: crate::index::InsertCodeMeta::default(),
                            unit_id: None,
                    },
                    make_test_embedding(i as f32),
                )
            })
            .collect();
        index.insert_batch(&chunks).await.unwrap();
        index
    }

    #[tokio::test]
    async fn test_index_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-stats");
        create_test_index(&path, 10).await;

        let stats = index_stats(&path).await.unwrap();
        assert_eq!(stats.corpus_id, "test");
        assert_eq!(stats.total_chunks, 10);
        assert!(stats.min_chunk_id >= 1);
        assert!(stats.max_chunk_id > stats.min_chunk_id);
        assert!(stats.index_size_bytes > 0);
    }

    #[tokio::test]
    async fn test_extract_shard() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source");
        create_test_index(&source_path, 10).await;

        let stats = index_stats(&source_path).await.unwrap();
        let mid = stats.min_chunk_id + 5;

        let shard_path = dir.path().join("shard");
        let shard_info = extract_shard(
            &source_path,
            ChunkRange::new(stats.min_chunk_id, mid),
            &shard_path,
        )
        .await
        .unwrap();

        assert_eq!(shard_info.chunk_count, 5);
        assert!(shard_info.size_bytes > 0);

        let shard = CorpusIndex::open(&shard_path).await.unwrap();
        let info = shard.info().await.unwrap();
        assert_eq!(info.chunk_count, 5);
        assert!(info.is_shard);
    }

    #[tokio::test]
    async fn test_merge_shards() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source");
        create_test_index(&source_path, 10).await;

        let stats = index_stats(&source_path).await.unwrap();
        let mid = stats.min_chunk_id + 5;

        let shard1_path = dir.path().join("shard1");
        let shard2_path = dir.path().join("shard2");
        extract_shard(
            &source_path,
            ChunkRange::new(stats.min_chunk_id, mid),
            &shard1_path,
        )
        .await
        .unwrap();
        extract_shard(
            &source_path,
            ChunkRange::new(mid, stats.max_chunk_id),
            &shard2_path,
        )
        .await
        .unwrap();

        let merged_path = dir.path().join("merged");
        let merged_info = merge_shards(
            &[shard1_path, shard2_path],
            &merged_path,
        )
        .await
        .unwrap();

        assert_eq!(merged_info.chunk_count, 10);
        assert!(!merged_info.is_shard);
        assert_eq!(merged_info.corpus_id, "test");
    }

    #[tokio::test]
    async fn single_shard_merge_fast_paths_to_rename() {
        // Given a single shard directory and no existing output, merge_shards
        // must rename in place rather than copy every chunk through LanceDB.
        // We verify the fast-path ran by: (a) the original directory no longer
        // exists, and (b) the merged index has the same chunk count.
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source");
        create_test_index(&source_path, 7).await;
        let stats = index_stats(&source_path).await.unwrap();

        let shard_path = dir.path().join("wikipedia-partition-local");
        extract_shard(
            &source_path,
            ChunkRange::new(stats.min_chunk_id, stats.max_chunk_id),
            &shard_path,
        )
        .await
        .unwrap();
        assert!(shard_path.exists());

        let merged_path = dir.path().join("wikipedia");
        let info = merge_shards(&[shard_path.clone()], &merged_path)
            .await
            .unwrap();
        assert_eq!(info.chunk_count, 7);
        assert!(
            !shard_path.exists(),
            "source partition dir should have been renamed away, got {}",
            shard_path.display()
        );
        assert!(merged_path.join("_corpus_meta.json").exists());

        // _corpus_meta.json must have shed partition-specific fields.
        let raw = std::fs::read_to_string(merged_path.join("_corpus_meta.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["is_shard"], serde_json::Value::Bool(false));
        assert!(v["processed_shards"].as_array().unwrap().is_empty());
        assert_eq!(v["ingestion_in_progress"], serde_json::Value::Bool(false));
    }

    #[tokio::test]
    async fn single_shard_merge_refuses_when_output_exists() {
        // If the output path already holds a corpus_meta, the fast-path
        // refuses and we fall through to full merge — which in turn produces
        // the expected error because LanceDB won't create a duplicate table.
        // What we care about here is that promote_single_shard itself leaves
        // everything alone when the output is populated.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        create_test_index(&src, 3).await;
        let dst = dir.path().join("dst");
        create_test_index(&dst, 1).await;

        let result = promote_single_shard(&src, &dst);
        assert!(result.is_err(), "must refuse to overwrite populated output");
        // Source stays untouched.
        assert!(src.join("_corpus_meta.json").exists());
    }

    #[tokio::test]
    async fn merge_shards_treats_empty_embedding_model_as_wildcard() {
        // Observed in the wild: a peer-pulled partition's meta has
        // `embedding_model: ""` even though its chunks were embedded
        // by the peer's actual qwen model. merge_shards must treat
        // empty as compatible with any non-empty value (otherwise the
        // recovery merge dead-ends), and must stamp the canonical
        // with the resolved (non-empty) model.
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("p1-with-model");
        let p2 = dir.path().join("p2-blank-model");
        create_test_index(&p1, 4).await; // model = "test-model"
        create_test_index(&p2, 4).await;

        // Blank out p2's embedding_model — simulate the peer-pull
        // copy bug.
        let p2_meta_path = p2.join("_corpus_meta.json");
        let raw = std::fs::read_to_string(&p2_meta_path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["embedding_model"] = serde_json::Value::String(String::new());
        std::fs::write(&p2_meta_path, serde_json::to_string(&v).unwrap()).unwrap();

        let merged_path = dir.path().join("merged");
        let info = merge_shards(&[p1, p2], &merged_path).await.expect(
            "merge should succeed with one blank embedding_model — it is treated as wildcard",
        );

        // Canonical stamped with the resolved (non-empty) model.
        assert_eq!(info.embedding_model, "test-model");
        // All chunks preserved (8 distinct contents — different seeds in create_test_index).
        assert_eq!(info.chunk_count, 8);
    }

    #[tokio::test]
    async fn merge_shards_rejects_two_distinct_non_empty_embedding_models() {
        // The wildcard relaxation only applies to empty strings —
        // a genuine model-name mismatch (qwen vs jina) still has to
        // error or the merged vector index would be nonsense.
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("p1");
        let p2 = dir.path().join("p2");
        create_test_index(&p1, 2).await; // model = "test-model"
        create_test_index(&p2, 2).await;

        // Rename p2's embedding_model to something different.
        let p2_meta_path = p2.join("_corpus_meta.json");
        let raw = std::fs::read_to_string(&p2_meta_path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["embedding_model"] = serde_json::Value::String("other-model".into());
        std::fs::write(&p2_meta_path, serde_json::to_string(&v).unwrap()).unwrap();

        let merged_path = dir.path().join("merged");
        let result = merge_shards(&[p1, p2], &merged_path).await;
        assert!(result.is_err(), "must error on distinct non-empty models");
    }

    #[tokio::test]
    async fn merge_shards_rejects_when_every_input_has_blank_embedding_model() {
        // If we genuinely have no model name to stamp on the canonical,
        // we error rather than write an unsearchable index.
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("p1");
        let p2 = dir.path().join("p2");
        create_test_index(&p1, 2).await;
        create_test_index(&p2, 2).await;

        for path in [&p1, &p2] {
            let meta_path = path.join("_corpus_meta.json");
            let raw = std::fs::read_to_string(&meta_path).unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            v["embedding_model"] = serde_json::Value::String(String::new());
            std::fs::write(&meta_path, serde_json::to_string(&v).unwrap()).unwrap();
        }

        let merged_path = dir.path().join("merged");
        let result = merge_shards(&[p1, p2], &merged_path).await;
        assert!(
            result.is_err(),
            "must error when no input shard has a non-empty embedding_model"
        );
    }

    #[tokio::test]
    async fn merge_partitions_into_canonical_recovers_stranded_state() {
        // The motivating case: two partition dirs on disk for the
        // same corpus, no canonical, merge step never fired. The
        // recovery primitive should produce a built canonical with:
        // - all distinct chunks merged (after dedup)
        // - scope replayed from one of the inputs
        // - processed_shards = union of inputs
        // - total_shards stamped
        // - provenance = SelfInitiated
        // - indexes_built = true, ingestion_in_progress = false
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path();

        // create_test_index materialises the index AT the given path
        // (not in a subdir), so we point it directly at the partition
        // paths the discovery code will scan for.
        let p1 = index_dir.join("foo-partition-node-aaaa");
        let p2 = index_dir.join("foo-partition-node-bbbb");
        let i1 = create_test_index(&p1, 4).await;
        let i2 = create_test_index(&p2, 4).await;

        // Stamp processed_shards + a scope on each input; the
        // recovery primitive must replay these onto the canonical.
        i1.record_processed_shard(0).unwrap();
        i1.record_processed_shard(1).unwrap();
        i2.record_processed_shard(2).unwrap();
        i2.record_processed_shard(3).unwrap();
        i1.write_scope(Some(crate::index::ScopeMeta {
            filter_descriptions: vec!["test scope".into()],
            filter_signature: "abcdef".into(),
            expandable: true,
        }))
        .unwrap();
        // Stamp total_shards on i2 only — function should read the
        // max across inputs.
        i2.set_total_shards(5).unwrap();
        drop(i1);
        drop(i2);

        // Sanity-check the partitions are discoverable.
        assert!(p1.join("_corpus_meta.json").exists());
        assert!(p2.join("_corpus_meta.json").exists());

        // ── Run the recovery ─────────────────────────────────────
        let report = merge_partitions_into_canonical(index_dir, "foo", None)
            .await
            .expect("merge should succeed");
        assert_eq!(report.partition_paths.len(), 2);
        assert_eq!(report.canonical_path, index_dir.join("foo"));

        // create_test_index seeds chunks with content "Content for
        // chunk {i}" 0..N — the SAME content across both partitions.
        // Dedup-by-content_hash should collapse them. (This is also
        // a useful check that dedup actually fires, not just that
        // chunks pass through.)
        // ...except chunks created here have content_hash=None
        // (the helper passes None), so the two-key dedup falls back
        // to the secondary (unit_id, source_doc_id) — both also None
        // — meaning all 8 are kept (no signal to dedup on).
        assert_eq!(report.chunks_input, 8);
        assert_eq!(report.chunks_merged, 8);
        assert_eq!(
            report.shard_union,
            std::collections::BTreeSet::from([0usize, 1, 2, 3])
        );
        assert_eq!(report.total_shards, Some(5));
        assert_eq!(report.embedding_model, "test-model");

        // Canonical meta should be ingestion-complete + indexes-built.
        let raw = std::fs::read_to_string(
            report.canonical_path.join("_corpus_meta.json"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["ingestion_in_progress"], serde_json::Value::Bool(false));
        assert_eq!(v["indexes_built"], serde_json::Value::Bool(true));
        assert_eq!(v["provenance"], serde_json::Value::String("self_initiated".into()));
        assert_eq!(v["total_shards"], serde_json::Value::Number(5.into()));
        // Scope replayed from i1 (first non-None scope wins).
        assert_eq!(v["scope"]["filter_signature"], "abcdef");
        // Union of shards stamped onto canonical.
        let processed: Vec<u64> = v["processed_shards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap())
            .collect();
        assert_eq!(processed, vec![0u64, 1, 2, 3]);

        // Partition dirs are NOT removed by the recovery primitive —
        // caller decides cleanup separately.
        assert!(p1.exists());
        assert!(p2.exists());
    }

    #[tokio::test]
    async fn merge_partitions_into_canonical_refuses_when_canonical_exists() {
        // Belt-and-suspenders: even if the user invokes the recovery
        // primitive when canonical is already populated, we never
        // clobber.
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().join("foo");
        create_test_index(&canonical, 1).await;
        assert!(canonical.join("_corpus_meta.json").exists());

        // Also need at least one partition dir or discovery errors
        // first; we want to specifically exercise the canonical-
        // exists guard.
        let p1 = dir.path().join("foo-partition-node-aaaa");
        create_test_index(&p1, 1).await;

        let result = merge_partitions_into_canonical(dir.path(), "foo", None).await;
        assert!(result.is_err(), "must refuse when canonical already exists");

        // Canonical untouched.
        assert!(canonical.join("_corpus_meta.json").exists());
    }

    #[tokio::test]
    async fn merge_partitions_into_canonical_errors_when_no_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let result = merge_partitions_into_canonical(dir.path(), "nope", None).await;
        assert!(result.is_err(), "must error when nothing to merge");
    }

    #[tokio::test]
    async fn test_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source");
        create_test_index(&source_path, 20).await;

        let stats = index_stats(&source_path).await.unwrap();
        let chunk_size = stats.total_chunks / 3;
        let mut shard_paths = Vec::new();
        let mut start = stats.min_chunk_id;

        for i in 0..3 {
            let end = if i == 2 {
                stats.max_chunk_id
            } else {
                start + chunk_size
            };
            let shard_path = dir.path().join(format!("shard{i}"));
            extract_shard(
                &source_path,
                ChunkRange::new(start, end),
                &shard_path,
            )
            .await
            .unwrap();
            shard_paths.push(shard_path);
            start = end;
        }

        let merged_path = dir.path().join("merged");
        let merged_info = merge_shards(&shard_paths, &merged_path).await.unwrap();
        assert_eq!(merged_info.chunk_count, 20);

        // Search should work on merged index.
        let merged = CorpusIndex::open(&merged_path).await.unwrap();
        let emb = make_test_embedding(5.0);
        let results = merged.search(&emb, "", 5).await.unwrap();
        assert!(!results.is_empty());
    }
}
