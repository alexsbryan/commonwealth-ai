// SPDX-License-Identifier: AGPL-3.0-or-later
//! Residency contract for the query-path index cache.
//!
//! `CorpusEngine::index_cache` is a query-path accelerator with NO
//! eviction: a handle admitted to it is resident for the life of the
//! process. That is the right trade for retrieval, which re-opens the
//! same few hot corpora thousands of times, and the wrong one for a
//! background walker that visits EVERY installed corpus on a timer —
//! the daemon's hourly `corpus_maintenance` sweep made every installed
//! LanceDB handle on the box permanently resident after one tick,
//! whether or not anyone ever queried that corpus
//! (`research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md`
//! §7.4 item 7).
//!
//! These tests pin the two halves of the fix:
//!   1. A query-path open still caches (nothing about retrieval changed).
//!   2. A transient open never admits a handle, no matter how many
//!      corpora the walker visits — while still being allowed to SERVE
//!      from a handle the query path already paid for.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::StringArray;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn};

fn make_tiny_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("title", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
    ]));
    let titles = StringArray::from(vec!["A", "B"]);
    let texts = StringArray::from(vec![
        "First entry with enough text to make a chunk.",
        "Second entry with enough text to make a chunk.",
    ]);
    let batch =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(titles), Arc::new(texts)]).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn write_recipe(recipes_dir: &Path, corpus_id: &str, parquet_path: &Path) -> PathBuf {
    let recipe_path = recipes_dir.join(format!("{corpus_id}.toml"));
    let parquet_str = parquet_path.to_string_lossy();
    let toml = format!(
        r#"
[corpus]
id = "{corpus_id}"
name = "Residency fixture {corpus_id}"
description = "index-cache residency test fixture"
license = "CC0"
mesh_sharing = false

[acquire]
type = "local_file"
path = "{parquet_str}"

[extract]
type = "parquet"
content_column = "text"
label_column = "title"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[index]
embedding_model = "test-mock"
embedding_dimensions = 8
"#
    );
    std::fs::write(&recipe_path, toml).unwrap();
    recipe_path
}

fn mock_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }))
}

/// Build an engine holding `n` tiny installed corpora and return it
/// alongside their index paths.
async fn engine_with_corpora(dir: &Path, n: usize) -> (CorpusEngine, Vec<PathBuf>) {
    let recipes_dir = dir.join("recipes");
    let indexes_dir = dir.join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    let parquet_path = dir.join("fixture.parquet");
    make_tiny_parquet(&parquet_path);

    let engine = CorpusEngine::new(recipes_dir.clone(), indexes_dir.clone(), mock_embed_fn())
        .with_embedding_model("test-mock");

    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        let corpus_id = format!("resident_{i}");
        let recipe = write_recipe(&recipes_dir, &corpus_id, &parquet_path);
        engine
            .ingest(&CorpusSpec::RecipePath(recipe), None)
            .await
            .expect("tiny fixture ingest");
        paths.push(indexes_dir.join(&corpus_id));
    }
    (engine, paths)
}

/// RED-FIRST (order mesh-scale-t0, item 7). A walker that visits every
/// installed corpus must not pin every handle resident. Before the fix
/// the maintenance sweep called `open_index`, so this assertion read
/// `3 == 0` after one simulated tick — and on a box with 1000 installed
/// corpora that is 1000 LanceDB handles resident forever after the
/// first hourly tick.
#[tokio::test]
async fn a_full_sweep_admits_no_handles_to_the_query_cache() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, paths) = engine_with_corpora(dir.path(), 3).await;

    // Baseline: ingest itself must not have populated the query cache,
    // or the number below would not be attributable to the sweep.
    assert_eq!(
        engine.index_cache_len(),
        0,
        "precondition: nothing resident before the sweep"
    );

    // One sweep tick, twice — a walker runs on a timer, so "does it
    // grow across ticks?" is the question that matters.
    for _ in 0..2 {
        for p in &paths {
            engine.open_index_transient(p).await.expect("transient open");
        }
    }

    assert_eq!(
        engine.index_cache_len(),
        0,
        "a background sweep may read through the query cache but must never populate it"
    );
}

/// The other half: the query path is unchanged. Without this, "the
/// sweep no longer caches" could be satisfied by breaking the cache
/// outright, which would cost retrieval a ~5s LanceDB re-open per
/// corpus per query on a large index.
#[tokio::test]
async fn the_query_path_still_caches_and_the_sweep_reuses_its_handles() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, paths) = engine_with_corpora(dir.path(), 2).await;

    engine.open_index(&paths[0]).await.expect("query open");
    assert_eq!(
        engine.index_cache_len(),
        1,
        "a query-path open must still admit its handle"
    );

    // The sweep visits both. It must serve the already-cached one from
    // the cache (free) and must not admit the other.
    for p in &paths {
        engine.open_index_transient(p).await.expect("transient open");
    }
    assert_eq!(
        engine.index_cache_len(),
        1,
        "the sweep must neither evict the hot handle nor add the cold one"
    );
}
