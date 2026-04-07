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
    let texts = StringArray::from(vec![
        "Compatibilists hold that free will is consistent with determinism. \
         Most contemporary philosophers accept some form of compatibilism.",
        "Frankfurt cases are widely cited as support for the compatibilist view \
         that moral responsibility does not require alternative possibilities.",
        "Hard incompatibilists argue that determinism is incompatible with \
         the kind of free will required for moral responsibility.",
        "Pereboom maintains that even if our actions are determined, we lack \
         the control necessary for genuine moral responsibility.",
    ]);

    let batch =
        RecordBatch::try_new(schema.clone(), vec![Arc::new(titles), Arc::new(texts)])
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

/// Mock inference: returns a canned JSON array of one claim per call,
/// or a "none" relationship for relationship-extraction prompts.
/// The engine's parser is forgiving — surrounding prose and markdown
/// fences are tolerated — but we return clean JSON to keep the test
/// readable.
fn mock_inference_fn() -> InferenceFn {
    Arc::new(|prompt: &str| {
        let response = if prompt.contains("two claims") || prompt.contains("Claim A") {
            // Relationship extraction prompt — keep things deterministic
            // by always declaring no relationship. Avoids combinatorial
            // blow-up if the test fixture grows.
            r#"{"relationship": "none", "confidence": 0.0}"#.to_string()
        } else {
            // Claim extraction prompt — return one claim per chunk.
            r#"[{
                "claim": "Free will is compatible with determinism.",
                "epistemic_status": "majority",
                "hedging_language": "Most contemporary philosophers accept",
                "attributed_to": "Compatibilists"
            }]"#
                .to_string()
        };
        Box::pin(async move { Ok(response) })
    })
}

/// Write a recipe TOML to disk pointing at the given local parquet
/// file. The recipe form is the same one a user would author by hand
/// for a custom corpus.
fn write_recipe_toml(
    recipes_dir: &Path,
    parquet_path: &Path,
    enable_enrichment: bool,
) -> PathBuf {
    let recipe_path = recipes_dir.join("test_corpus.toml");
    let parquet_str = parquet_path.to_string_lossy();
    let enrichment_block = if enable_enrichment {
        r#"
[enrichment]
enabled = true
extract_relationships = true
relationship_similarity_threshold = 0.0
max_relationship_candidates = 16
claim_extraction_prompt = """
Extract propositional claims from this passage. Return a JSON array.
"""
relationship_extraction_prompt = """
Given two claims:
Claim A: {claim_a}
Claim B: {claim_b}
Determine the relationship.
"""
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

    // The index should open and be searchable. Since we never enabled
    // enrichment, there must be no claims table.
    let index = engine.open_index_for_corpus("test_corpus").await.unwrap();
    assert!(
        !index.has_claims_table().await,
        "non-enriched corpus should not have a claims table"
    );

    // A vector search with a deterministic query embedding should
    // return at least one result. We use the embed_fn directly so the
    // test stays self-contained.
    let query_embedding = engine.embed("compatibilism").await.unwrap();
    let results = index.search(&query_embedding, "compatibilism", 5).await.unwrap();
    assert!(
        !results.is_empty(),
        "search on a populated index should return at least one chunk"
    );
}

/// The enrichment-enabled ingest path. This is the SEP demo target:
/// parquet → chunks → embeddings → LanceDB → claim extraction →
/// relationship extraction. Verifies the engine actually invokes the
/// `InferenceFn`, parses claims, and stores them in the `claims` table.
#[tokio::test]
async fn parquet_ingest_with_enrichment_populates_claims_table() {
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

    // The claims table must exist now.
    let index = engine.open_index_for_corpus("test_corpus").await.unwrap();
    assert!(
        index.has_claims_table().await,
        "enriched ingest should create the claims table"
    );

    // Per the mock inference fn, every chunk produces one claim.
    let query_embedding = engine.embed("free will").await.unwrap();
    let scored_claims = index
        .search_claims(&query_embedding, "free will", 10)
        .await
        .expect("search_claims should succeed on enriched index");
    assert!(
        !scored_claims.is_empty(),
        "enriched corpus should return claims from search_claims"
    );

    // The mocked claims always carry "majority" status with a
    // compatibilist attribution — verify the metadata round-tripped
    // through LanceDB intact.
    let first = &scored_claims[0].claim;
    assert_eq!(
        first.epistemic_status,
        corpus_engine::EpistemicStatus::Majority
    );
    assert_eq!(first.attributed_to.as_deref(), Some("Compatibilists"));
}

/// Progress callbacks must fire for every phase, including the
/// enrichment phases. The desktop UI relies on this to show
/// "Extracting claims…" instead of going silent after indexing.
#[tokio::test]
async fn ingest_progress_callback_fires_for_enrichment_phases() {
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
            corpus_engine::IngestProgress::ExtractingClaims { .. } => "extracting_claims",
            corpus_engine::IngestProgress::FoundCandidatePairs { .. } => "found_pairs",
            corpus_engine::IngestProgress::ExtractingRelationships { .. } => {
                "extracting_relationships"
            }
            corpus_engine::IngestProgress::Complete { .. } => "complete",
            corpus_engine::IngestProgress::BuildingLinkGraph { .. } => "building_link_graph",
            corpus_engine::IngestProgress::ComputingArticleProfiles { .. } => {
                "computing_article_profiles"
            }
        };
        phases_inner.lock().unwrap().push(label);
    });

    engine
        .ingest(&CorpusSpec::RecipePath(recipe_path), Some(progress))
        .await
        .expect("ingest should succeed");

    let observed = phases.lock().unwrap().clone();
    assert!(
        observed.contains(&"extracting_claims"),
        "progress callback should fire for the claim-extraction phase, got {observed:?}"
    );
    assert!(
        observed.contains(&"found_pairs"),
        "progress callback should fire when candidate pairs are found, got {observed:?}"
    );
    assert!(
        observed.contains(&"complete"),
        "progress callback should fire on completion, got {observed:?}"
    );
}
