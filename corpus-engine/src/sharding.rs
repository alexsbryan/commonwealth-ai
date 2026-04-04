//! Shard operations: the three-operation contract between corpus-engine
//! and Commonwealth.
//!
//! - `index_stats`: report chunk ID range, count, and size
//! - `extract_shard`: extract a subset of an index into a new file
//! - `merge_shards`: merge multiple shard files into a single index

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::types::{ChunkRange, IndexInfo, IndexStats, ShardInfo};

/// Report chunk ID range, count, and size for an index.
pub fn index_stats(index_path: &Path) -> Result<IndexStats> {
    let index = CorpusIndex::open(index_path)?;
    let info = index.info()?;

    let db = index.connection();
    let (min_id, max_id): (u64, u64) = db.query_row(
        "SELECT COALESCE(MIN(id), 0), COALESCE(MAX(id) + 1, 0) FROM chunks",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let file_size = std::fs::metadata(index_path)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(IndexStats {
        corpus_id: info.corpus_id,
        total_chunks: info.chunk_count,
        min_chunk_id: min_id,
        max_chunk_id: max_id,
        index_size_bytes: file_size,
    })
}

/// Extract a subset of an existing index into a new file.
/// Output contains only chunks with IDs in the given range.
/// Embeddings, FTS entries, and metadata are preserved.
pub fn extract_shard(
    source_path: &Path,
    chunk_range: ChunkRange,
    output_path: &Path,
) -> Result<ShardInfo> {
    let source = CorpusIndex::open(source_path)?;
    let source_info = source.info()?;

    // Create the shard index with the same metadata.
    let mut shard = CorpusIndex::create(
        output_path,
        &source_info.corpus_id,
        &source_info.corpus_name,
        &source_info.embedding_model,
        source_info.embedding_dimensions,
        source_info.mesh_sharing,
        "",
    )?;

    // Mark as shard in metadata.
    shard.set_meta("is_shard", "true")?;
    shard.set_meta("chunk_range_start", &chunk_range.start_id.to_string())?;
    shard.set_meta("chunk_range_end", &chunk_range.end_id.to_string())?;

    // Copy chunks in the range from source to shard.
    let source_db = source.connection();
    let mut stmt = source_db.prepare(
        "SELECT id, content, title, url, embedding, metadata FROM chunks \
         WHERE id >= ?1 AND id < ?2 ORDER BY id",
    )?;

    let mut rows = stmt.query(params![chunk_range.start_id, chunk_range.end_id])?;
    let mut chunk_count = 0u64;

    let shard_db = shard.connection();
    shard_db.execute_batch("BEGIN")?;

    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let content: String = row.get(1)?;
        let title: Option<String> = row.get(2)?;
        let url: Option<String> = row.get(3)?;
        let embedding: Vec<u8> = row.get(4)?;
        let metadata: Option<String> = row.get(5)?;

        shard_db.execute(
            "INSERT INTO chunks (id, content, title, url, embedding, metadata) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, content, title, url, embedding, metadata],
        )?;

        // FTS5 insert.
        shard_db.execute(
            "INSERT INTO chunks_fts(rowid, content, title) VALUES (?1, ?2, ?3)",
            params![id, content, title],
        )?;

        // Vec insert (if available).
        let _ = shard_db.execute(
            "INSERT INTO chunks_vec(rowid, embedding) VALUES (?1, ?2)",
            params![id, embedding],
        );

        chunk_count += 1;
    }

    shard_db.execute_batch("COMMIT")?;

    let file_size = std::fs::metadata(output_path)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(ShardInfo {
        path: output_path.to_path_buf(),
        chunk_range,
        chunk_count,
        size_bytes: file_size,
    })
}

