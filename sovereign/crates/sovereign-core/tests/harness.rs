use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::executor::AutoApprovalChannel;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::stubs::PassthroughRouter;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::*;
use sovereign_core::SkillRegistry;
use sovereign_core::ToolRegistry;
use sovereign_store::sqlite::SqliteStateStore;

// ─── Deterministic Inference Provider ────────────────────────
//
// A rules-based inference provider that produces structured, predictable
// responses based on prompt content. Not a mock — it exercises real code
// paths. The runtime, executor, search pipeline, memory system, and store
// all run with real logic.

pub struct DeterministicInference;

#[async_trait]
impl InferenceProvider for DeterministicInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let prompt_lower = request.prompt.to_lowercase();

        let text = if prompt_lower.contains("a, b, or c")
            || prompt_lower.contains("a) simple")
            || prompt_lower.contains("categories:")
        {
            // Router classification — route to DeepQuery
            "B".to_string()
        } else if prompt_lower.contains("routine")
            && prompt_lower.contains("moderate")
            && prompt_lower.contains("hard")
        {
            // Difficulty estimation
            "moderate".to_string()
        } else if prompt_lower.contains("yes or no") {
            "yes".to_string()
        } else if prompt_lower.contains("\"steps\"") && prompt_lower.contains("\"edges\"") {
            // Plan generation
            r#"{"goal":"test","steps":[{"id":0,"description":"answer","kind":"reason","prompt":"Answer the question","speed":"slow"}],"edges":[]}"#.to_string()
        } else if prompt_lower.contains("relevant knowledge:") {
            // Synthesis with knowledge context
            "Based on the provided knowledge, here is the answer. [Source: local knowledge] The sources indicate this is correct.".to_string()
        } else if prompt_lower.contains("search results") {
            "Based on the sources provided, [1] indicates the answer. [2] supports this.".to_string()
        } else if prompt_lower.contains("\"pass\"") && prompt_lower.contains("feedback") {
            r#"{"pass": true}"#.to_string()
        } else if prompt_lower.contains("select the best") {
            "1".to_string()
        } else if prompt_lower.contains("extract") && prompt_lower.contains("memor") {
            // Memory extraction — return empty to avoid side effects
            "No new facts to extract.".to_string()
        } else if prompt_lower.contains("working memory") || prompt_lower.contains("current goal") {
            // Working memory compression
            r#"{"current_goal": null, "facts": [], "active_documents": []}"#.to_string()
        } else {
            // Default: deterministic echo response
            let snippet = &request.prompt[..request.prompt.len().min(100)];
            format!("Response to: {snippet}")
        };

        Ok(CompletionResponse {
            text,
            tokens_used: 10,
            model_id: "deterministic".to_string(),
            latency_ms: 1,
            oicp_meta: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("Streaming not supported in deterministic inference".to_string()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        // Return a zero vector — FTS5 text search will be the primary retrieval method
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

// ─── Test Harness ────────────────────────────────────────────

pub struct TestHarness {
    pub runtime: Runtime,
    pub store: Arc<SqliteStateStore>,
}

impl TestHarness {
    /// Create a harness with DeterministicInference, real in-memory SQLite,
    /// PassthroughRouter (SimpleQuery for all), and no tools.
    pub fn new() -> Self {
        let inference: Arc<dyn InferenceProvider> = Arc::new(DeterministicInference);
        let shared_store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        let store_trait: Arc<dyn StateStore> = Arc::clone(&shared_store) as Arc<dyn StateStore>;

        let skills = Arc::new(SkillRegistry::new());
        let router: Box<dyn sovereign_core::traits::Router> =
            Box::new(PassthroughRouter);
        let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
        let tools = Arc::new(ToolRegistry::new());
        let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> =
            Arc::new(AutoApprovalChannel);

        let runtime = Runtime::new(
            inference,
            router,
            Box::new(planner),
            tools,
            store_trait,
            skills,
            approval,
        );

        Self {
            runtime,
            store: shared_store,
        }
    }

    /// Send a message and return the response. Uses a random conversation ID.
    pub async fn send(&self, message: &str) -> Response {
        let conv_id = uuid::Uuid::new_v4().to_string();
        self.send_in(message, &conv_id).await
    }

    /// Send a message in a specific conversation.
    pub async fn send_in(&self, message: &str, conversation_id: &str) -> Response {
        self.runtime
            .handle_message(message, conversation_id)
            .await
            .expect("handle_message should not fail")
    }

    /// Extract provenance from a response's metadata.
    pub fn provenance(&self, response: &Response) -> ResponseProvenance {
        let metadata = response
            .message
            .metadata
            .as_ref()
            .expect("Response should have metadata");
        let prov_value = metadata
            .get("provenance")
            .expect("Metadata should contain provenance");
        serde_json::from_value(prov_value.clone())
            .expect("Provenance should deserialize")
    }

    /// Get the number of messages in a conversation.
    pub async fn conversation_length(&self, conversation_id: &str) -> usize {
        match self.store.get_conversation(conversation_id).await {
            Ok(conv) => conv.messages.len(),
            Err(_) => 0,
        }
    }

    /// Ingest test content directly into the store as corpus chunks.
    /// Each (source, content) pair becomes a DocumentChunk with the given corpus_id.
    /// FTS5 triggers fire, so text search works immediately.
    pub async fn ingest_test_corpus(&self, corpus_id: &str, chunks: Vec<(&str, &str)>) {
        let doc_chunks: Vec<DocumentChunk> = chunks
            .iter()
            .enumerate()
            .map(|(i, (source, content))| DocumentChunk {
                id: format!("{corpus_id}:{source}:{i}"),
                source: source.to_string(),
                content: content.to_string(),
                chunk_index: i,
                embedding: None,
                created_at: 0,
                source_type: SourceType::Corpus {
                    corpus_id: corpus_id.to_string(),
                },
                version: 0,
                deleted_at: None,
            })
            .collect();

        self.store.store_chunks(&doc_chunks).await.unwrap();

        self.store
            .save_corpus_state(&CorpusState {
                corpus_id: corpus_id.to_string(),
                installed_at: 0,
                source_date: "test".to_string(),
                chunks_count: chunks.len() as i64,
                index_size_mb: 0,
                last_updated: 0,
                version: 0,
                deleted_at: None,
            })
            .await
            .unwrap();
    }
}
