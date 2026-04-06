//! Shard operations: the three-operation contract between corpus-engine
//! and Commonwealth.
//!
//! - `index_stats`: report chunk ID range, count, and size
//! - `extract_shard`: extract a subset of an index into a new directory
//! - `merge_shards`: merge multiple shard directories into a single index

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{Int64Array, RecordBatch};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::types::{ChunkRange, IndexInfo, IndexStats, ShardInfo};

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
/// Chunks are renumbered to form a contiguous ID space.
pub async fn merge_shards(
    shard_paths: &[PathBuf],
    output_path: &Path,
) -> Result<IndexInfo> {
    if shard_paths.is_empty() {
        return Err(Error::NoShardsFound("no shard paths provided".into()));
    }

    // Read metadata from first shard.
    let first = CorpusIndex::open(&shard_paths[0]).await?;
    let first_info = first.info().await?;
    drop(first);

    // Create output index.
    let merged = CorpusIndex::create(
        output_path,
        &first_info.corpus_id,
        &first_info.corpus_name,
        &first_info.embedding_model,
        first_info.embedding_dimensions,
        first_info.mesh_sharing,
        "",
    )
    .await?;

    let dim = first_info.embedding_dimensions;
    let mut next_id: i64 = 1;

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

            // Renumber IDs.
            let new_ids: Vec<i64> = (0..num_rows)
                .map(|i| {
                    let id = next_id + i as i64;
                    id
                })
                .collect();
            next_id += num_rows as i64;

            // Rebuild batch with new IDs.
            let schema = crate::index::corpus_schema(dim);
            let new_batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int64Array::from(new_ids)),
                    batch.column_by_name("content").unwrap().clone(),
                    batch.column_by_name("title").unwrap().clone(),
                    batch.column_by_name("url").unwrap().clone(),
                    batch.column_by_name("embedding").unwrap().clone(),
                    batch.column_by_name("metadata").unwrap().clone(),
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

    merged.info().await
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
