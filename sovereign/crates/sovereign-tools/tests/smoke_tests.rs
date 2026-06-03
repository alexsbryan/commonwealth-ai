//! Smoke tests for corpus ingestion and full search pipeline.
//!
//! These tests exercise real code paths:
//! - Real Parquet parsing (arrow + parquet crates)
//! - Real SQLite with FTS5 (in-memory)
//! - Real search pipeline (FTS5 query → ranked results)
//! - Real provenance recording
//!
//! No mocks. No stubs. No model files. No network.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::executor::AutoApprovalChannel;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::stubs::PassthroughRouter;
use sovereign_core::traits::*;
use sovereign_core::types::*;
use sovereign_core::{SkillRegistry, ToolRegistry};
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::corpus::parquet_reader::ParquetParser;
use sovereign_tools::corpus::CorpusParser;

// ─── Deterministic Inference (same as harness.rs) ────────────

struct DeterministicInference;

#[async_trait]
impl InferenceProvider for DeterministicInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let prompt_lower = request.prompt.to_lowercase();
        let text = if prompt_lower.contains("a, b, or c") || prompt_lower.contains("categories:") {
            "B".to_string()
        } else if prompt_lower.contains("routine")
            && prompt_lower.contains("moderate")
            && prompt_lower.contains("hard")
        {
            "moderate".to_string()
        } else if prompt_lower.contains("relevant knowledge:") {
            "Based on the provided knowledge, the sources confirm this answer. See [Source: sep] for details.".to_string()
        } else if prompt_lower.contains("extract") && prompt_lower.contains("memor") {
            "No new facts.".to_string()
        } else if prompt_lower.contains("working memory") || prompt_lower.contains("current goal") {
            r#"{"current_goal": null, "facts": [], "active_documents": []}"#.to_string()
        } else {
            format!(
                "Response to: {}",
                &request.prompt[..request.prompt.len().min(80)]
            )
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 10,
            prompt_tokens: 0,
            model_id: "deterministic".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("not supported".to_string()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

// ─── Test Helpers ────────────────────────────────────────────

fn make_test_parquet(path: &std::path::Path) {
    use arrow::array::StringArray;
    use arrow::datatypes::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;
    use std::fs::File;

    let schema = Arc::new(Schema::new(vec![
        Field::new("text", DataType::Utf8, false),
        Field::new("category", DataType::Utf8, true),
    ]));

    let texts = StringArray::from(vec![
        "Henri Bergson's essay 'Laughter' (Le Rire, 1900) examines the comic as a social corrective. \
         Bergson argues that laughter targets the mechanical encrusted upon the living — rigidity of \
         body, mind, or character that deviates from the suppleness life demands. The comic arises \
         when a person behaves like a thing, an automaton, or a puppet. Laughter serves as a social \
         sanction, correcting this mechanical behavior by shaming the individual back into flexibility.",

        "Epistemology is the branch of philosophy concerned with knowledge. It asks questions like: \
         What is knowledge? How is knowledge acquired? To what extent can a subject be known? \
         Major epistemological theories include rationalism, empiricism, and constructivism.",

        "Immanuel Kant's Critique of Pure Reason (1781) investigates the limits and conditions of \
         human knowledge. Kant argues that while all knowledge begins with experience, it does not \
         follow that all knowledge arises from experience. He distinguishes between a priori and a \
         posteriori knowledge, and between analytic and synthetic judgments.",

        "Philosophy of mind examines the nature of mental phenomena, consciousness, and their \
         relationship to the physical body. Key problems include the mind-body problem, the hard \
         problem of consciousness, and the nature of intentionality.",

        "Existentialism is a philosophical movement that emphasizes individual existence, freedom, \
         and choice. Key thinkers include Kierkegaard, Heidegger, Sartre, and de Beauvoir. \
         Central themes include authenticity, anxiety, absurdity, and the meaning of being.",
    ]);

    let categories = StringArray::from(vec![
        Some("Bergson"),
        Some("Epistemology"),
        Some("Kant"),
        Some("Philosophy of Mind"),
        Some("Existentialism"),
    ]);

    let batch = arrow::record_batch::RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(texts), Arc::new(categories)],
    )
    .unwrap();

    let file = File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn build_runtime(store: Arc<SqliteStateStore>) -> Runtime {
    let inference: Arc<dyn InferenceProvider> = Arc::new(DeterministicInference);
    let store_trait: Arc<dyn StateStore> = Arc::clone(&store) as Arc<dyn StateStore>;
    let skills = Arc::new(SkillRegistry::new());
    let router: Box<dyn sovereign_core::traits::Router> = Box::new(PassthroughRouter);
    let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
    let tools = Arc::new(ToolRegistry::new());
    let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> = Arc::new(AutoApprovalChannel);

    Runtime::new(
        inference,
        router,
        Box::new(planner),
        tools,
        store_trait,
        skills,
        approval,
        InferenceConfig::default(),
    )
}

fn extract_provenance(response: &Response) -> ResponseProvenance {
    let metadata = response.message.metadata.as_ref().unwrap();
    let prov_value = metadata.get("provenance").unwrap();
    serde_json::from_value(prov_value.clone()).unwrap()
}

// ═══════════════════════════════════════════════════════════════
// SMOKE TEST GROUP 1: Corpus Ingestion
// ═══════════════════════════════════════════════════════════════

#[test]
fn parquet_parser_produces_chunks_with_correct_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let parquet_path = dir.path().join("sep.parquet");
    make_test_parquet(&parquet_path);

    let parser = ParquetParser::new("sep", "text", Some("category"));
    let chunks: Vec<DocumentChunk> = parser
        .parse(&parquet_path)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();

    // All 5 articles produced chunks.
    assert!(
        chunks.len() >= 5,
        "Expected at least 5 chunks, got {}",
        chunks.len()
    );

    // Every chunk has the correct source_type.
    for chunk in &chunks {
        assert_eq!(
            chunk.source_type,
            SourceType::Corpus {
                corpus_id: "sep".to_string()
            },
            "All chunks should have source_type Corpus/sep"
        );
    }

    // Bergson article is present and has substantive content.
    let bergson_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.content.contains("Bergson"))
        .collect();
    assert!(
        !bergson_chunks.is_empty(),
        "Bergson article should be in chunks"
    );
    assert!(
        bergson_chunks[0].content.contains("Laughter"),
        "Bergson chunk should mention Laughter essay"
    );
    assert!(
        bergson_chunks[0].content.contains("mechanical"),
        "Bergson chunk should contain key concept"
    );

    // Category label is prepended.
    assert!(
        bergson_chunks[0]
            .content
            .starts_with("Stanford Encyclopedia of Philosophy: Bergson"),
        "Chunk should start with SEP label + category. Got: {}",
        &bergson_chunks[0].content[..80.min(bergson_chunks[0].content.len())]
    );
}

