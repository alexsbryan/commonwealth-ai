use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::Result;
use crate::types::*;

// ─── 1. Inference ──────────────────────────────────────────────

#[async_trait]
pub trait InferenceProvider: Send + Sync {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse>;

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>>;

    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a query string, applying any model-specific query instruction prefix.
    ///
    /// Default implementation calls `embed()` — override in providers that support
    /// asymmetric instruction-aware models (e.g. Qwen3-Embedding) where the query
    /// side gets a different prefix than the document side. The distinction yields
    /// 1–5% retrieval improvement on instruction-aware models.
    async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed(query).await
    }

    /// Return the model ID that will be selected for a request at the given
    /// speed tier, without running inference. Used to populate provenance on
    /// streaming responses (where `complete_stream` returns no metadata).
    /// Default returns `"unknown"` — override in providers that know their
    /// loaded model names without blocking on a lock.
    fn model_id_for(&self, speed: Speed) -> String {
        let _ = speed;
        "unknown".to_string()
    }

    fn capabilities(&self) -> ProviderCapabilities;
}

// ─── 2. Routing ────────────────────────────────────────────────

#[async_trait]
pub trait Router: Send + Sync {
    async fn classify(
        &self,
        message: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<RoutingOutcome>;
}

// ─── 3. Planning ───────────────────────────────────────────────

#[async_trait]
pub trait Planner: Send + Sync {
    async fn plan(
        &self,
        goal: &str,
        context: &ConversationContext,
        available_tools: &[ToolDescriptor],
    ) -> Result<Plan>;

    async fn replan(
        &self,
        original: &Plan,
        completed: &[(usize, StepOutput)],
        failure: &StepError,
    ) -> Result<Plan>;
}

// ─── 4. Tool Execution ────────────────────────────────────────

#[async_trait]
pub trait Tool: Send + Sync {
    fn descriptor(&self) -> ToolDescriptor;
    fn required_permissions(&self) -> Vec<Permission>;

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput>;

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        let _ = params;
        Ok(())
    }

    /// Retry configuration for transient failures.
    /// Returns None for tools that should not retry (e.g., email send).
    fn retry_config(&self) -> Option<RetryConfig> {
        None
    }
}

// ─── 5. Storage ────────────────────────────────────────────────

#[async_trait]
pub trait StateStore: Send + Sync {
    // Conversations
    async fn save_message(&self, msg: &Message) -> Result<()>;
    async fn get_conversation(&self, id: &str) -> Result<Conversation>;
    async fn list_conversations(&self, limit: usize, offset: usize) -> Result<Vec<Conversation>>;
    async fn search_messages(&self, query: &str) -> Result<Vec<Message>>;
    async fn delete_conversation(&self, id: &str) -> Result<()>;

    // Tasks
    async fn save_task(&self, task: &Task) -> Result<()>;
    async fn get_task(&self, id: &str) -> Result<Task>;

    // Memory
    async fn save_memory(&self, memory: &Memory) -> Result<()>;
    async fn get_relevant_memories(&self, context: &str, limit: usize) -> Result<Vec<Memory>>;
    async fn get_all_memories(&self) -> Result<Vec<Memory>>;
    async fn delete_memory(&self, id: &str) -> Result<()>;
    async fn update_memory_confidence(&self, id: &str, confidence: f64) -> Result<()>;
    async fn touch_memory(&self, id: &str, timestamp: i64) -> Result<()>;

    // Routing log
    async fn log_routing(
        &self,
        message_hash: &str,
        classified_as: &str,
        latency_ms: i64,
    ) -> Result<()>;
    /// Attach metacognition fields to a routing_log row written by `log_routing`.
    /// Default no-op so existing implementations compile without changes.
    async fn log_routing_meta(
        &self,
        message_hash: &str,
        coarse_intent: &str,
        self_assessment: Option<&str>,
    ) -> Result<()> {
        let _ = (message_hash, coarse_intent, self_assessment);
        Ok(())
    }
    async fn get_routing_corrections(&self, limit: usize) -> Result<Vec<RoutingCorrection>>;
    async fn mark_routing_correct(&self, message_hash: &str, was_correct: bool) -> Result<()>;

    // Documents (RAG)
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()>;
    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>>;
    async fn search_documents_scored(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunk>> {
        let chunks = self
            .search_documents(query_embedding, query_text, limit)
            .await?;
        Ok(chunks
            .into_iter()
            .map(|c| ScoredChunk { chunk: c, score: 0.0 })
            .collect())
    }
    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>>;
    async fn delete_chunks_by_corpus(&self, corpus_id: &str) -> Result<u64>;
    async fn list_sources(&self) -> Result<Vec<String>>;

    // Corpus state
    async fn save_corpus_state(&self, state: &CorpusState) -> Result<()>;
    async fn get_corpus_state(&self, corpus_id: &str) -> Result<CorpusState>;
    async fn list_corpus_states(&self) -> Result<Vec<CorpusState>>;
    async fn delete_corpus_state(&self, corpus_id: &str) -> Result<()>;

    // Vector index readiness
    async fn set_vector_index_ready(&self, corpus_id: &str, ready: bool) -> Result<()>;
    async fn get_vector_index_ready(&self, corpus_id: &str) -> Result<bool>;

    // Search budget
    async fn get_search_budget(&self, backend: &str) -> Result<Option<SearchBudget>>;
    async fn update_search_budget(&self, budget: &SearchBudget) -> Result<()>;

    // Permissions
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>>;
    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()>;

    // Health
    async fn save_health_report(
        &self,
        report: &crate::health::HealthReport,
    ) -> Result<()> {
        let _ = report;
        Ok(())
    }
    async fn save_pending_decision(
        &self,
        d: &crate::health::PendingDecision,
    ) -> Result<()> {
        let _ = d;
        Ok(())
    }
    async fn list_pending_decisions(
        &self,
    ) -> Result<Vec<crate::health::PendingDecision>> {
        Ok(vec![])
    }
    async fn resolve_pending_decision(
        &self,
        id: i64,
        chosen: crate::health::RepairKind,
    ) -> Result<()> {
        let _ = (id, chosen);
        Ok(())
    }
}

// ─── Approval Channel ─────────────────────────────────────────

#[async_trait]
pub trait ApprovalChannel: Send + Sync {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool>;
    async fn ask_user(&self, question: &str) -> Result<String>;
    fn emit_progress(&self, step: &Step, output: &StepOutput);
}
