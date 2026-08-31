// SPDX-License-Identifier: AGPL-3.0-or-later
//! Public-API witness for the sharding contract.
//!
//! Inline tests in `src/sharding.rs` cover the per-op happy paths
//! (`extract_shard`, `merge_shards`, `index_stats`, `test_round_trip`)
//! and the merge-partition error cases. This file fills the genuine
//! e2e gaps in the public surface:
//!
//! 1. `append_partition_to_canonical` had no inline test at all.
//! 2. `merge_partitions_into_canonical` had only refusal/recovery
//!    tests — no happy-path round trip.
//! 3. Multi-shard search semantics — that a term unique to one shard
//!    is still findable after merging.
//!
//! Lives at the public-API layer (`corpus_engine::*` only) so a future
//! `sharding.rs` split is witnessed by the surface external callers
//! (commonwealth-knowledge::shard_manager, sovereign-cli::mesh_cmd,
//! sovereign-tools::catalog_ingest) actually depend on.

use std::path::Path;

use corpus_engine::index::{InsertChunk, InsertCodeMeta};
use corpus_engine::{
    append_partition_to_canonical, merge_partitions_into_canonical,
    sharding::{extract_shard, index_stats, merge_shards},
    ChunkRange, CorpusIndex,
};

const EMBED_DIM: usize = 8;

fn embedding(seed: f32) -> Vec<f32> {
    (0..EMBED_DIM).map(|i| seed + i as f32 * 0.1).collect()
}

fn chunk(content: &str, title: &str, content_hash: Option<&str>) -> InsertChunk {
    InsertChunk {
        content: content.into(),
        title: Some(title.into()),
        url: None,
        metadata: None,
        content_hash: content_hash.map(str::to_owned),
        source_doc_id: None,
        source_file: None,
        code: InsertCodeMeta::default(),
        unit_id: None,
    }
}

async fn build_index(path: &Path, corpus_id: &str, rows: &[(&str, &str, Option<&str>)]) {
    let index = CorpusIndex::create(
        path,
        corpus_id,
        "Test Corpus",
        "test-model",
        EMBED_DIM,
        true,
        "MIT",
    )
    .await
    .expect("create index");
    let payload: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, (content, title, hash))| (chunk(content, title, *hash), embedding(i as f32)))
        .collect();
    index.insert_batch(&payload).await.expect("insert_batch");
}

// ── append_partition_to_canonical ──────────────────────────────────────

#[tokio::test]
async fn append_creates_canonical_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("partition");
    let canonical = dir.path().join("canonical");

    build_index(
        &source,
        "legal",
        &[
            ("alpha body", "Alpha", Some("h-alpha")),
            ("bravo body", "Bravo", Some("h-bravo")),
            ("cha body", "Cha", Some("h-cha")),
        ],
    )
    .await;

    let report = append_partition_to_canonical(
        &source,
        &canonical,
        "legal",
        "Legal Corpus",
        "test-model",
        EMBED_DIM,
        true,
    )
    .await
    .expect("append");

    assert_eq!(report.chunks_inserted, 3);
    assert_eq!(report.chunks_deduped, 0);

    let merged = CorpusIndex::open(&canonical).await.unwrap();
    let info = merged.info().await.unwrap();
    assert_eq!(info.chunk_count, 3);
    assert_eq!(info.corpus_id, "legal");
}

#[tokio::test]
async fn append_dedupes_overlapping_content_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().join("canonical");
    let partition = dir.path().join("partition");

    // Seed canonical with two rows directly via append (the only public
    // way to populate a canonical without going through the engine
    // façade).
    let seed = dir.path().join("seed");
    build_index(
        &seed,
        "legal",
        &[
            ("alpha body", "Alpha", Some("h-alpha")),
            ("bravo body", "Bravo", Some("h-bravo")),
        ],
    )
    .await;
    append_partition_to_canonical(
        &seed,
        &canonical,
        "legal",
        "Legal Corpus",
        "test-model",
        EMBED_DIM,
        true,
    )
    .await
    .unwrap();

    // Partition shares one hash with canonical + adds one new.
    build_index(
        &partition,
        "legal",
        &[
            ("alpha body", "Alpha-dupe", Some("h-alpha")), // dupe
            ("delta body", "Delta", Some("h-delta")),      // new
        ],
    )
    .await;

    let report = append_partition_to_canonical(
        &partition,
        &canonical,
        "legal",
        "Legal Corpus",
        "test-model",
        EMBED_DIM,
        true,
    )
    .await
    .expect("append");

    assert_eq!(report.chunks_inserted, 1, "only h-delta should land");
    assert_eq!(report.chunks_deduped, 1, "h-alpha dupe rejected");

    let merged = CorpusIndex::open(&canonical).await.unwrap();
    let info = merged.info().await.unwrap();
    assert_eq!(info.chunk_count, 3, "alpha + bravo + delta");
}

