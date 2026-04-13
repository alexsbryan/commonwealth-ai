//! Generalized document operation tool.
//!
//! Runs any user-described operation across an uploaded document via
//! map-reduce. The planner writes the map and reduce prompts; this tool
//! executes them and persists the output in a [`DocumentSession`] so
//! follow-up questions can reference results without re-running.
//!
//! The existing [`DocumentTool`](crate::document::DocumentTool) handles
//! the hardcoded "summarize"/"analyze" operations. This tool generalizes
//! to arbitrary operations described in natural language.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::*;
// ToolExample is part of types::* but explicit for clarity.

/// Chunks per map batch. Larger batches = fewer inference calls but more
/// tokens per call. At 8 chunks (~4000 tokens input), a 0.6B fast slot
/// processes each batch in ~3 seconds. 764 chunks / 8 = 96 batches ≈ 5 min.
const CHUNKS_PER_BATCH: usize = 8;
const REDUCE_BATCH_SIZE: usize = 8;
const MAX_REDUCE_DEPTH: usize = 5;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub struct DocumentOperationTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl DocumentOperationTool {
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }

    /// Map phase: apply the map prompt to batches of chunks.
    async fn map_chunks(
        &self,
        chunks: &[DocumentChunk],
        map_prompt: &str,
    ) -> Result<Vec<String>> {
        let batches: Vec<&[DocumentChunk]> = chunks.chunks(CHUNKS_PER_BATCH).collect();
        let total_batches = batches.len();
        let mut fragments = Vec::with_capacity(total_batches);
        let map_start = std::time::Instant::now();

        for (i, batch) in batches.iter().enumerate() {
            let passage: String = batch
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");

            let prompt = format!(
                "{map_prompt}\n\nPassage ({} of {total_batches}):\n{passage}\n\n\
                 Return only JSON. If nothing relevant appears in this \
                 passage, return null.",
                i + 1,
            );

            let request = CompletionRequest {
                prompt,
                system_message: Some(format!(
                    "You are processing section {} of {} from a document. \
                     Follow the extraction instructions precisely.",
                    i + 1,
                    total_batches,
                )),
                preferred_speed: Speed::Fast,
                max_tokens: Some(512),
                temperature: Some(0.0), // deterministic extraction
                structured_output: None,
                think_budget: Some(0), // no thinking — pure extraction
                top_k: None,
                top_p: None,
                oicp: None,
            };

            let response = self.inference.complete(&request).await?;

            // Skip null/empty responses (passage had nothing relevant).
            let text = response.text.trim().to_string();
            if text != "null" && !text.is_empty() {
                fragments.push(text);
            }

            let elapsed = map_start.elapsed().as_secs();
            let rate = if elapsed > 0 { (i + 1) as f32 / elapsed as f32 } else { 0.0 };
            let eta = if rate > 0.0 { ((total_batches - i - 1) as f32 / rate) as u64 } else { 0 };
            eprintln!(
                "  [document_operation] Map batch {}/{total_batches} ({} chunks) | {:.1} batch/s | ETA {}m{}s",
                i + 1,
                batch.len(),
                rate,
                eta / 60,
                eta % 60,
            );
        }

        Ok(fragments)
    }

    /// Hierarchical reduce: merge fragments in batches, then merge the
    /// intermediate results. Recurses for very large documents.
    fn hierarchical_reduce<'a>(
        &'a self,
        fragments: &'a [String],
        reduce_prompt: &'a str,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            if depth > MAX_REDUCE_DEPTH {
                return Err(Error::Execution(
                    "Map-reduce exceeded maximum recursion depth".to_string(),
                ));
            }

            if fragments.len() <= REDUCE_BATCH_SIZE {
                return self.reduce_once(fragments, reduce_prompt).await;
            }

            eprintln!(
                "  [document_operation] Reduce pass {} ({} fragments)",
                depth + 1,
                fragments.len(),
            );

            // First pass: reduce in batches.
            let mut intermediate = Vec::new();
            for batch in fragments.chunks(REDUCE_BATCH_SIZE) {
                let merged = self.reduce_once(batch, reduce_prompt).await?;
                intermediate.push(merged);
            }

            // Recurse on intermediate results.
            self.hierarchical_reduce(&intermediate, reduce_prompt, depth + 1)
                .await
        })
    }

    async fn reduce_once(
        &self,
        fragments: &[String],
        reduce_prompt: &str,
    ) -> Result<String> {
        let combined = fragments.join("\n\n---\n\n");
        let prompt = format!(
            "{reduce_prompt}\n\nFragments:\n{combined}\n\n\
             Produce the merged JSON. Resolve conflicts in favour of \
             the more specific or later-occurring information."
        );

        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are merging extraction results from multiple document sections. \
                 Produce a coherent, deduplicated final output."
                    .to_string(),
            ),
            preferred_speed: Speed::Slow,
            max_tokens: Some(1024),
            temperature: Some(0.3),
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
        };

        let response = self.inference.complete(&request).await?;
        Ok(response.text)
    }
}

