//! Regression tests for the ingest pipeline's failure modes.
//!
//! These tests guard the bug where the desktop's `EmbeddedLlamaCpp::embed`
//! returned `NotImplemented`, the engine happily called `CorpusIndex::create`
//! anyway, the embed call inside the chunk loop returned an error, and
//! the user-visible UI ended up showing the corpus as "indexed" because
//! `installed_indexes()` scanned the half-built directory and found a
//! valid `_corpus_meta.json`.
//!
//! After the fix:
//! 1. `CorpusEngine::ingest` pre-flights the embed function before
//!    creating any on-disk state.
//! 2. If any later step fails, the partial index directory is removed
//!    so subsequent `installed_indexes()` calls don't see ghost installs.
//! 3. A pipeline that produces zero chunks fails explicitly instead of
//!    quietly creating an empty index.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use arrow::array::StringArray;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, Error};

// ─── Fixtures ────────────────────────────────────────────────

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
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(titles), Arc::new(texts)],
    )
    .unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn make_empty_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("title", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
    ]));
    let titles: StringArray = StringArray::from(Vec::<&str>::new());
    let texts: StringArray = StringArray::from(Vec::<&str>::new());
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(titles), Arc::new(texts)],
    )
    .unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn write_recipe(
    recipes_dir: &Path,
    parquet_path: &Path,
    embedding_dimensions: usize,
) -> PathBuf {
    let recipe_path = recipes_dir.join("test_corpus.toml");
    let parquet_str = parquet_path.to_string_lossy();
    let toml = format!(
        r#"
[corpus]
id = "test_corpus"
name = "Test Corpus"
description = "Failure-mode test fixture"
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
embedding_dimensions = {embedding_dimensions}
"#
    );
    std::fs::write(&recipe_path, toml).unwrap();
    recipe_path
}

/// Embedder that always errors. Simulates the desktop's broken
/// `EmbeddedLlamaCpp::embed` that returned `NotImplemented`.
fn always_failing_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| {
        Box::pin(async {
            Err(Error::Embed(
                "Embedding not available yet (Phase 6)".to_string(),
            ))
        })
    })
}

/// Embedder that returns the wrong number of dimensions.
fn wrong_dim_embed_fn() -> EmbedFn {
    Arc::new(|_text: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }))
}

/// Embedder that succeeds the pre-flight ("probe") call but fails on
/// the very next call. Used to verify the cleanup-on-failure path
/// covers errors that happen mid-pipeline, not just at the start.
fn fails_after_first_call_embed_fn() -> EmbedFn {
    let counter = Arc::new(AtomicUsize::new(0));
    Arc::new(move |_text: &str| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if n == 0 {
                Ok(vec![0.1_f32; 8])
            } else {
                Err(Error::Embed("Simulated mid-pipeline failure".into()))
            }
        })
    })
}

fn build_engine(embed: EmbedFn, recipes: PathBuf, indexes: PathBuf) -> CorpusEngine {
    CorpusEngine::new(recipes, indexes, embed).with_embedding_model("test-mock")
}

// ─── Tests ───────────────────────────────────────────────────

/// The exact scenario the user hit on the desktop: the configured
/// embed function returns `NotImplemented`, ingest must fail fast,
/// and no on-disk state may be created. Subsequent
/// `installed_indexes()` calls must NOT see the corpus.
#[tokio::test]
async fn ingest_fails_fast_when_embed_function_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);

    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    let engine = build_engine(
        always_failing_embed_fn(),
        recipes_dir,
        indexes_dir.clone(),
    );

    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await;

    let err = result.expect_err("ingest should fail when embed function errors");
    let msg = err.to_string();
    assert!(
        msg.contains("not available") || msg.contains("Embedding"),
        "error should explain the embed problem, got: {msg}"
    );

    // Critical: no half-built index directory survives the failure.
    let ghost = indexes_dir.join("test_corpus");
    assert!(
        !ghost.exists(),
        "no ghost index directory should remain after a failed install, found {}",
        ghost.display()
    );

    // And `installed_indexes()` reports no installed corpora.
    let installed = engine.installed_indexes().await.unwrap();
    assert!(
        !installed.iter().any(|i| i.corpus_id == "test_corpus"),
        "failed install must not appear in installed_indexes()"
    );
}

