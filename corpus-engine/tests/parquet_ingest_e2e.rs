// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end ingest tests for the SEP-style pipeline:
//!   parquet source → Parquet extractor → paragraph chunker
//!   → mock embed → LanceDB → optional enrichment via mock inference.
//!
//! These tests guard the architectural fix where corpus installs were
//! routed through the legacy `CorpusManager` (which couldn't find
//! 'wikipedia' in its registry) instead of through `corpus_engine`.
//! They exercise the same code path the desktop install button now
//! takes, so a regression here means the user-visible install flow is
//! broken.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::StringArray;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, InferenceFn};

// ─── Fixtures ────────────────────────────────────────────────

/// Build a tiny SEP-shaped parquet file at `path`.
/// Two distinct entries so the relationship-extraction phase has
/// at least one cross-entry candidate pair to evaluate.
fn make_sep_like_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("title", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
    ]));

    let titles = StringArray::from(vec![
        "Compatibilism",
        "Compatibilism",
        "Hard Incompatibilism",
        "Hard Incompatibilism",
    ]);
    // Each passage must be ≥80 words so the eligibility filter passes.
    let texts = StringArray::from(vec![
        "Compatibilists hold that free will is consistent with determinism. \
         The central compatibilist thesis is that freedom of action requires only that \
         agents act in accordance with their own desires and values, without external \
         constraint or compulsion. Contemporary compatibilists have developed sophisticated \
         accounts that distinguish between the ability to act otherwise and the kind of \
         freedom required for moral responsibility. The hierarchical account holds that \
         an agent acts freely when their effective will aligns with higher-order desires. \
         Critics argue that this analysis fails to capture intuitions about origination, \
         but compatibilists respond that such intuitions reflect confused thinking about \
         what freedom actually requires for genuine agency.",
        "Frankfurt cases are widely cited as support for the compatibilist view \
         that moral responsibility does not require alternative possibilities. \
         In these cases an agent is unable to do otherwise due to a counterfactual \
         intervener, yet we standardly judge the agent responsible for their action. \
         Compatibilists argue this shows that the ability to do otherwise is not \
         a necessary condition for moral responsibility. The deeper question is whether \
         such cases successfully isolate the relevant condition or whether they smuggle \
         in assumptions about the causal history of action that beg the question \
         against incompatibilist intuitions about what responsibility ultimately requires.",
        "Hard incompatibilists maintain that neither determinism nor indeterminism \
         is compatible with the sort of free will required for moral responsibility. \
         Even if our actions are causally undetermined at the quantum level, this \
         randomness cannot underwrite the kind of control that genuine responsibility \
         demands. The hard incompatibilist position challenges both libertarianism and \
         compatibilism, holding that our practices of reactive attitudes and retributive \
         punishment are founded on a kind of agency that agents do not possess. \
         This conclusion has significant implications for criminal justice and moral \
         psychology, though some argue that forward-looking responsibility practices can \
         be preserved even if backward-looking retributivism cannot be justified.",
        "Pereboom maintains that even if our actions are determined, we lack \
         the control necessary for genuine moral responsibility of the basic desert kind. \
         His four-case argument proceeds by introducing a series of cases beginning with \
         direct neuroscientific manipulation and moving toward ordinary causal determinism, \
         arguing that the relevant differences between cases do not ground a principled \
         distinction in attributions of responsibility. The argument concludes that since \
         we would not hold agents responsible in the manipulation cases, we should not \
         hold them responsible in the ordinary case either. Compatibilists dispute whether \
         the cases are genuinely analogous, particularly regarding the role of the \
         agent's own character and values in producing the action.",
    ]);

    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(titles), Arc::new(texts)])
        .expect("build record batch");

    let file = std::fs::File::create(path).expect("create parquet file");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("arrow writer");
    writer.write(&batch).expect("write batch");
    writer.close().expect("close writer");
}

/// Mock embedder: deterministic 8-dim vector keyed off text length.
/// All embeddings will be similar enough to pair up during relationship
/// extraction without needing a real model.
fn mock_embed_fn() -> EmbedFn {
    Arc::new(|text: &str| {
        let len = text.len() as f32;
        let v: Vec<f32> = (0..8).map(|i| (len + i as f32) / 100.0).collect();
        Box::pin(async move { Ok(v) })
    })
}