#[async_trait]
impl Tool for DocumentOperationTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "document_operation".to_string(),
            name: "Document Operation".to_string(),
            description: "Run a user-defined map-reduce operation across a document. \
                          The map_prompt and reduce_prompt are written by the planner \
                          based on the user's operation description."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "The source path of the ingested document"
                    },
                    "operation": {
                        "type": "string",
                        "description": "The user's operation request in natural language"
                    },
                    "map_prompt": {
                        "type": "string",
                        "description": "Prompt applied to each chunk batch to extract relevant information"
                    },
                    "reduce_prompt": {
                        "type": "string",
                        "description": "Prompt applied to merge extracted fragments into a final result"
                    },
                    "conversation_id": {
                        "type": "string",
                        "description": "Conversation ID for session persistence"
                    },
                    "batch_size": {
                        "type": "integer",
                        "description": "Chunks per map batch (default: 4)"
                    }
                },
                "required": ["source", "operation", "map_prompt", "reduce_prompt"]
            }),
            examples: vec![
                ToolExample {
                    situation: "User wants to extract character arcs from a novel".to_string(),
                    call: serde_json::json!({
                        "source": "manuscript.pdf",
                        "operation": "extract character arcs",
                        "map_prompt": "From this section, extract: character names, key actions they take, and how they change.",
                        "reduce_prompt": "Synthesize all character information into a comprehensive character map with arcs."
                    }),
                },
            ],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        for field in &["source", "operation", "map_prompt", "reduce_prompt"] {
            if params.get(*field).and_then(|v| v.as_str()).is_none() {
                return Err(Error::InvalidInput(format!(
                    "document_operation requires '{field}'"
                )));
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let source = params["source"].as_str().unwrap();
        let operation = params["operation"].as_str().unwrap();
        let map_prompt = params["map_prompt"].as_str().unwrap();
        let reduce_prompt = params["reduce_prompt"].as_str().unwrap();
        let conversation_id = params
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.conversation_id);

        // 1. Retrieve all chunks for this source.
        tracing::info!(source = source, "document_operation: looking up chunks");
        let chunks = self.store.get_chunks_by_source(source).await?;
        tracing::info!(source = source, chunks = chunks.len(), "document_operation: exact match result");

        if chunks.is_empty() {
            // Try fuzzy match on filename. The planner may introduce typos
            // or case differences in the source name.
            let sources = self.store.list_sources().await?;
            let source_lower = source.to_lowercase();
            let matching: Vec<&str> = sources
                .iter()
                .filter(|s| {
                    let sl = s.to_lowercase();
                    sl.contains(&source_lower)
                        || source_lower.contains(&sl)
                        || sl.ends_with(&source_lower)
                        // Fuzzy: check if most words from the source appear in the stored name.
                        || source_lower.split_whitespace()
                            .filter(|w| w.len() > 2)
                            .filter(|w| sl.contains(w))
                            .count() >= 2
                })
                .map(|s| s.as_str())
                .collect();

            tracing::info!(
                source = source,
                available = ?sources,
                matched = ?matching,
                "document_operation: fuzzy match results"
            );

            if matching.is_empty() {
                return Ok(StepOutput::Text(format!(
                    "No document found with source '{source}'. Available: {}",
                    if sources.is_empty() {
                        "none".to_string()
                    } else {
                        sources.join(", ")
                    }
                )));
            }

            // Use the first match (best guess).
            if !matching.is_empty() {
                let full_source = matching[0].to_string();
                let chunks = self.store.get_chunks_by_source(&full_source).await?;
                tracing::info!(
                    full_source = %full_source,
                    chunks = chunks.len(),
                    "document_operation: fuzzy match found chunks"
                );
                return self
                    .run_operation(
                        &chunks,
                        &full_source,
                        operation,
                        map_prompt,
                        reduce_prompt,
                        conversation_id,
                    )
                    .await;
            }

            // Unreachable — matching.is_empty() handled above.
            return Ok(StepOutput::Text(format!(
                "No documents match '{source}'. Available: {}",
                matching.join(", ")
            )));
        }

        self.run_operation(
            &chunks,
            source,
            operation,
            map_prompt,
            reduce_prompt,
            conversation_id,
        )
        .await
    }
}