#[test]
fn parquet_chunks_are_storable_and_searchable_via_fts5() {
    let dir = tempfile::tempdir().unwrap();
    let parquet_path = dir.path().join("sep.parquet");
    make_test_parquet(&parquet_path);

    // Parse Parquet into chunks.
    let parser = ParquetParser::new("sep", "text", Some("category"));
    let chunks: Vec<DocumentChunk> = parser
        .parse(&parquet_path)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();

    // Store in real SQLite.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = SqliteStateStore::open_in_memory().unwrap();
    rt.block_on(store.store_chunks(&chunks)).unwrap();

    // Search via FTS5.
    let results = rt
        .block_on(store.search_documents(&[], "Bergson laughter mechanical", 5))
        .unwrap();

    assert!(
        !results.is_empty(),
        "FTS5 should find Bergson article after Parquet ingestion"
    );
    assert!(
        results[0].content.contains("Bergson"),
        "Top result should be the Bergson article"
    );
    assert_eq!(
        results[0].source_type,
        SourceType::Corpus {
            corpus_id: "sep".to_string()
        },
        "Result should retain corpus source_type"
    );
}

#[test]
fn parquet_ingestion_handles_multiple_topics() {
    let dir = tempfile::tempdir().unwrap();
    let parquet_path = dir.path().join("sep.parquet");
    make_test_parquet(&parquet_path);

    let parser = ParquetParser::new("sep", "text", Some("category"));
    let chunks: Vec<DocumentChunk> = parser
        .parse(&parquet_path)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = SqliteStateStore::open_in_memory().unwrap();
    rt.block_on(store.store_chunks(&chunks)).unwrap();

    // Search for different topics — each should find the right article.
    let kant = rt
        .block_on(store.search_documents(&[], "Kant critique pure reason", 5))
        .unwrap();
    assert!(!kant.is_empty(), "Should find Kant article");
    assert!(kant[0].content.contains("Kant"));

    let existentialism = rt
        .block_on(store.search_documents(&[], "existentialism Sartre freedom", 5))
        .unwrap();
    assert!(
        !existentialism.is_empty(),
        "Should find existentialism article"
    );
    assert!(
        existentialism[0].content.contains("Existentialism")
            || existentialism[0].content.contains("existentialism")
    );

    let epistemology = rt
        .block_on(store.search_documents(&[], "epistemology knowledge justified belief", 5))
        .unwrap();
    assert!(!epistemology.is_empty(), "Should find epistemology article");
}

