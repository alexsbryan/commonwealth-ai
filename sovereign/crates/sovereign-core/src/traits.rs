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
    ) -> Result<Intent>;
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

    // Documents (RAG)
    async fn store_chunks(&self, chunks: &[DocumentChunk]) -> Result<()>;
    async fn search_documents(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<DocumentChunk>>;
    async fn get_chunks_by_source(&self, source: &str) -> Result<Vec<DocumentChunk>>;
    async fn list_sources(&self) -> Result<Vec<String>>;

    // Permissions
    async fn get_permission(&self, tool_id: &str, scope: &str) -> Result<Option<bool>>;
    async fn set_permission(&self, tool_id: &str, scope: &str, granted: bool) -> Result<()>;
}

// ─── Approval Channel ─────────────────────────────────────────

#[async_trait]
pub trait ApprovalChannel: Send + Sync {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool>;
    async fn ask_user(&self, question: &str) -> Result<String>;
    fn emit_progress(&self, step: &Step, output: &StepOutput);
}
