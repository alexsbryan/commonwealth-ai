// SPDX-License-Identifier: AGPL-3.0-or-later
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::*;
use sovereign_core::types::*;
use sovereign_tools::document::DocumentTool;
use sovereign_tools::rag::chunk::chunk_text;
use sovereign_tools::rag::ingest::ingest_directory;
use sovereign_tools::rag::parse::{list_parseable_files, parse_file};

use sovereign_store::sqlite::SqliteStateStore;

// ─── Parse Tests ───────────────────────────────────────────────

#[test]
fn parse_txt_file() {
    let dir = tempdir_with_files(&[("test.txt", "Hello, world!")]);
    let path = dir.join("test.txt");
    let doc = parse_file(&path).unwrap();
    assert_eq!(doc.content, "Hello, world!");
    assert!(doc.source.contains("test.txt"));
}

#[test]
fn parse_md_file() {
    let dir = tempdir_with_files(&[("test.md", "# Title\n\nSome **bold** text.\n\n[link](url)")]);
    let path = dir.join("test.md");
    let doc = parse_file(&path).unwrap();
    assert!(doc.content.contains("Title"));
    assert!(doc.content.contains("bold"));
    assert!(!doc.content.contains('#'));
    assert!(!doc.content.contains("**"));
}

#[test]
fn parse_unsupported_format() {
    let dir = tempdir_with_files(&[("test.pdf", "fake pdf")]);
    let path = dir.join("test.pdf");
    assert!(parse_file(&path).is_err());
}

#[test]
fn list_parseable_files_filters() {
    let dir = tempdir_with_files(&[
        ("a.txt", "text"),
        ("b.md", "markdown"),
        ("c.rs", "rust code"),
        ("d.json", "json"),
    ]);
    let files = list_parseable_files(&dir).unwrap();
    assert_eq!(files.len(), 2);
}

// ─── Chunk Tests ───────────────────────────────────────────────

#[test]
fn chunk_short_document() {
    let chunks = chunk_text("Short document.");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].content, "Short document.");
}

#[test]
fn chunk_multi_paragraph() {
    let text = "Para one.\n\nPara two.\n\nPara three.";
    let chunks = chunk_text(text);
    assert_eq!(chunks.len(), 1); // Short enough for one chunk.
    assert!(chunks[0].content.contains("Para one."));
    assert!(chunks[0].content.contains("Para three."));
}

// ─── Ingestion Integration Tests ───────────────────────────────

#[tokio::test]
async fn ingest_directory_txt_files() {
    let dir = tempdir_with_files(&[
        ("doc1.txt", "Rust is a systems programming language."),
        ("doc2.txt", "Python is great for data science."),
        ("doc3.md", "# JavaScript\n\nJavaScript runs in the browser."),
    ]);

    let store = SqliteStateStore::open_in_memory().unwrap();

    let result = ingest_directory(&dir, &store, None).await.unwrap();
    assert_eq!(result.files_processed, 3);
    assert_eq!(result.files_skipped, 0);
    assert!(result.chunks_created >= 3);
}

#[tokio::test]
async fn ingest_and_search_documents() {
    let dir = tempdir_with_files(&[
        (
            "rust.txt",
            "Rust provides memory safety without garbage collection through its ownership system.",
        ),
        (
            "python.txt",
            "Python is an interpreted language popular for machine learning and data analysis.",
        ),
        (
            "go.txt",
            "Go was designed at Google for building scalable networked services.",
        ),
    ]);

    let store = SqliteStateStore::open_in_memory().unwrap();
    ingest_directory(&dir, &store, None).await.unwrap();

    // FTS5 search for "memory safety" should find the Rust document.
    let results = store
        .search_documents(&[], "memory safety", 5)
        .await
        .unwrap();
    assert!(!results.is_empty(), "Expected FTS5 to find documents");
    assert!(
        results[0].content.contains("Rust"),
        "Expected Rust document first, got: {}",
        &results[0].content[..results[0].content.len().min(100)]
    );
}

