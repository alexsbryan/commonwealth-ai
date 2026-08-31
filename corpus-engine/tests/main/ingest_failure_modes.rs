// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let batch =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(titles), Arc::new(texts)]).unwrap();
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
    let batch =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(titles), Arc::new(texts)]).unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn write_recipe(recipes_dir: &Path, parquet_path: &Path, embedding_dimensions: usize) -> PathBuf {
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

    let engine = build_engine(always_failing_embed_fn(), recipes_dir, indexes_dir.clone());

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
/// The engine auto-adapts to whatever dimension count the model actually
/// returns, so a recipe that says 8-dim but gets a 4-dim model should
/// succeed and produce a usable index at the real dimension count.
#[tokio::test]
async fn ingest_adapts_to_actual_embedding_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);

    // Recipe says 8-dim but the embedder returns 4 — engine should adapt.
    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    let engine = build_engine(wrong_dim_embed_fn(), recipes_dir, indexes_dir.clone());

    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest should succeed with auto-adapted dimensions");

    assert!(result.chunks_created > 0, "should have produced chunks");
    // Index directory was created (not cleaned up as a ghost).
    assert!(indexes_dir.join("test_corpus").exists());
}

/// Ingest into a canonical directory another subsystem already owns.
///
/// Ingest fills `<index_dir>/<corpus>-partition-<node>/` and only
/// `finalise_solo_ingest` materialises the canonical `<corpus>/`. That
/// directory is a shared address — the enrichment sink, the
/// watched-folder machinery, the atlas builder and the SCIP reindexer
/// all write into it while ingest is still running — so promotion has
/// to merge into a non-empty destination rather than assume it owns
/// the path.
///
/// WHAT THIS TEST DOES AND DOES NOT PIN. It exercises the real
/// `ingest()` → `finalise_solo_ingest()` → `promote_single_shard()`
/// path and asserts three things end to end: the corpus reaches
/// canonical, every pre-existing sidecar survives, and BOTH corpus
/// surfaces resolve it afterwards.
///
/// That last assertion is the load-bearing one. `installed_indexes()`
/// (retrieval) enumerates directories and keys each one off its meta's
/// `corpus_id` — which a partition also declares — while
/// `open_index_for_corpus` (the reading surface) matches by directory
/// name. When promotion fails they disagree, and that disagreement is
/// the whole user-visible bug: retrieval cites chunks the reading desk
/// then cannot dereference. A test that checked only
/// `installed_indexes()` would have stayed green through the entire
/// incident.
///
/// It does NOT reproduce the historical refusal (note 79fdd04c).
/// Verified by running it against the pre-fix `sharding.rs`: it passes
/// there too. The old code refused only when the SAME entry name
/// existed on both sides, and a plain recipe ingest puts just
/// `_corpus_meta.json` and `chunks.lance` in the partition — both of
/// which promotion still refuses to merge by design. The production
/// failure needed `_enrichment_state.json` in the partition as well,
/// which only the watched-folder path (enrichment sink active) writes.
/// The refusal itself is pinned by the `promote_single_shard_*` tests
/// in `sharding.rs`, which do fail without the fix.
#[tokio::test]
async fn ingest_promotes_into_a_canonical_dir_another_subsystem_owns() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);
    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    // Lose the race on purpose: the canonical dir already holds the
    // sidecars other subsystems drop there mid-ingest.
    let canonical = indexes_dir.join("test_corpus");
    std::fs::create_dir_all(canonical.join("atlas")).unwrap();
    std::fs::write(
        canonical.join("_enrichment_state.json"),
        br#"{"status":"Running"}"#,
    )
    .unwrap();
    std::fs::write(canonical.join("_watched_folder_state.json"), br#"{}"#).unwrap();
    std::fs::write(canonical.join("atlas/.read_v2"), b"").unwrap();
    std::fs::write(canonical.join("scip_graph.db"), b"sqlite-stub").unwrap();

    let embed_fn: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
    let engine = build_engine(embed_fn, recipes_dir, indexes_dir.clone());

    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest itself must succeed");
    assert!(result.chunks_created > 0);

    // The corpus landed in canonical, not just its sidecars.
    assert!(
        canonical.join("_corpus_meta.json").exists(),
        "promotion must materialise the canonical meta",
    );
    assert!(canonical.join("chunks.lance").exists());

    // Every pre-existing sidecar survived — promotion must not have
    // bulldozed the directory it merged into.
    for sidecar in [
        "_enrichment_state.json",
        "_watched_folder_state.json",
        "scip_graph.db",
        "atlas/.read_v2",
    ] {
        assert!(
            canonical.join(sidecar).exists(),
            "{sidecar} must survive promotion",
        );
    }

    // No partition left stranded beside the canonical.
    let strays: Vec<_> = std::fs::read_dir(&indexes_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("test_corpus-partition-"))
        .collect();
    assert!(
        strays.is_empty(),
        "partition must be consumed, found stranded: {strays:?}",
    );

    // ── The divergence check ──
    // Retrieval and the reading surface resolve corpora by different
    // strategies. Asserting only the first is what let this ship.
    let installed = engine.installed_indexes().await.unwrap();
    assert!(
        installed.iter().any(|i| i.corpus_id == "test_corpus"),
        "retrieval must see the corpus",
    );
    let index = engine
        .open_index_for_corpus("test_corpus")
        .await
        .expect("the READING surface must resolve it too — this is the assertion that fails when the partition is stranded");
    assert!(index.info().await.unwrap().chunk_count > 0);
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
        msg.contains("Simulated mid-pipeline failure") || msg.contains("Embedding"),
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
    let embed_fn: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
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
    let working_embed: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
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

/// A stopped ingest is listed by `installed_indexes()` and REFUSED by
/// `usable_indexes()` — the two questions are different and must not share
/// one answer.
///
/// The state is not contrived. Measured on the dev host 2026-08-30: 41
/// corpora carry a meta, 41 pass the writer predicate, 38 pass the consumer
/// one. The three in the gap (`e2e-notebook`, `wikipedia-newsworthy`, a
/// folder-governance corpus) all sit at `committed_iter_pos: 0` with a
/// `chunks.lance` on disk and `indexes_built: false` — an ingest that
/// committed its chunks and died before `build_indexes()`. That is the shape
/// that made `corpus status` print `ready` for seven unsearchable corpora.
///
/// `installed_indexes()` is deliberately gated on `is_ingestion_complete`,
/// which asks "is a writer active right now" — the right question for the
/// resume paths and the wrong one for retrieval. Before `usable_indexes()`
/// existed, ~84 call sites each decided for themselves and exactly one
/// retrieval leg checked `indexes_built` (ARCH §10.6, one decider).
#[tokio::test]
async fn a_stopped_ingest_is_listed_but_not_usable() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);
    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    let working_embed: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
    let engine = build_engine(working_embed, recipes_dir, indexes_dir.clone());
    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest should succeed");

    // Positive control: while the corpus is whole, both surfaces agree.
    let listed = engine.installed_indexes().await.unwrap();
    let usable = engine.usable_indexes().await.unwrap();
    assert!(listed.iter().any(|i| i.corpus_id == "test_corpus"));
    assert!(
        usable.iter().any(|i| i.corpus_id == "test_corpus"),
        "a finished ingest must be usable — otherwise this test proves nothing"
    );

    // Now reproduce the stranded state: chunks committed, indexes never built.
    let meta_path = indexes_dir.join("test_corpus").join("_corpus_meta.json");
    let raw = std::fs::read_to_string(&meta_path).unwrap();
    let mut meta: serde_json::Value = serde_json::from_str(&raw).unwrap();
    meta["ingestion_in_progress"] = serde_json::json!(false);
    meta["indexes_built"] = serde_json::json!(false);
    std::fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).unwrap();

    let engine2 = build_engine(
        Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) })),
        dir.path().join("recipes"),
        indexes_dir,
    );
    let listed = engine2.installed_indexes().await.unwrap();
    let usable = engine2.usable_indexes().await.unwrap();

    assert!(
        listed.iter().any(|i| i.corpus_id == "test_corpus"),
        "no writer is active, so installed_indexes() must still list it"
    );
    assert!(
        !usable.iter().any(|i| i.corpus_id == "test_corpus"),
        "indexes_built is false, so usable_indexes() must refuse it — this is \
         the assertion that fails if the two questions are collapsed again"
    );
}