impl DocumentOperationTool {
    async fn run_operation(
        &self,
        chunks: &[DocumentChunk],
        source: &str,
        operation: &str,
        map_prompt: &str,
        reduce_prompt: &str,
        conversation_id: &str,
    ) -> Result<StepOutput> {
        let word_count: usize = chunks.iter().map(|c| c.content.split_whitespace().count()).sum();

        eprintln!(
            "[document_operation] '{}': {} chunks, ~{} words, operation: {}",
            source,
            chunks.len(),
            word_count,
            operation,
        );

        // Small document: single pass with map prompt (no reduce needed).
        let output = if chunks.len() <= CHUNKS_PER_BATCH {
            let full_text: String = chunks
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");

            let prompt = format!(
                "{map_prompt}\n\nDocument:\n{full_text}\n\nReturn JSON."
            );

            let request = CompletionRequest {
                prompt,
                system_message: Some(format!(
                    "You are processing the document \"{source}\". \
                     Follow the extraction instructions precisely."
                )),
                preferred_speed: Speed::Slow,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                structured_output: None,
                think_budget: None,
                top_k: None,
                top_p: None,
                oicp: None,
            };

            self.inference.complete(&request).await?.text
        } else {
            // Map-reduce for larger documents.
            let fragments = self.map_chunks(chunks, map_prompt).await?;

            if fragments.is_empty() {
                return Ok(StepOutput::Text(
                    "No relevant content found for this operation in the document.".to_string(),
                ));
            }

            self.hierarchical_reduce(&fragments, reduce_prompt, 0)
                .await?
        };

        // Persist to document session.
        let filename = source
            .rsplit('/')
            .next()
            .unwrap_or(source)
            .to_string();

        let session = DocumentSession {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            filename: filename.clone(),
            source: source.to_string(),
            word_count,
            chunk_count: chunks.len(),
            created_at: now(),
            operation: operation.to_string(),
            map_prompt: map_prompt.to_string(),
            reduce_prompt: reduce_prompt.to_string(),
            last_output: Some(output.clone()),
            history: vec![DocumentOperation {
                description: operation.to_string(),
                output: output.clone(),
                completed_at: now(),
            }],
        };

        // Check if there's an existing session for this conversation.
        if let Ok(Some(mut existing)) = self
            .store
            .get_document_session_by_conversation(conversation_id)
            .await
        {
            // Append to existing session.
            existing.operation = operation.to_string();
            existing.map_prompt = map_prompt.to_string();
            existing.reduce_prompt = reduce_prompt.to_string();
            existing.last_output = Some(output.clone());
            existing.history.push(DocumentOperation {
                description: operation.to_string(),
                output: output.clone(),
                completed_at: now(),
            });
            let _ = self.store.update_document_session(&existing).await;
        } else {
            let _ = self.store.create_document_session(&session).await;
        }

        Ok(StepOutput::Text(output))
    }
}
