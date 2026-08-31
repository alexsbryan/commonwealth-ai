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
            engine
                .open_index_transient(p)
                .await
                .expect("transient open");
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
        engine
            .open_index_transient(p)
            .await
            .expect("transient open");
    }
    assert_eq!(
        engine.index_cache_len(),
        1,
        "the sweep must neither evict the hot handle nor add the cold one"
    );
}

/// The same contract, reached BY CORPUS ID — the spelling walkers use.
///
/// `open_index_transient` existed for paths and was applied to the hourly
/// maintenance sweep. It had no by-id sibling, so every other walker reached
/// for `open_index_for_corpus`, which caches. Measured on the dev host
/// 2026-08-31: the newsworthy tick opened `wikipedia` through that wrapper to
/// stream it once, and the daemon then held 185 of its 256 file descriptors on
/// that corpus's `chunks.lance/_indices/*` for the life of the process. A
/// code-intel enrichment sharing the daemon died on `Too many open files
/// (os error 24)` and still reported exit 0.
///
/// Twice round the loop, because a walker runs on a timer and "does it grow
/// across ticks?" is the question that matters.
#[tokio::test]
async fn a_by_id_walker_admits_no_handles_to_the_query_cache() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _paths) = engine_with_corpora(dir.path(), 3).await;

    assert_eq!(
        engine.index_cache_len(),
        0,
        "precondition: nothing resident before the walker runs"
    );

    for _ in 0..2 {
        for i in 0..3 {
            engine
                .open_index_for_corpus_transient(&format!("resident_{i}"))
                .await
                .expect("transient open by id");
        }
    }

    assert_eq!(
        engine.index_cache_len(),
        0,
        "a by-id walker must never populate the query cache"
    );
}

/// The other half: the by-id QUERY wrapper still caches, and a walker that
/// follows it reuses the hot handle rather than evicting or duplicating it.
///
/// Without this, "the walker no longer caches" could be satisfied by breaking
/// `open_index_for_corpus` outright, which is the retrieval path for every
/// HTTP route, desktop command and chat turn on the box.
#[tokio::test]
async fn the_by_id_query_wrapper_still_caches_and_the_walker_reuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _paths) = engine_with_corpora(dir.path(), 2).await;

    engine
        .open_index_for_corpus("resident_0")
        .await
        .expect("query open by id");
    assert_eq!(
        engine.index_cache_len(),
        1,
        "a by-id query open must still admit its handle"
    );

    for i in 0..2 {
        engine
            .open_index_for_corpus_transient(&format!("resident_{i}"))
            .await
            .expect("transient open by id");
    }

    assert_eq!(
        engine.index_cache_len(),
        1,
        "the walker must neither evict the hot handle nor admit the cold one"
    );
}

/// STRUCTURAL, not remembered (ARCH §7): nothing in this crate may open a
/// corpus through the CACHING by-id wrapper.
///
/// The two tests above pin the engine's contract, but they cannot catch the
/// regression that actually happened — a walker reaching for
/// `open_index_for_corpus` because it is the obvious name. Every by-id open
/// inside `corpus-engine/src` today is a walker (delta application, the
/// newsworthy fold, the atlas strategies); none of them answer a query. The
/// query-path callers live in the crates above this one (mesh, desktop,
/// server, tools) and are untouched by this guard.
///
/// If a genuine query path is ever added HERE, this test is the place to
/// record why — add the exemption with its reason rather than deleting the
/// guard.
#[test]
fn no_walker_in_this_crate_opens_a_corpus_through_the_caching_wrapper() {
    fn walk(dir: &Path, hits: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, hits);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    // The call, not the identifier: doc comments and the
                    // definition itself name it legitimately.
                    if line.contains(".open_index_for_corpus(") {
                        hits.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
    }

    let mut hits = Vec::new();
    walk(Path::new("src"), &mut hits);
    assert!(
        hits.is_empty(),
        "these call the caching by-id wrapper from inside corpus-engine; a \
         one-shot or on-a-timer read must use `open_index_for_corpus_transient` \
         or it pins that corpus's LanceDB handles for the life of the process:\n  {}",
        hits.join("\n  ")
    );
}