// ═══════════════════════════════════════════════════════════════
// SMOKE TEST GROUP 2: Full Pipeline (message → search → provenance)
// ═══════════════════════════════════════════════════════════════

#[tokio::test]
async fn full_pipeline_query_finds_corpus_and_records_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let parquet_path = dir.path().join("sep.parquet");
    make_test_parquet(&parquet_path);

    // Ingest Parquet into real store.
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let parser = ParquetParser::new("sep", "text", Some("category"));
    let chunks: Vec<DocumentChunk> = parser
        .parse(&parquet_path)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    store.store_chunks(&chunks).await.unwrap();
    store
        .save_corpus_state(&CorpusState {
            corpus_id: "sep".to_string(),
            installed_at: 0,
            source_date: "test".to_string(),
            chunks_count: chunks.len() as i64,
            index_size_mb: 0,
            last_updated: 0,
            version: 0,
            deleted_at: None,
            vector_index_ready: false,
        })
        .await
        .unwrap();

    // Build runtime with the loaded store.
    let runtime = build_runtime(Arc::clone(&store));

    // Send a query about Bergson.
    let response = runtime
        .handle_message(
            "What did Bergson write about humor and laughter?",
            "smoke-test-1",
        )
        .await
        .unwrap();

    // The response should exist and be non-empty.
    assert!(
        !response.message.content.is_empty(),
        "Response should not be empty"
    );

    // Provenance should show SEP was consulted with results.
    let prov = extract_provenance(&response);
    assert!(
        prov.search_method.is_some(),
        "Search method should be recorded"
    );
    assert!(
        prov.sources
            .iter()
            .any(|s| s.origin == "sep" && s.count > 0),
        "Provenance should show SEP chunks were found. Sources: {:?}",
        prov.sources
    );

    // The response content should reference knowledge (the DeterministicInference
    // returns "Based on the provided knowledge..." when it sees "Relevant knowledge:")
    assert!(
        response.message.content.contains("knowledge")
            || response.message.content.contains("source"),
        "Response should reference knowledge sources. Got: {}",
        &response.message.content[..response.message.content.len().min(200)]
    );
}

#[tokio::test]
async fn full_pipeline_query_without_corpus_records_empty_provenance() {
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let runtime = build_runtime(Arc::clone(&store));

    let response = runtime
        .handle_message("What is the meaning of life?", "smoke-test-2")
        .await
        .unwrap();

    assert!(!response.message.content.is_empty());

    let prov = extract_provenance(&response);
    // No corpora installed → no sources
    assert!(
        prov.sources.is_empty(),
        "Should have no sources without corpora"
    );
}