/// If the embed function returns the wrong number of dimensions, the
/// engine must reject the install up front rather than producing an
/// index with mismatched embeddings that breaks search later.
#[tokio::test]
async fn ingest_rejects_dimension_mismatch_in_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);

    // Recipe asks for 8-dim embeddings; the embedder returns 4.
    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    let engine = build_engine(
        wrong_dim_embed_fn(),
        recipes_dir,
        indexes_dir.clone(),
    );

    let err = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect_err("ingest should reject dimension mismatch");
    let msg = err.to_string();
    assert!(
        msg.contains("dimensions") || msg.contains("dimension"),
        "error should mention dimension mismatch, got: {msg}"
    );

    // No ghost directory.
    assert!(!indexes_dir.join("test_corpus").exists());
}

/// The cleanup path. The pre-flight succeeds, but the embed function
/// blows up partway through the chunk loop. We must remove the
/// partial index directory before propagating the error.
#[tokio::test]
async fn ingest_cleans_up_partial_index_when_pipeline_fails_midway() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);

    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    let engine = build_engine(
        fails_after_first_call_embed_fn(),
        recipes_dir,
        indexes_dir.clone(),
    );

    let err = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect_err("ingest should fail when embed errors mid-loop");
    let msg = err.to_string();
    assert!(
        msg.contains("Simulated mid-pipeline failure")
            || msg.contains("Embedding"),
        "error should describe the mid-pipeline failure, got: {msg}"
    );

    // The cleanup pass must have removed the half-built directory
    // even though it was successfully created earlier in the run.
    let ghost = indexes_dir.join("test_corpus");
    assert!(
        !ghost.exists(),
        "partial index directory should be cleaned up on mid-pipeline failure"
    );

    let installed = engine.installed_indexes().await.unwrap();
    assert!(
        !installed.iter().any(|i| i.corpus_id == "test_corpus"),
        "failed install must not appear in installed_indexes()"
    );
}

/// An empty source produces zero chunks. We treat this as an explicit
/// failure rather than silently creating an empty index — the user
/// would otherwise see "installed" with no way to actually search.
#[tokio::test]
async fn ingest_fails_when_no_chunks_are_produced() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("empty.parquet");
    make_empty_parquet(&parquet_path);

    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    // A working embedder — the failure here is upstream of embedding.
    let embed_fn: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
    let engine = build_engine(embed_fn, recipes_dir, indexes_dir.clone());

    let err = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect_err("empty source should produce an error");
    let msg = err.to_string();
    assert!(
        msg.contains("zero chunks") || msg.contains("empty"),
        "error should explain that zero chunks were produced, got: {msg}"
    );

    // No ghost install.
    assert!(!indexes_dir.join("test_corpus").exists());
}

/// Re-installing a previously-failed corpus must work. This catches the
/// case where cleanup leaves stray files behind that block re-install.
#[tokio::test]
async fn reinstalling_after_failure_works() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);
    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    // First attempt: failing embedder.
    let engine_bad = build_engine(
        always_failing_embed_fn(),
        recipes_dir.clone(),
        indexes_dir.clone(),
    );
    let _ = engine_bad
        .ingest(&CorpusSpec::RecipePath(recipe_path.clone()), None)
        .await;
    drop(engine_bad);

    // The first attempt left no on-disk state (preflight cleanup).
    assert!(!indexes_dir.join("test_corpus").exists());

    // Second attempt: working embedder, same recipe.
    let working_embed: EmbedFn =
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
    let engine_good = build_engine(working_embed, recipes_dir, indexes_dir.clone());
    let result = engine_good
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("re-install with a working embedder should succeed");
    assert!(result.chunks_created >= 2);

    let installed = engine_good.installed_indexes().await.unwrap();
    assert!(
        installed.iter().any(|i| i.corpus_id == "test_corpus"),
        "successful re-install should appear in installed_indexes()"
    );
}
