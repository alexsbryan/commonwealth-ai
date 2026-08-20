// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::slot_policy::Workload;
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::*;

const CHUNKS_PER_BATCH: usize = 4;
const MAX_REDUCE_INPUT_CHARS: usize = 8192;

/// Process entire documents via map-reduce.
/// Handles documents of any size — from a single page to a full book.
///
/// Operations:
/// - "summarize": produce a comprehensive summary of the entire document
/// - "analyze": identify key themes, arguments, and structure
pub struct DocumentTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl DocumentTool {
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }

    /// Map phase: process chunks in batches, producing a summary for each batch.
    async fn map_chunks(&self, chunks: &[DocumentChunk], operation: &str) -> Result<Vec<String>> {
        let map_prompt = match operation {
            "analyze" => "Identify the key themes, arguments, and structure in the following text section. Be specific and cite details.",
            _ => "Summarize the following text section. Preserve key facts, names, and conclusions. Be concise but thorough.",
        };

        let mut batch_summaries = Vec::new();

        for (batch_idx, batch) in chunks.chunks(CHUNKS_PER_BATCH).enumerate() {
            let batch_text: String = batch
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            // SLOT_POLICY §3 Housekeep: document map-phase section
            // summarize/analyze — advisory context feeding the reduce,
            // not durable truth.
            let mut request = Workload::Housekeep
                .request(format!("{map_prompt}\n\n---\n\n{batch_text}"))
                .with_system(&format!(
                    "You are processing section {} of a larger document. {map_prompt}",
                    batch_idx + 1,
                ))
                .with_output_budget(512);
            request.temperature = Some(0.3);
            // POLICY-DEBT(SLOT_POLICY §3 Housekeep): None preserved for P1
            // neutrality (bundle is Some(0)); P5 confirms.
            request.think_budget = None;

            let response = self.inference.complete(&request).await?;
            batch_summaries.push(response.text);

            eprintln!(
                "  [document] Processed batch {}/{} ({} chunks)",
                batch_idx + 1,
                chunks.len().div_ceil(CHUNKS_PER_BATCH),
                batch.len(),
            );
        }

        Ok(batch_summaries)
    }

    /// Reduce phase: synthesize batch summaries into a final result.
    /// Recurses if the combined summaries are still too large for one prompt.
    fn reduce_summaries<'a>(
        &'a self,
        summaries: Vec<String>,
        operation: &'a str,
        source: &'a str,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            if depth > 5 {
                return Err(Error::Execution(
                    "Map-reduce exceeded maximum recursion depth".to_string(),
                ));
            }

            let combined: String = summaries.join("\n\n---\n\n");

            if combined.len() <= MAX_REDUCE_INPUT_CHARS {
                // Fits in one prompt — produce final synthesis.
                let reduce_prompt = match operation {
                "analyze" => format!(
                    "You have been given section-by-section analyses of the document \"{source}\". \
                     Synthesize these into a comprehensive analysis covering: key themes, main arguments, \
                     structure, and notable details.\n\nSection analyses:\n{combined}"
                ),
                _ => format!(
                    "You have been given section-by-section summaries of the document \"{source}\". \
                     Synthesize these into a single, comprehensive summary that captures all the key \
                     points in a coherent narrative.\n\nSection summaries:\n{combined}"
                ),
            };

                // SLOT_POLICY §3 Synthesize: final reduce composing a
                // coherent document summary/analysis for the user.
                let mut request = Workload::Synthesize
                    .request(reduce_prompt)
                    .with_system(
                        "You are synthesizing a final summary from section summaries. \
                     Produce a coherent, comprehensive result.",
                    )
                    .with_output_budget(1024);
                request.temperature = Some(0.5);

                let response = self.inference.complete(&request).await?;
                return Ok(response.text);
            }

            // Too large — recurse: re-summarize the summaries in batches.
            eprintln!(
                "  [document] Reduce pass {} ({} summaries, {} chars)",
                depth + 1,
                summaries.len(),
                combined.len(),
            );

            // Create synthetic chunks from the summaries for re-processing.
            let synthetic_chunks: Vec<DocumentChunk> = summaries
                .iter()
                .enumerate()
                .map(|(i, s)| DocumentChunk {
                    id: format!("reduce-{depth}-{i}"),
                    source: source.to_string(),
                    content: s.clone(),
                    chunk_index: i,
                    embedding: None,
                    created_at: 0,
                    source_type: SourceType::UserDocument,
                    version: 0,
                    deleted_at: None,
                })
                .collect();

            let new_summaries = self.map_chunks(&synthetic_chunks, operation).await?;
            self.reduce_summaries(new_summaries, operation, source, depth + 1)
                .await
        }) // Box::pin(async move {
    }
}