/// Mock inference for the field model enrichment pipeline.
/// Returns canned JSON responses for skeleton extraction, cluster
/// labeling, fault line detection, and open question prompts.
fn mock_inference_fn() -> InferenceFn {
    Arc::new(|prompt: &str, _schema: Option<&serde_json::Value>| {
        let response = if prompt.contains("structure of philosophical debate")
            || prompt.contains("introductory passages")
        {
            // Skeleton extraction prompt — return one question with one position.
            r#"[{
                "passage_index": 0,
                "canonical_question": "Is free will compatible with determinism?",
                "question_type": "conceptual",
                "positions": [{
                    "name": "Compatibilism",
                    "claim": "Free will is compatible with determinism",
                    "status": "majority",
                    "proponents": ["Frankfurt"]
                }]
            }]"#
            .to_string()
        } else if prompt.contains("semantically similar") {
            // Cluster labeling prompt.
            r#"{"topic": "compatibilism", "position_name": "Compatibilism", "is_argumentative": true, "is_objection": false, "is_open_question": false, "is_coherent": true}"#.to_string()
        } else if prompt.contains("crux") || prompt.contains("dialogue") {
            // Fault line detection prompt.
            r#"{"crux": "Whether alternative possibilities are required", "confidence": 0.85, "resolution_condition": null}"#.to_string()
        } else if prompt.contains("unresolved") || prompt.contains("open question") {
            // Open question prompt.
            r#"{"question": "What explains the force of manipulation arguments?", "why_unresolved": "No consensus"}"#.to_string()
        } else if prompt.contains("Summarize the core claim") {
            // Discovered position description.
            "Free will requires agent causation".to_string()
        } else {
            // Fallback — return empty JSON array.
            "[]".to_string()
        };
        Box::pin(async move { Ok(response) })
    })
}

/// Write a recipe TOML to disk pointing at the given local parquet
/// file. The recipe form is the same one a user would author by hand
/// for a custom corpus.
fn write_recipe_toml(recipes_dir: &Path, parquet_path: &Path, enable_enrichment: bool) -> PathBuf {
    let recipe_path = recipes_dir.join("test_corpus.toml");
    let parquet_str = parquet_path.to_string_lossy();
    let enrichment_block = if enable_enrichment {
        r#"
[enrichment]
enabled = true
type = "field_model"
domain = "philosophy"
prompt_version = "1.0.0"
"#
    } else {
        ""
    };

    let toml = format!(
        r#"
[corpus]
id = "test_corpus"
name = "Test Corpus"
description = "A small parquet fixture for ingest e2e tests"
license = "CC0"
mesh_sharing = false
size_compressed_gb = 0.0
size_indexed_gb = 0.0

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
{enrichment_block}
"#
    );

    std::fs::write(&recipe_path, toml).expect("write recipe");
    recipe_path
}

fn build_engine(recipes_dir: PathBuf, indexes_dir: PathBuf) -> CorpusEngine {
    CorpusEngine::new(recipes_dir, indexes_dir, mock_embed_fn())
        .with_embedding_model("test-mock")
        .with_inference_fn(mock_inference_fn())
}

// ─── Tests ───────────────────────────────────────────────────

/// The non-enrichment ingest path. Mirrors what happens when a user
/// installs a plain corpus like Wikipedia or Stack Exchange: parquet →
/// chunks → embeddings → LanceDB. No claims table should be created.
#[tokio::test]
async fn parquet_ingest_creates_searchable_index() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("sep-mini.parquet");
    make_sep_like_parquet(&parquet_path);

    let recipe_path = write_recipe_toml(&recipes_dir, &parquet_path, false);
    let engine = build_engine(recipes_dir, indexes_dir.clone());

    // Drive the same pipeline the desktop install command drives.
    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest should succeed");

    assert_eq!(result.corpus_id, "test_corpus");
    assert!(
        result.chunks_created >= 4,
        "expected at least one chunk per parquet row, got {}",
        result.chunks_created
    );
    assert!(result.index_size_bytes > 0);

    // The corpus should appear in the on-disk index listing.
    let installed = engine.installed_indexes().await.unwrap();
    let entry = installed
        .iter()
        .find(|i| i.corpus_id == "test_corpus")
        .expect("ingested corpus should be in installed_indexes()");
    assert_eq!(entry.embedding_dimensions, 8);
    assert_eq!(entry.embedding_model, "test-mock");
    assert!(!entry.is_shard);
    assert!(entry.chunk_count >= 4);

    // The index should open and be searchable.
    let index = engine.open_index_for_corpus("test_corpus").await.unwrap();

    // A vector search with a deterministic query embedding should
    // return at least one result. We use the embed_fn directly so the
    // test stays self-contained.
    let query_embedding = engine.embed("compatibilism").await.unwrap();
    let results = index
        .search(&query_embedding, "compatibilism", 5)
        .await
        .unwrap();
    assert!(
        !results.is_empty(),
        "search on a populated index should return at least one chunk"
    );
}