/// Merge multiple shard files into a single index.
/// Chunks are renumbered to form a contiguous ID space.
/// Embeddings and FTS entries are rebuilt for the merged set.
pub fn merge_shards(
    shard_paths: &[PathBuf],
    output_path: &Path,
) -> Result<IndexInfo> {
    if shard_paths.is_empty() {
        return Err(Error::NoShardsFound("no shard paths provided".into()));
    }

    // Read metadata from first shard.
    let first = CorpusIndex::open(&shard_paths[0])?;
    let first_info = first.info()?;
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
    )?;

    let merged_db = merged.connection();
    merged_db.execute_batch("BEGIN")?;

    let mut next_id: i64 = 1;

    for shard_path in shard_paths {
        let shard = CorpusIndex::open(shard_path)?;
        let shard_db = shard.connection();

        let mut stmt = shard_db.prepare(
            "SELECT content, title, url, embedding, metadata FROM chunks ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let content: String = row.get(0)?;
            let title: Option<String> = row.get(1)?;
            let url: Option<String> = row.get(2)?;
            let embedding: Vec<u8> = row.get(3)?;
            let metadata: Option<String> = row.get(4)?;

            merged_db.execute(
                "INSERT INTO chunks (id, content, title, url, embedding, metadata) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![next_id, content, title, url, embedding, metadata],
            )?;

            merged_db.execute(
                "INSERT INTO chunks_fts(rowid, content, title) VALUES (?1, ?2, ?3)",
                params![next_id, content, title],
            )?;

            let _ = merged_db.execute(
                "INSERT INTO chunks_vec(rowid, embedding) VALUES (?1, ?2)",
                params![next_id, embedding],
            );

            next_id += 1;
        }
    }

    merged_db.execute_batch("COMMIT")?;

    merged.info()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::InsertChunk;

    fn make_test_embedding(seed: f32) -> Vec<f32> {
        (0..8).map(|i| seed + i as f32 * 0.1).collect()
    }

    fn create_test_index(path: &Path, chunk_count: u64) -> CorpusIndex {
        let mut index = CorpusIndex::create(
            path, "test", "Test Corpus", "test-model", 8, true, "MIT",
        )
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
        index.insert_batch(&chunks).unwrap();
        index
    }

    #[test]
    fn test_index_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        create_test_index(&path, 10);

        let stats = index_stats(&path).unwrap();
        assert_eq!(stats.corpus_id, "test");
        assert_eq!(stats.total_chunks, 10);
        assert!(stats.min_chunk_id >= 1);
        assert!(stats.max_chunk_id > stats.min_chunk_id);
        assert!(stats.index_size_bytes > 0);
    }

    #[test]
    fn test_extract_shard() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.db");
        create_test_index(&source_path, 10);

        // Get stats to know the ID range.
        let stats = index_stats(&source_path).unwrap();

        // Extract a shard of the first 5 chunks.
        let mid = stats.min_chunk_id + 5;
        let shard_path = dir.path().join("shard.db");
        let shard_info = extract_shard(
            &source_path,
            ChunkRange::new(stats.min_chunk_id, mid),
            &shard_path,
        )
        .unwrap();

        assert_eq!(shard_info.chunk_count, 5);
        assert!(shard_info.size_bytes > 0);

        // Verify the shard is searchable.
        let shard = CorpusIndex::open(&shard_path).unwrap();
        let info = shard.info().unwrap();
        assert_eq!(info.chunk_count, 5);
        assert!(info.is_shard);
    }

    #[test]
    fn test_merge_shards() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.db");
        create_test_index(&source_path, 10);

        let stats = index_stats(&source_path).unwrap();
        let mid = stats.min_chunk_id + 5;

        // Extract two shards.
        let shard1_path = dir.path().join("shard1.db");
        let shard2_path = dir.path().join("shard2.db");
        extract_shard(
            &source_path,
            ChunkRange::new(stats.min_chunk_id, mid),
            &shard1_path,
        )
        .unwrap();
        extract_shard(
            &source_path,
            ChunkRange::new(mid, stats.max_chunk_id),
            &shard2_path,
        )
        .unwrap();

        // Merge them.
        let merged_path = dir.path().join("merged.db");
        let merged_info = merge_shards(
            &[shard1_path, shard2_path],
            &merged_path,
        )
        .unwrap();

        assert_eq!(merged_info.chunk_count, 10);
        assert!(!merged_info.is_shard);
        assert_eq!(merged_info.corpus_id, "test");
    }

    #[test]
    fn test_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.db");
        create_test_index(&source_path, 20);

        let stats = index_stats(&source_path).unwrap();

        // Extract 3 shards.
        let chunk_size = stats.total_chunks / 3;
        let mut shard_paths = Vec::new();
        let mut start = stats.min_chunk_id;

        for i in 0..3 {
            let end = if i == 2 {
                stats.max_chunk_id
            } else {
                start + chunk_size
            };
            let shard_path = dir.path().join(format!("shard{i}.db"));
            extract_shard(
                &source_path,
                ChunkRange::new(start, end),
                &shard_path,
            )
            .unwrap();
            shard_paths.push(shard_path);
            start = end;
        }

        // Merge back.
        let merged_path = dir.path().join("merged.db");
        let merged_info = merge_shards(&shard_paths, &merged_path).unwrap();

        assert_eq!(merged_info.chunk_count, 20);

        // Search should work on merged index.
        let merged = CorpusIndex::open(&merged_path).unwrap();
        let results = merged.search(&[], "chunk", 5).unwrap();
        assert!(!results.is_empty());
    }
}