#[async_trait]
impl Tool for DocumentTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("document").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("document")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("source").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Document tool requires a 'source' parameter".to_string(),
            ));
        }
        let operation = params
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("summarize");
        if !["summarize", "analyze"].contains(&operation) {
            return Err(Error::InvalidInput(format!(
                "Unknown operation: {operation}. Use 'summarize' or 'analyze'."
            )));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let source = params
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'source' parameter".to_string()))?;

        let operation = params
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("summarize");

        // 1. Retrieve all chunks for this source.
        let chunks = self.store.get_chunks_by_source(source).await?;

        if chunks.is_empty() {
            // Try partial match on filename.
            let sources = self.store.list_sources().await?;
            let matching: Vec<&str> = sources
                .iter()
                .filter(|s| s.contains(source) || s.ends_with(source))
                .map(|s| s.as_str())
                .collect();

            if matching.is_empty() {
                return Ok(StepOutput::Text(format!(
                    "No document found with source '{source}'. Available documents: {}",
                    if sources.is_empty() {
                        "none (use --ingest to add documents)".to_string()
                    } else {
                        sources.join(", ")
                    }
                )));
            }

            if matching.len() == 1 {
                // Exact match on partial path — retry with full path.
                let full_source = matching[0];
                let chunks = self.store.get_chunks_by_source(full_source).await?;
                return self.process_chunks(&chunks, operation, full_source).await;
            }

            return Ok(StepOutput::Text(format!(
                "Multiple documents match '{source}': {}",
                matching.join(", ")
            )));
        }

        self.process_chunks(&chunks, operation, source).await
    }
}

impl DocumentTool {
    async fn process_chunks(
        &self,
        chunks: &[DocumentChunk],
        operation: &str,
        source: &str,
    ) -> Result<StepOutput> {
        eprintln!(
            "[document] Processing '{}': {} chunks, operation={}",
            source,
            chunks.len(),
            operation,
        );

        if chunks.len() <= CHUNKS_PER_BATCH {
            // Small document — process directly without map-reduce.
            let full_text: String = chunks
                .iter()
                .map(|c| c.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");

            let prompt = match operation {
                "analyze" => format!("Analyze the following document. Identify key themes, arguments, and structure.\n\n{full_text}"),
                _ => format!("Summarize the following document comprehensively.\n\n{full_text}"),
            };

            // SLOT_POLICY §3 Synthesize: small-document direct
            // summary/analysis composed for the user (no map-reduce).
            let mut request = Workload::Synthesize
                .request(prompt)
                .with_system(&format!(
                    "You are processing the document \"{source}\". Provide a thorough {operation}."
                ))
                .with_output_budget(1024);
            request.temperature = Some(0.5);

            let response = self.inference.complete(&request).await?;
            return Ok(StepOutput::Text(response.text));
        }

        // Map-reduce for large documents.
        let batch_summaries = self.map_chunks(chunks, operation).await?;
        let final_result = self
            .reduce_summaries(batch_summaries, operation, source, 0)
            .await?;

        Ok(StepOutput::Text(final_result))
    }
}