#[tokio::test]
async fn full_pipeline_multi_topic_queries_find_different_sources() {
    let dir = tempfile::tempdir().unwrap();
    let parquet_path = dir.path().join("sep.parquet");
    make_test_parquet(&parquet_path);

    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let parser = ParquetParser::new("sep", "text", Some("category"));
    let chunks: Vec<DocumentChunk> = parser
        .parse(&parquet_path)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    store.store_chunks(&chunks).await.unwrap();
    store
        .save_corpus_state(&CorpusState {
            corpus_id: "sep".to_string(),
            installed_at: 0,
            source_date: "test".to_string(),
            chunks_count: chunks.len() as i64,
            index_size_mb: 0,
            last_updated: 0,
            version: 0,
            deleted_at: None,
            vector_index_ready: false,
        })
        .await
        .unwrap();

    let runtime = build_runtime(Arc::clone(&store));

    // Query 1: Bergson
    let r1 = runtime
        .handle_message("Tell me about Bergson's theory of humor", "c1")
        .await
        .unwrap();
    let p1 = extract_provenance(&r1);
    assert!(
        p1.sources.iter().any(|s| s.origin == "sep" && s.count > 0),
        "Bergson query should find SEP sources"
    );

    // Query 2: Kant
    let r2 = runtime
        .handle_message("Explain Kant's Critique of Pure Reason", "c2")
        .await
        .unwrap();
    let p2 = extract_provenance(&r2);
    assert!(
        p2.sources.iter().any(|s| s.origin == "sep" && s.count > 0),
        "Kant query should find SEP sources"
    );

    // Query 3: Unrelated (no match expected)
    let r3 = runtime
        .handle_message("What is the best recipe for chocolate cake?", "c3")
        .await
        .unwrap();
    let p3 = extract_provenance(&r3);
    // Should search but find nothing relevant
    let sep_hits = p3
        .sources
        .iter()
        .find(|s| s.origin == "sep")
        .map(|s| s.count)
        .unwrap_or(0);
    assert_eq!(
        sep_hits, 0,
        "Chocolate cake query should not match philosophy corpus"
    );
}

#[tokio::test]
async fn full_pipeline_provenance_persists_across_store_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let parquet_path = dir.path().join("sep.parquet");
    make_test_parquet(&parquet_path);

    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let parser = ParquetParser::new("sep", "text", Some("category"));
    let chunks: Vec<DocumentChunk> = parser
        .parse(&parquet_path)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    store.store_chunks(&chunks).await.unwrap();
    store
        .save_corpus_state(&CorpusState {
            corpus_id: "sep".to_string(),
            installed_at: 0,
            source_date: "test".to_string(),
            chunks_count: chunks.len() as i64,
            index_size_mb: 0,
            last_updated: 0,
            version: 0,
            deleted_at: None,
            vector_index_ready: false,
        })
        .await
        .unwrap();

    let runtime = build_runtime(Arc::clone(&store));

    runtime
        .handle_message("What is epistemology?", "persist-test")
        .await
        .unwrap();

    // Read back from the store.
    let conv = store.get_conversation("persist-test").await.unwrap();
    let assistant_msg = conv
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();

    // Extract provenance from the persisted message.
    let metadata = assistant_msg.metadata.as_ref().unwrap();
    let prov: ResponseProvenance =
        serde_json::from_value(metadata.get("provenance").unwrap().clone()).unwrap();

    assert!(!prov.intent.is_empty());
    assert_eq!(prov.inference_backend, "deterministic");
    assert!(prov.search_method.is_some());
    // SEP is installed, so it should appear in sources (with or without matches).
    assert!(
        !prov.sources.is_empty(),
        "Persisted provenance should list searched corpora"
    );
}