#[tokio::test]
async fn ingest_and_search_no_results() {
    let dir = tempdir_with_files(&[("doc.txt", "This is about cats and dogs.")]);

    let store = SqliteStateStore::open_in_memory().unwrap();
    ingest_directory(&dir, &store, None).await.unwrap();

    let results = store
        .search_documents(&[], "quantum physics", 5)
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn ingest_skips_unsupported_files() {
    let dir = tempdir_with_files(&[
        ("doc.txt", "valid text"),
        ("code.rs", "fn main() {}"),
        ("data.json", "{}"),
    ]);

    let store = SqliteStateStore::open_in_memory().unwrap();
    let result = ingest_directory(&dir, &store, None).await.unwrap();
    assert_eq!(result.files_processed, 1);
    assert_eq!(result.files_skipped, 0); // .rs and .json aren't listed as parseable
}

// ─── DocumentTool Tests ────────────────────────────────────────

struct SummaryMockInference;

#[async_trait]
impl InferenceProvider for SummaryMockInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        // Return a mock summary based on what was asked.
        let text = if request.prompt.contains("Synthesize") || request.prompt.contains("synthesize")
        {
            "Final comprehensive summary of all sections.".to_string()
        } else {
            format!(
                "Summary of: {}...",
                &request.prompt[..request.prompt.len().min(50)]
            )
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 20,
            prompt_tokens: 0,
            model_id: "mock".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("mock".to_string()))
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("mock".to_string()))
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

#[tokio::test]
async fn document_tool_summarize_small() {
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let inference = Arc::new(SummaryMockInference);

    // Ingest a small document (fits in one batch).
    store
        .store_chunks(&[DocumentChunk {
            id: "doc:0".to_string(),
            source: "doc.txt".to_string(),
            content: "This is a short document about Rust programming.".to_string(),
            chunk_index: 0,
            embedding: None,
            created_at: 0,
            source_type: SourceType::UserDocument,
            version: 0,
            deleted_at: None,
        }])
        .await
        .unwrap();

    let tool = DocumentTool::new(store, inference);
    let result = tool
        .declared()
        .execute(
            &serde_json::json!({"source": "doc.txt", "operation": "summarize"}),
            &ToolContext {
                conversation_id: "c1".to_string(),
                task_id: None,
                working_directory: None,
                in_reasoning_loop: false,
                agent_session_token: None,
                turn_index: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    if let StepOutput::Text(text) = result {
        assert!(!text.is_empty());
    } else {
        panic!("Expected StepOutput::Text");
    }
}

#[tokio::test]
async fn document_tool_summarize_large() {
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let inference = Arc::new(SummaryMockInference);

    // Ingest a document with 10 chunks (triggers map-reduce).
    let chunks: Vec<DocumentChunk> = (0..10)
        .map(|i| DocumentChunk {
            id: format!("big:{}", i),
            source: "big.txt".to_string(),
            content: format!(
                "Chapter {i}. This is a lengthy chapter about topic {i} with lots of detail."
            ),
            chunk_index: i,
            embedding: None,
            created_at: 0,
            source_type: SourceType::UserDocument,
            version: 0,
            deleted_at: None,
        })
        .collect();

    store.store_chunks(&chunks).await.unwrap();

    let tool = DocumentTool::new(store, inference);
    let result = tool
        .declared()
        .execute(
            &serde_json::json!({"source": "big.txt", "operation": "summarize"}),
            &ToolContext {
                conversation_id: "c1".to_string(),
                task_id: None,
                working_directory: None,
                in_reasoning_loop: false,
                agent_session_token: None,
                turn_index: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    if let StepOutput::Text(text) = result {
        // Should contain the final synthesis.
        assert!(
            text.contains("comprehensive summary") || text.contains("Summary"),
            "Expected synthesis output, got: {text}"
        );
    } else {
        panic!("Expected StepOutput::Text");
    }
}

#[tokio::test]
async fn document_tool_source_not_found() {
    let store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
    let inference = Arc::new(SummaryMockInference);

    let tool = DocumentTool::new(store, inference);
    let result = tool
        .declared()
        .execute(
            &serde_json::json!({"source": "nonexistent.txt", "operation": "summarize"}),
            &ToolContext {
                conversation_id: "c1".to_string(),
                task_id: None,
                working_directory: None,
                in_reasoning_loop: false,
                agent_session_token: None,
                turn_index: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    if let StepOutput::Text(text) = result {
        assert!(text.contains("No document found"));
    } else {
        panic!("Expected StepOutput::Text");
    }
}

// ─── Test Helpers ──────────────────────────────────────────────

fn tempdir_with_files(files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sovereign-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
    dir
}
