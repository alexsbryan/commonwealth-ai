use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::*;

/// Search over ingested documents using vector similarity or text search.
pub struct KnowledgeTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
    corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
}

impl KnowledgeTool {
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self {
            store,
            inference,
            corpus_engine: None,
        }
    }

    /// Set an optional corpus engine for searching local corpus indexes.
    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
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
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Prose synthesis over local knowledge corpora, with \
                                inline citations to source chunks."
            })),
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
        let embedding = self.inference.embed(query).await.ok();

        // ── Search user documents via StateStore ──────────────
        let store_chunks: Vec<(String, String, f32)> = if let Some(ref emb) = embedding {
            self.store
                .search_documents_scored(emb, query, 5)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|sc| (sc.chunk.source, sc.chunk.content, sc.score))
                .collect()
        } else {
            Vec::new()
        };

        // ── Search corpus-engine indexes ─────────────────────
        let corpus_chunks: Vec<(String, String, f32)> =
            if let (Some(ref engine), Some(ref emb)) = (&self.corpus_engine, &embedding) {
                let mut results = Vec::new();
                if let Ok(indexes) = engine.installed_indexes().await {
                    for info in &indexes {
                        match engine.open_index(&info.path).await {
                            Ok(idx) => {
                                if let Ok(scored) = idx.search(emb, query, 5).await {
                                    for sc in scored {
                                        let source = sc.title.unwrap_or(sc.corpus_id);
                                        results.push((source, sc.content, sc.score));
                                    }
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                }
                results
            } else {
                Vec::new()
            };

        // ── Merge, sort by score, truncate ───────────────────
        let mut all: Vec<(String, String, f32)> = store_chunks;
        all.extend(corpus_chunks);

        if all.is_empty() {
            // Fall back to FTS message search.
            let messages = self.store.search_messages(query).await.unwrap_or_default();
            if messages.is_empty() {
                return Ok(StepOutput::Text(
                    "No relevant documents or messages found.".to_string(),
                ));
            }
            let results: Vec<String> = messages
                .iter()
                .take(5)
                .map(|m| format!("[{}] {}", m.role_str(), &m.content[..m.content.len().min(500)]))
                .collect();
            return Ok(StepOutput::Text(results.join("\n\n")));
        }

        // Sort descending by score.
        all.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(10);

        // Format results.
        let results: Vec<String> = all
            .iter()
            .map(|(source, content, _score)| {
                format!("[{}] {}", source, &content[..content.len().min(500)])
            })
            .collect();

        Ok(StepOutput::Text(results.join("\n\n")))
    }
}
