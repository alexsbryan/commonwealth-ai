use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::*;

/// Search over ingested documents using vector similarity or text search.
pub struct KnowledgeTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl KnowledgeTool {
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }
}

#[async_trait]
impl Tool for KnowledgeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "knowledge".to_string(),
            name: "Knowledge".to_string(),
            description: "Search your ingested documents for relevant information".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![] // No special permissions needed to search own documents.
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Knowledge tool requires a 'query' string parameter".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'query' parameter".to_string()))?;

        // Try vector search via embeddings first.
        let chunks = match self.inference.embed(query).await {
            Ok(embedding) => {
                self.store
                    .search_documents(&embedding, query, 5)
                    .await
                    .unwrap_or_default()
            }
            Err(_) => {
                // Embedding not available — fall back to text search.
                Vec::new()
            }
        };

        // If vector search returned nothing, try FTS text search.
        let chunks = if chunks.is_empty() {
            // Use message search as a proxy for document search
            // (documents aren't in FTS5 yet — that's Phase 7).
            let messages = self.store.search_messages(query).await.unwrap_or_default();
            if messages.is_empty() {
                return Ok(StepOutput::Text(
                    "No relevant documents or messages found.".to_string(),
                ));
            }
            // Format message results as document-like output.
            let results: Vec<String> = messages
                .iter()
                .take(5)
                .map(|m| format!("[{}] {}", m.role_str(), &m.content[..m.content.len().min(500)]))
                .collect();
            return Ok(StepOutput::Text(results.join("\n\n")));
        } else {
            chunks
        };

        // Format document chunks.
        let results: Vec<String> = chunks
            .iter()
            .map(|c| format!("[{}] {}", c.source, &c.content[..c.content.len().min(500)]))
            .collect();

        Ok(StepOutput::Text(results.join("\n\n")))
    }
}