#[tokio::test]
async fn append_rejects_dimension_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("partition");
    let canonical = dir.path().join("canonical");

    build_index(&source, "legal", &[("a", "A", None)]).await;

    // Stated dim 16 differs from the source's 8.
    let err = append_partition_to_canonical(
        &source,
        &canonical,
        "legal",
        "Legal Corpus",
        "test-model",
        16,
        true,
    )
    .await
    .expect_err("dim mismatch must error");

    let msg = format!("{err}");
    assert!(
        msg.contains("dim") && msg.contains("mismatches"),
        "error should call out dim mismatch, got: {msg}",
    );
}

// ── merge_partitions_into_canonical ────────────────────────────────────

#[tokio::test]
async fn merge_partitions_happy_path() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path();

    // Discovery rule: <corpus>-partition-*/ siblings of the canonical.
    for (i, rows) in [
        vec![("alpha", "Alpha", Some("h-a"))],
        vec![("bravo", "Bravo", Some("h-b"))],
        vec![("cha", "Cha", Some("h-c")), ("delta", "Delta", Some("h-d"))],
    ]
    .into_iter()
    .enumerate()
    {
        let p = index_dir.join(format!("legal-partition-{i}"));
        build_index(&p, "legal", &rows).await;
    }

    let report = merge_partitions_into_canonical(index_dir, "legal", None)
        .await
        .expect("merge");

    assert_eq!(report.chunks_input, 4);
    let canonical = CorpusIndex::open(&index_dir.join("legal")).await.unwrap();
    let info = canonical.info().await.unwrap();
    assert_eq!(info.chunk_count, 4);
    assert_eq!(info.corpus_id, "legal");
}

// ── extract_shard + merge_shards search semantics ──────────────────────

#[tokio::test]
async fn merged_search_finds_term_unique_to_one_shard() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");

    // 9 chunks; the term "needle" appears only at chunk index 7.
    let mut rows: Vec<(String, String, Option<String>)> = (0..9)
        .map(|i| (format!("filler body {i}"), format!("Title {i}"), None))
        .collect();
    rows[7].0 = "the needle is here".into();
    let row_refs: Vec<(&str, &str, Option<&str>)> = rows
        .iter()
        .map(|(c, t, h)| (c.as_str(), t.as_str(), h.as_deref()))
        .collect();
    build_index(&source, "test", &row_refs).await;

    let stats = index_stats(&source).await.unwrap();
    assert_eq!(stats.total_chunks, 9);

    // Split into 3 shards by chunk-id range.
    let chunk_size = stats.total_chunks / 3;
    let mut start = stats.min_chunk_id;
    let mut shards = Vec::new();
    for i in 0..3 {
        let end = if i == 2 {
            stats.max_chunk_id
        } else {
            start + chunk_size
        };
        let p = dir.path().join(format!("shard-{i}"));
        extract_shard(&source, ChunkRange::new(start, end), &p)
            .await
            .unwrap();
        shards.push(p);
        start = end;
    }

    let merged_path = dir.path().join("merged");
    let info = merge_shards(&shards, &merged_path).await.unwrap();
    assert_eq!(info.chunk_count, 9);

    let merged = CorpusIndex::open(&merged_path).await.unwrap();
    let hits = merged.search(&embedding(7.0), "needle", 5).await.unwrap();
    assert!(
        hits.iter().any(|h| h.content.contains("needle")),
        "merged search must surface the unique term from shard-2",
    );
}