/// `ensure_empty_index` must materialise the index even when the index
/// DIRECTORY already exists without an index in it.
///
/// This is the real-world shape, not a contrived one: the generic
/// enrichment-progress sink writes `<index_dir>/<corpus_id>/
/// _enrichment_state.json` before the first ingest completes, so by the
/// time an all-unreadable folder reaches this path the directory is
/// already there. Branching on `path.exists()` sent that case to
/// `CorpusIndex::open`, which failed with
/// `IndexNotFound: Missing metadata at …/_corpus_meta.json` — precisely
/// the state `ensure_empty_index` exists to repair. Caught 2026-07-24 by
/// the desktop real-mode e2e harness, whose governance fixture (two .md
/// files under a `folder` corpus, which allows only pdf+txt) extracts
/// zero documents and lands here.
#[tokio::test]
async fn ensure_empty_index_succeeds_when_dir_exists_without_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_tiny_parquet(&parquet_path);
    let recipe_path = write_recipe(&recipes_dir, &parquet_path, 8);

    // Another subsystem got here first: the directory exists and holds a
    // sibling state file, but no `_corpus_meta.json`.
    let index_path = indexes_dir.join("test_corpus");
    std::fs::create_dir_all(&index_path).unwrap();
    std::fs::write(
        index_path.join("_enrichment_state.json"),
        br#"{"status":"Queued"}"#,
    )
    .unwrap();

    let embed_fn: EmbedFn = Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.1_f32; 8]) }));
    let engine = build_engine(embed_fn, recipes_dir, indexes_dir.clone());

    let result = engine
        .ensure_empty_index(&CorpusSpec::RecipePath(recipe_path))
        .await
        .expect("ensure_empty_index must repair a metadata-less directory, not fail on it");
    assert_eq!(result.chunks_created, 0);

    // The index is now valid on disk and visible to the rest of the system.
    assert!(index_path.join("_corpus_meta.json").exists());
    let installed = engine.installed_indexes().await.unwrap();
    assert!(
        installed.iter().any(|i| i.corpus_id == "test_corpus"),
        "an ensured-empty index must be listed as installed"
    );
    // The sibling file that caused the bug is untouched.
    assert!(index_path.join("_enrichment_state.json").exists());
}