/// The enrichment-enabled ingest path. This is the SEP demo target:
/// parquet → chunks → embeddings → LanceDB → field model enrichment.
/// Verifies the engine invokes the `InferenceFn` for skeleton extraction
/// and cluster labeling, and writes the field_skeleton.json artifact.
#[tokio::test]
async fn parquet_ingest_with_enrichment_creates_field_model() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("sep-mini.parquet");
    make_sep_like_parquet(&parquet_path);

    let recipe_path = write_recipe_toml(&recipes_dir, &parquet_path, true);
    let engine = build_engine(recipes_dir, indexes_dir.clone());

    let result = engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("enriched ingest should succeed");

    assert!(result.chunks_created >= 4);

    // The field skeleton should exist and contain the mock-extracted data.
    let index = engine.open_index_for_corpus("test_corpus").await.unwrap();
    let skeleton = index
        .load_field_skeleton()
        .expect("load_field_skeleton should not error")
        .expect("field_skeleton.json should exist after enrichment");

    assert_eq!(skeleton.schema_version, 1);
    assert_eq!(skeleton.corpus_id, "test_corpus");
    assert_eq!(skeleton.domain_id, "philosophy");
    assert!(!skeleton.generated_at.is_empty());

    // The mock inference returns a question about free will with a
    // Compatibilism position for every batch of overview chunks.
    // With 4 chunks batched by 4, we get 1 batch → 1 question.
    assert!(
        !skeleton.canonical_questions.is_empty(),
        "skeleton should have at least one canonical question"
    );

    let q = &skeleton.canonical_questions[0];
    assert!(
        q.question.contains("free will"),
        "question should mention free will, got: {}",
        q.question
    );

    assert!(
        !q.positions.is_empty(),
        "question should have at least one position"
    );
    let pos = &q.positions[0];
    assert_eq!(pos.name, "Compatibilism");
    assert_eq!(pos.status, "majority");
    assert_eq!(pos.source, "skeleton");
    assert!(pos.proponents.contains(&"Frankfurt".to_string()));

    // The skeleton JSON should be valid — round-trip test.
    let json = serde_json::to_string_pretty(&skeleton).unwrap();
    let reparsed: corpus_engine::FieldSkeleton = serde_json::from_str(&json).unwrap();
    assert_eq!(
        reparsed.canonical_questions.len(),
        skeleton.canonical_questions.len()
    );
}

/// Verify that the enrichment checkpoint is cleared after successful completion.
/// A leftover checkpoint would trigger the health checker's "interrupted enrichment" warning.
#[tokio::test]
async fn enrichment_clears_checkpoint_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("sep-mini.parquet");
    make_sep_like_parquet(&parquet_path);

    let recipe_path = write_recipe_toml(&recipes_dir, &parquet_path, true);
    let engine = build_engine(recipes_dir, indexes_dir.clone());

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("ingest should succeed");

    // The checkpoint file should NOT exist after successful completion.
    let index = engine.open_index_for_corpus("test_corpus").await.unwrap();
    let checkpoint_path = index.path().join("_enrichment_checkpoint.json");
    assert!(
        !checkpoint_path.exists(),
        "checkpoint file should be cleared after successful enrichment"
    );
}

/// Verify that non-enriched corpora don't produce field model artifacts.
#[tokio::test]
async fn non_enriched_corpus_has_no_field_model() {
    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("plain.parquet");
    make_sep_like_parquet(&parquet_path);

    let recipe_path = write_recipe_toml(&recipes_dir, &parquet_path, false);
    let engine = build_engine(recipes_dir, indexes_dir);

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), None)
        .await
        .expect("non-enriched ingest should succeed");

    let index = engine.open_index_for_corpus("test_corpus").await.unwrap();
    let skeleton = index.load_field_skeleton().unwrap();
    assert!(
        skeleton.is_none(),
        "non-enriched corpus should not have field_skeleton.json"
    );
    assert!(
        !index.has_field_model_tables().await,
        "non-enriched corpus should not have field model tables"
    );
}

/// Progress callbacks must fire for every phase.
/// The desktop UI relies on this to show status instead of going
/// silent after indexing.
#[tokio::test]
async fn ingest_progress_callback_fires_for_completion() {
    use std::sync::Mutex;

    let dir = tempfile::tempdir().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let indexes_dir = dir.path().join("indexes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let parquet_path = dir.path().join("fixture.parquet");
    make_sep_like_parquet(&parquet_path);

    let recipe_path = write_recipe_toml(&recipes_dir, &parquet_path, true);
    let engine = build_engine(recipes_dir, indexes_dir);

    let phases: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let phases_inner = Arc::clone(&phases);
    let progress: corpus_engine::ProgressCallback = Box::new(move |p| {
        let label = match p {
            corpus_engine::IngestProgress::Downloading { .. } => "downloading",
            corpus_engine::IngestProgress::Extracting { .. } => "extracting",
            corpus_engine::IngestProgress::Chunking { .. } => "chunking",
            corpus_engine::IngestProgress::Embedding { .. } => "embedding",
            corpus_engine::IngestProgress::Indexing { .. } => "indexing",
            corpus_engine::IngestProgress::OptimizingIndex { .. } => "optimizing_index",
            corpus_engine::IngestProgress::Enriching { .. } => "enriching",
            corpus_engine::IngestProgress::Complete { .. } => "complete",
        };
        phases_inner.lock().unwrap().push(label);
    });

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), Some(progress))
        .await
        .expect("ingest should succeed");

    let observed = phases.lock().unwrap().clone();
    // The field model enrichment progress is not forwarded to IngestProgress
    // yet (TODO), so we just check that the pipeline completes.
    assert!(
        observed.contains(&"complete"),
        "progress callback should fire on completion, got {observed:?}"
    );
}
