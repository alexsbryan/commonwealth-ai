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

/// Chunks per map batch. Smaller batches = more calls but each call
/// is disproportionately faster (attention scales with sequence length).
/// 4 chunks × ~512 tokens = ~2048 tokens input — fast on the 9B model.
const CHUNKS_PER_BATCH: usize = 4;
/// Number of map batches to dispatch concurrently.
/// Against embedded inference: serializes (same speed as sequential).
/// Against a remote server with --parallel N: N batches run simultaneously.
const N_PARALLEL: usize = 4;
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
    /// Dispatches N_PARALLEL batches concurrently via `complete_batch`.
    /// Against embedded inference this serializes (same speed).
    /// Against a remote server with --parallel N, batches run simultaneously.
    async fn map_chunks(
        &self,
        chunks: &[DocumentChunk],
        map_prompt: &str,
    ) -> Result<Vec<String>> {
        let batches: Vec<&[DocumentChunk]> = chunks.chunks(CHUNKS_PER_BATCH).collect();
        let total_batches = batches.len();
        let total_groups = (total_batches + N_PARALLEL - 1) / N_PARALLEL;
        let mut fragments = Vec::with_capacity(total_batches);
        let map_start = std::time::Instant::now();
        let mut batches_done = 0usize;

        eprintln!(
            "  [document_operation] Map: {} batches in {} groups of {} (parallel dispatch)",
            total_batches, total_groups, N_PARALLEL,
        );

        for (group_idx, group) in batches.chunks(N_PARALLEL).enumerate() {
            // Build requests for all batches in this group.
            let requests: Vec<CompletionRequest> = group
                .iter()
                .enumerate()
                .map(|(i, batch)| {
                    let batch_idx = group_idx * N_PARALLEL + i;
                    let passage: String = batch
                        .iter()
                        .map(|c| c.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n");

                    // Minimal prompt — every token of instruction prefix is
                    // prefill overhead multiplied by batch_count. No system
                    // message (saves ~50 tokens of template per call).
                    CompletionRequest {
                        prompt: format!(
                            "{map_prompt}\n\nPassage:\n{passage}\n\nExtract relevant info. If nothing relevant, respond: null",
                        ),
                        system_message: None,
                        preferred_speed: Speed::Fast,
                        max_tokens: Some(384),
                        temperature: Some(0.0),
                        structured_output: None,
                        think_budget: Some(0),
                        top_k: None,
                        top_p: None,
                        oicp: None,
                    }
                })
                .collect();

            let group_size = requests.len();

            // Dispatch concurrently via complete_batch.
            let responses = self.inference.complete_batch(&requests).await?;

            if batches_done == 0 {
                if let Some(first) = responses.first() {
                    eprintln!(
                        "  [document_operation] First batch completed. Model: {}, latency: {}ms",
                        first.model_id,
                        first.latency_ms,
                    );
                }
            }

            for response in responses {
                let text = response.text.trim().to_string();
                if text != "null" && !text.is_empty() {
                    fragments.push(text);
                }
            }

            batches_done += group_size;
            let elapsed = map_start.elapsed().as_secs();
            let rate = if elapsed > 0 { batches_done as f32 / elapsed as f32 } else { 0.0 };
            let remaining = total_batches - batches_done;
            let eta = if rate > 0.0 { (remaining as f32 / rate) as u64 } else { 0 };
            eprintln!(
                "  [document_operation] Map group {}/{} ({} batches) | {:.1} batch/s | ETA {}m{}s",
                group_idx + 1,
                total_groups,
                group_size,
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
                eprintln!(
                    "  [document_operation] Final reduce (depth {}, {} fragments) — using primary model",
                    depth, fragments.len(),
                );
                // Final merge uses the primary model for quality synthesis.
                return self.reduce_final(fragments, reduce_prompt).await;
            }

            eprintln!(
                "  [document_operation] Reduce pass {} ({} fragments)",
                depth + 1,
                fragments.len(),
            );

            // First pass: reduce in batches.
            let reduce_batches: Vec<&[String]> = fragments.chunks(REDUCE_BATCH_SIZE).collect();
            let total_reduce = reduce_batches.len();
            let reduce_start = std::time::Instant::now();
            let mut intermediate = Vec::new();

            for (i, batch) in reduce_batches.iter().enumerate() {
                let merged = self.reduce_once(batch, reduce_prompt).await?;
                intermediate.push(merged);

                let elapsed = reduce_start.elapsed().as_secs();
                let rate = if elapsed > 0 { (i + 1) as f32 / elapsed as f32 } else { 0.0 };
                let remaining = total_reduce - i - 1;
                let eta = if rate > 0.0 { (remaining as f32 / rate) as u64 } else { 0 };
                eprintln!(
                    "  [document_operation] Reduce batch {}/{} (depth {}) | {:.1}/s | ETA {}m{}s",
                    i + 1, total_reduce, depth, rate, eta / 60, eta % 60,
                );
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
        // Minimal prompt — no system message, no extra instructions.
        let prompt = format!(
            "Merge these extracted notes. Deduplicate, organize, keep all details.\n\n\
             {combined}"
        );

        let request = CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(1024),
            temperature: Some(0.0),
            structured_output: None,
            think_budget: Some(0),
            top_k: None,
            top_p: None,
            oicp: None,
        };

        let response = self.inference.complete(&request).await?;
        Ok(response.text)
    }

    /// Final reduce — uses the primary model for quality synthesis.
    /// This produces the user-facing response, so quality matters more
    /// than speed here.
    async fn reduce_final(
        &self,
        fragments: &[String],
        reduce_prompt: &str,
    ) -> Result<String> {
        let combined = fragments.join("\n\n---\n\n");
        let prompt = format!(
            "{reduce_prompt}\n\nFragments:\n{combined}\n\n\
             Produce a comprehensive, well-organized final answer."
        );

        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are producing the final synthesis of a document analysis. \
                 Be thorough, well-organized, and cite specific details."
                    .to_string(),
            ),
            preferred_speed: Speed::Slow,
            max_tokens: Some(2048),
            temperature: Some(0.3),
            structured_output: None,
            think_budget: None, // allow thinking for the final synthesis
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
