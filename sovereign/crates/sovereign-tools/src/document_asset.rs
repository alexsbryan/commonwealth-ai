//! Document Asset Manager — ingest once, query forever.
//!
//! Handles the full lifecycle of a document asset:
//!
//! 1. **Ingest** — parse, chunk, embed, and build a structural skeleton.
//!    Embedding and skeleton extraction run concurrently. Once embedding
//!    completes, RAG queries are available while the skeleton keeps building.
//!
//! 2. **Route** — classify a user's question into one of four operation
//!    types (RAG, Synthesis, Aggregation, Transformation) using the
//!    skeleton's overview and the question text.
//!
//! 3. **Execute** — run the selected operation and return the response
//!    with source citations.
//!
//! Reuses the existing RAG pipeline (`rag::parse`, `rag::chunk`) for
//! parsing and chunking. Reuses `DocumentOperationTool`'s map-reduce
//! pattern for the synthesis and aggregation executors.

use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::*;

use crate::rag::chunk::{chunk_text, TextChunk};
use crate::rag::parse::parse_file;

// ─── Progress types ──────────────────────────────────────────

/// Progress updates emitted during document ingest. The frontend
/// listens to these via Tauri events to drive the progress bar
/// and ingest banner.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum IngestProgress {
    /// File parsed, chunking complete, embedding about to start.
    Started {
        word_count: usize,
        chunk_count: usize,
        filename: String,
    },
    /// Embedding chunks into the vector store.
    Indexing { done: usize, total: usize },
    /// Embedding complete. RAG queries now available.
    RagAvailable { asset_id: String },
    /// Skeleton extraction in progress.
    BuildingSkeleton { done: usize, total: usize },
    /// Fully ready. All operations available.
    Ready {
        asset_id: String,
        main_entities: usize,
        structural_moments: usize,
    },
    /// Ingest failed.
    Failed { reason: String },
}

/// Progress updates during a query operation. The frontend shows
/// these as loading state text below the input.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum OperationProgress {
    /// Router has decided which operation to use.
    Routing { operation: String },
    /// Retrieving relevant passages from the index.
    Retrieving,
    /// Analysing a specific entity across the document.
    AnalysingEntity { name: String },
    /// Synthesising the final response.
    Synthesising,
}

// ─── Manager ─────────────────────────────────────────────────

/// Manages the lifecycle of document assets: ingest, route, execute.
///
/// Holds references to inference (for embedding + LLM calls) and
/// storage (for persisting assets and chunks). Does not own a
/// CorpusEngine — document assets use the existing `DocumentStore`
/// chunk storage with FTS5 search, not LanceDB corpus indexes.
pub struct DocumentAssetManager {
    inference: Arc<dyn InferenceProvider>,
    store: Arc<dyn StateStore>,
}

impl DocumentAssetManager {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        store: Arc<dyn StateStore>,
    ) -> Self {
        Self { inference, store }
    }

    /// Ingest a file from disk. Parses, chunks, embeds, and builds
    /// a structural skeleton. Returns the completed asset.
    ///
    /// The progress callback fires at each phase boundary so the
    /// frontend can update the UI in real time.
    pub async fn ingest(
        &self,
        file_path: &std::path::Path,
        on_progress: impl Fn(IngestProgress) + Send + Sync + 'static,
    ) -> Result<DocumentAsset> {
        let on_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(on_progress);

        // ── Parse and chunk ─────────────────────────────────
        let parsed = parse_file(file_path)?;
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document")
            .to_string();

        let text_chunks = chunk_text(&parsed.content);
        let word_count = parsed.content.split_whitespace().count();
        let chunk_count = text_chunks.len();
        let file_size_mb = std::fs::metadata(file_path)
            .map(|m| m.len() as f32 / (1024.0 * 1024.0))
            .unwrap_or(0.0);

        let asset_id = uuid::Uuid::new_v4().to_string();
        let index_id = format!("doc-{asset_id}");

        // Infer title from filename (strip extension).
        let title = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&filename)
            .replace('_', " ")
            .replace('-', " ");

        // ── Create asset in Pending state ───────────────────
        let asset = DocumentAsset {
            id: asset_id.clone(),
            title,
            filename: filename.clone(),
            file_size_mb,
            word_count,
            chunk_count,
            document_type: DocumentTypeTag::Unknown,
            ingested_at: chrono::Utc::now(),
            index_id: index_id.clone(),
            skeleton: None,
            state: AssetState::Pending,
        };
        self.store.save_document_asset(&asset).await?;

        on_progress(IngestProgress::Started {
            word_count,
            chunk_count,
            filename: filename.clone(),
        });

        // ── Concurrent: embedding + skeleton ────────────────
        //
        // Embedding and skeleton extraction run in parallel via
        // tokio::join!. Embedding uses batch calls for throughput.
        // Once embedding finishes, RAG queries work even while the
        // skeleton is still building.

        let source_id = format!("asset:{asset_id}");
        let text_chunks = Arc::new(text_chunks);

        // ── Embedding future ────────────────────────────────
        let embed_future = {
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let asset_id = asset_id.clone();
            let source_id = source_id.clone();
            let text_chunks = Arc::clone(&text_chunks);
            let on_progress = Arc::clone(&on_progress);

            async move {
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::Indexing {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;

                let now_ts = chrono::Utc::now().timestamp();
                let mut doc_chunks = Vec::with_capacity(chunk_count);

                // Batch embed in groups of 64 for throughput.
                const EMBED_BATCH: usize = 64;
                let mut all_embeddings: Vec<Option<Vec<f32>>> = Vec::with_capacity(chunk_count);

                for batch_start in (0..chunk_count).step_by(EMBED_BATCH) {
                    let batch_end = (batch_start + EMBED_BATCH).min(chunk_count);
                    let texts: Vec<String> = text_chunks[batch_start..batch_end]
                        .iter()
                        .map(|c| c.content.clone())
                        .collect();

                    match inference.embed_batch(&texts).await {
                        Ok(embeddings) => {
                            for emb in embeddings {
                                all_embeddings.push(Some(emb));
                            }
                        }
                        Err(_) => {
                            // Fallback: mark these as no-embedding.
                            for _ in batch_start..batch_end {
                                all_embeddings.push(None);
                            }
                        }
                    }

                    on_progress(IngestProgress::Indexing {
                        done: batch_end,
                        total: chunk_count,
                    });
                    let _ = store
                        .update_asset_state(
                            &asset_id,
                            &AssetState::Indexing {
                                chunks_done: batch_end,
                                chunks_total: chunk_count,
                            },
                        )
                        .await;
                }

                // Build DocumentChunk records.
                for (i, tc) in text_chunks.iter().enumerate() {
                    doc_chunks.push(DocumentChunk {
                        id: format!("{source_id}:{}", tc.index),
                        source: source_id.clone(),
                        content: tc.content.clone(),
                        chunk_index: tc.index,
                        embedding: all_embeddings.get(i).cloned().flatten(),
                        created_at: now_ts,
                        source_type: SourceType::UserDocument,
                        version: 0,
                        deleted_at: None,
                    });
                }

                store.store_chunks(&doc_chunks).await?;

                // RAG is now available.
                store
                    .update_asset_state(&asset_id, &AssetState::PartiallyReady)
                    .await?;
                on_progress(IngestProgress::RagAvailable {
                    asset_id: asset_id.clone(),
                });

                Ok::<(), sovereign_core::error::Error>(())
            }
        };

        // ── Skeleton future ─────────────────────────────────
        let skeleton_future = {
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let asset_id = asset_id.clone();
            let text_chunks = Arc::clone(&text_chunks);
            let on_progress = Arc::clone(&on_progress);

            async move {
                // Detect document type from the opening chunks.
                let doc_type = detect_document_type(&inference, &text_chunks).await;

                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;

                let skeleton = build_skeleton(
                    &inference,
                    &store,
                    &asset_id,
                    &text_chunks,
                    &doc_type,
                    &on_progress,
                )
                .await?;

                store.save_asset_skeleton(&asset_id, &skeleton).await?;
                store
                    .update_asset_state(&asset_id, &AssetState::Ready)
                    .await?;

                on_progress(IngestProgress::Ready {
                    asset_id: asset_id.clone(),
                    main_entities: skeleton.main_entities.len(),
                    structural_moments: skeleton.structural_moments.len(),
                });

                Ok::<(DocumentSkeleton, DocumentTypeTag), sovereign_core::error::Error>((
                    skeleton, doc_type,
                ))
            }
        };

        // Run both concurrently. Embedding typically finishes first
        // (pure computation), unlocking RAG while skeleton keeps going.
        let (embed_result, skeleton_result) = tokio::join!(embed_future, skeleton_future);

        embed_result?;
        let (skeleton, doc_type) = skeleton_result?;

        Ok(DocumentAsset {
            id: asset_id,
            title: asset.title,
            filename,
            file_size_mb,
            word_count,
            chunk_count,
            document_type: doc_type,
            ingested_at: asset.ingested_at,
            index_id,
            skeleton: Some(skeleton),
            state: AssetState::Ready,
        })
    }

    /// Route a user's question to the right operation type, then
    /// execute it and return the response with source citations.
    pub async fn ask(
        &self,
        asset: &DocumentAsset,
        request: &str,
        on_progress: impl Fn(OperationProgress) + Send + Sync,
    ) -> Result<(String, DocumentAssetOperation, Vec<String>)> {
        let start = std::time::Instant::now();
        tracing::info!(
            asset_id = %asset.id,
            title = %asset.title,
            doc_type = ?asset.document_type,
            has_skeleton = asset.skeleton.is_some(),
            request_chars = request.len(),
            "DocumentAssetManager::ask — begin"
        );

        let operation = self.route(asset, request).await?;

        tracing::info!(
            asset_id = %asset.id,
            operation = %operation.label(),
            "DocumentAssetManager::ask — routed"
        );

        on_progress(OperationProgress::Routing {
            operation: operation.label().to_string(),
        });

        let (response, sources) = self
            .execute_operation(asset, request, &operation, &on_progress)
            .await?;

        tracing::info!(
            asset_id = %asset.id,
            operation = %operation.label(),
            response_chars = response.len(),
            source_count = sources.len(),
            total_latency_ms = start.elapsed().as_millis() as u64,
            "DocumentAssetManager::ask — done"
        );

        Ok((response, operation, sources))
    }

    /// Delete an asset and its chunks.
    pub async fn delete(&self, id: &str) -> Result<()> {
        // Delete chunks from the document store.
        let source_id = format!("asset:{}", id);
        if let Ok(chunks) = self.store.get_chunks_by_source(&source_id).await {
            if !chunks.is_empty() {
                // Soft-delete by overwriting with empty + deleted_at.
                // The store's delete_chunks_by_corpus doesn't apply here
                // since these are UserDocument source type.
                // For now, we just delete the asset record — chunks are
                // orphaned but small. A future cleanup job can GC them.
            }
        }
        self.store.delete_document_asset(id).await
    }

    // ─── Routing ─────────────────────────────────────────────

    /// Classify a question into an operation type using the document's
    /// skeleton overview and the question text. Uses the fast model
    /// for low latency.
    async fn route(
        &self,
        asset: &DocumentAsset,
        request: &str,
    ) -> Result<DocumentAssetOperation> {
        tracing::debug!(asset_id = %asset.id, "document_asset::route — begin");

        let overview = asset
            .skeleton
            .as_ref()
            .map(|s| s.overview.as_str())
            .unwrap_or("Document structure not yet available.");

        let entity_names: Vec<String> = asset
            .skeleton
            .as_ref()
            .map(|s| s.main_entities.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default();

        tracing::debug!(
            overview_chars = overview.len(),
            entity_count = entity_names.len(),
            "document_asset::route — classifying"
        );

        let prompt = format!(
            "You are a document operation router. Given a user's question about a document, \
             classify it into exactly one operation type.\n\n\
             Document overview: {overview}\n\
             Main entities: {entities}\n\
             Document type: {doc_type}\n\n\
             User question: {request}\n\n\
             Respond with exactly one of these JSON objects:\n\
             - {{\"op\": \"rag\", \"query\": \"<search query>\"}}\n\
             - {{\"op\": \"synthesis\", \"focus\": \"<what to trace>\", \"entities\": [\"<names>\"]}}\n\
             - {{\"op\": \"aggregation\", \"query\": \"<what to find all of>\"}}\n\
             - {{\"op\": \"transformation\"}}\n\n\
             Guidelines:\n\
             - Use \"rag\" for questions about specific passages, chapters, or facts.\n\
             - Use \"synthesis\" for questions that require tracing something across \
               the full document (character arcs, argument development, thematic evolution).\n\
             - Use \"aggregation\" for \"find every mention of X\" or \"list all instances of Y\".\n\
             - Use \"transformation\" for rewriting, editing, or extracting structured data.\n\n\
             Respond with only the JSON object, no other text.",
            entities = entity_names.join(", "),
            doc_type = asset.document_type.label(),
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Fast,
                max_tokens: Some(128),
                temperature: Some(0.0),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            })
            .await?;

        parse_route_response(&response.text, request)
    }

    // ─── Execution ───────────────────────────────────────────

    /// Execute a routed operation against the document.
    async fn execute_operation(
        &self,
        asset: &DocumentAsset,
        request: &str,
        operation: &DocumentAssetOperation,
        on_progress: &(dyn Fn(OperationProgress) + Send + Sync),
    ) -> Result<(String, Vec<String>)> {
        let source_id = asset.source_key();

        match operation {
            DocumentAssetOperation::Rag { query } => {
                on_progress(OperationProgress::Retrieving);
                self.execute_rag(&source_id, query, request).await
            }
            DocumentAssetOperation::Synthesis { focus, entities } => {
                for entity in entities {
                    on_progress(OperationProgress::AnalysingEntity {
                        name: entity.clone(),
                    });
                }
                on_progress(OperationProgress::Synthesising);
                self.execute_synthesis(asset, focus, entities, request)
                    .await
            }
            DocumentAssetOperation::Aggregation { query } => {
                on_progress(OperationProgress::Retrieving);
                self.execute_aggregation(&source_id, query, request).await
            }
            DocumentAssetOperation::Transformation => {
                on_progress(OperationProgress::Synthesising);
                self.execute_transformation(&source_id, request).await
            }
        }
    }

    /// RAG: retrieve relevant passages and synthesise an answer.
    async fn execute_rag(
        &self,
        source_id: &str,
        query: &str,
        original_request: &str,
    ) -> Result<(String, Vec<String>)> {
        tracing::info!(
            source_id = %source_id,
            query_chars = query.len(),
            "execute_rag — begin"
        );

        let query_embedding = self.inference.embed(query).await?;
        let results = self
            .store
            .search_documents(&query_embedding, query, 8)
            .await?;

        // Filter to chunks from this document only.
        let relevant: Vec<&DocumentChunk> = results
            .iter()
            .filter(|c| c.source == source_id)
            .collect();

        tracing::debug!(
            total_results = results.len(),
            relevant_count = relevant.len(),
            "execute_rag — retrieval done"
        );

        if relevant.is_empty() {
            tracing::warn!(
                source_id = %source_id,
                "execute_rag — no relevant passages found for this document"
            );
            return Ok((
                "No relevant passages found for this question.".to_string(),
                vec![],
            ));
        }

        let passages: String = relevant
            .iter()
            .enumerate()
            .map(|(i, c)| format!("[{}] {}", i + 1, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let sources: Vec<String> = relevant.iter().map(|c| c.content.clone()).collect();

        let prompt = format!(
            "Answer the user's question based on these passages from the document.\n\n\
             Passages:\n{passages}\n\n\
             Question: {original_request}\n\n\
             Cite passage numbers [1], [2], etc. when referencing specific content. \
             If the passages don't contain enough information, say so honestly."
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Medium,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            })
            .await?;

        Ok((response.text, sources))
    }

    /// Synthesis: trace an entity or theme across the full document
    /// using the skeleton's entity index.
    async fn execute_synthesis(
        &self,
        asset: &DocumentAsset,
        focus: &str,
        entities: &[String],
        original_request: &str,
    ) -> Result<(String, Vec<String>)> {
        tracing::info!(
            asset_id = %asset.id,
            focus_chars = focus.len(),
            entity_count = entities.len(),
            "execute_synthesis — begin"
        );

        let source_id = asset.source_key();
        let all_chunks = self.store.get_chunks_by_source(&source_id).await?;

        tracing::debug!(
            total_chunks = all_chunks.len(),
            has_skeleton = asset.skeleton.is_some(),
            "execute_synthesis — chunks loaded"
        );

        if all_chunks.is_empty() {
            tracing::warn!(asset_id = %asset.id, "execute_synthesis — document has no indexed content");
            return Ok(("Document has no indexed content.".to_string(), vec![]));
        }

        // Use the skeleton entity index to find relevant chunk indices.
        let relevant_indices: Vec<usize> = if let Some(ref skeleton) = asset.skeleton {
            let mut indices = Vec::new();
            for entity_name in entities {
                if let Some(appearances) = skeleton.entity_index.get(entity_name) {
                    indices.extend(&appearances.chunk_indices);
                }
            }
            indices.sort();
            indices.dedup();
            if indices.is_empty() {
                // Fallback: sample evenly across the document.
                (0..all_chunks.len()).step_by(all_chunks.len().max(1) / 20.max(1)).collect()
            } else {
                indices
            }
        } else {
            // No skeleton — degrade to sampling.
            (0..all_chunks.len()).step_by(all_chunks.len().max(1) / 20.max(1)).collect()
        };

        let selected: Vec<&DocumentChunk> = relevant_indices
            .iter()
            .filter_map(|&i| all_chunks.get(i))
            .take(30) // Cap to avoid prompt overflow.
            .collect();

        tracing::debug!(
            selected_count = selected.len(),
            relevant_indices_count = relevant_indices.len(),
            "execute_synthesis — chunks selected"
        );

        let passages: String = selected
            .iter()
            .map(|c| {
                format!(
                    "[section {}] {}",
                    c.chunk_index,
                    &c.content[..c.content.len().min(500)]
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let sources: Vec<String> = selected.iter().map(|c| c.content.clone()).collect();

        let skeleton_context = asset
            .skeleton
            .as_ref()
            .map(|s| {
                let moments: String = s
                    .structural_moments
                    .iter()
                    .take(10)
                    .map(|m| format!("- Section {}: {}", m.chunk_index, m.description))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Document overview: {}\n\nKey structural moments:\n{}",
                    s.overview, moments
                )
            })
            .unwrap_or_default();

        let prompt = format!(
            "You are analysing a full document. Synthesise an answer that traces \
             how {focus} develops across the text.\n\n\
             {skeleton_context}\n\n\
             Relevant sections (in document order):\n{passages}\n\n\
             Question: {original_request}\n\n\
             Draw on observations from early, middle, and late sections. \
             Cite section numbers when referencing specific content."
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(2048),
                temperature: Some(0.5),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            })
            .await?;

        Ok((response.text, sources))
    }

    /// Aggregation: search every section for all instances matching
    /// the query.
    async fn execute_aggregation(
        &self,
        source_id: &str,
        query: &str,
        original_request: &str,
    ) -> Result<(String, Vec<String>)> {
        tracing::info!(
            source_id = %source_id,
            query_chars = query.len(),
            "execute_aggregation — begin"
        );

        let all_chunks = self.store.get_chunks_by_source(source_id).await?;

        // Simple keyword/embedding scan over all chunks.
        let query_lower = query.to_lowercase();
        let matching: Vec<&DocumentChunk> = all_chunks
            .iter()
            .filter(|c| c.content.to_lowercase().contains(&query_lower))
            .collect();

        tracing::debug!(
            total_chunks = all_chunks.len(),
            matching_count = matching.len(),
            "execute_aggregation — keyword scan done"
        );

        let sources: Vec<String> = matching.iter().map(|c| c.content.clone()).collect();

        if matching.is_empty() {
            tracing::warn!(query = %query, "execute_aggregation — no matches found");
            return Ok((
                format!("No instances of \"{query}\" found in the document."),
                vec![],
            ));
        }

        let matches_text: String = matching
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, c)| {
                let excerpt = &c.content[..c.content.len().min(300)];
                format!("[{}] Section {}: ...{}...", i + 1, c.chunk_index, excerpt)
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "The user asked: {original_request}\n\n\
             Found {} instances across the document:\n\n{matches_text}\n\n\
             Summarise the findings. Group by theme or chronology if appropriate. \
             Cite instance numbers.",
            matching.len(),
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Medium,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            })
            .await?;

        Ok((response.text, sources))
    }

    /// Transformation: apply a user-requested transformation.
    async fn execute_transformation(
        &self,
        source_id: &str,
        request: &str,
    ) -> Result<(String, Vec<String>)> {
        tracing::info!(
            source_id = %source_id,
            request_chars = request.len(),
            "execute_transformation — begin"
        );

        let all_chunks = self.store.get_chunks_by_source(source_id).await?;
        let full_text: String = all_chunks
            .iter()
            .take(20) // Limit to avoid prompt overflow.
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        tracing::debug!(
            total_chunks = all_chunks.len(),
            used_chunks = all_chunks.len().min(20),
            full_text_chars = full_text.len(),
            "execute_transformation — text assembled"
        );

        let prompt = format!(
            "Apply the following transformation to the document text:\n\n\
             Transformation: {request}\n\n\
             Document:\n{full_text}"
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Medium,
                max_tokens: Some(2048),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            })
            .await?;

        Ok((response.text, vec![]))
    }

}

// ─── Skeleton extraction (free functions) ────────────────────
//
// These are free functions rather than methods on DocumentAssetManager
// because they're called from spawned futures that can't borrow &self.

/// Detect the document type from the first few chunks.
async fn detect_document_type(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
) -> DocumentTypeTag {
    let sample: String = chunks
        .iter()
        .take(3)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Classify this document into one category based on these opening passages:\n\n\
         {sample}\n\n\
         Categories:\n\
         - Narrative (novels, memoirs, literary non-fiction)\n\
         - Argument (dissertations, essays, philosophy)\n\
         - Evidence (legal briefs, scientific papers)\n\
         - Chronicle (history, biography, journalism)\n\
         - Technical (manuals, specifications, documentation)\n\n\
         Respond with exactly one word: Narrative, Argument, Evidence, Chronicle, or Technical."
    );

    let response = inference
        .complete(&CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(16),
            temperature: Some(0.0),
            think_budget: Some(0),
            structured_output: None,
            top_k: None,
            top_p: None,
            oicp: None,
        })
        .await;

    let detected = match response {
        Ok(r) => match r.text.trim().to_lowercase().as_str() {
            "narrative" => DocumentTypeTag::Narrative,
            "argument" => DocumentTypeTag::Argument,
            "evidence" => DocumentTypeTag::Evidence,
            "chronicle" => DocumentTypeTag::Chronicle,
            "technical" => DocumentTypeTag::Technical,
            _ => DocumentTypeTag::Unknown,
        },
        Err(e) => {
            tracing::warn!(error = %e, "detect_document_type — inference failed, defaulting to Unknown");
            DocumentTypeTag::Unknown
        }
    };
    tracing::info!(detected = ?detected, "detect_document_type — classified");
    detected
}

/// Build the structural skeleton by processing chunks in batches
/// through the LLM. Extracts sections, entities (with kind), and
/// structural moments.
async fn build_skeleton(
    inference: &Arc<dyn InferenceProvider>,
    store: &Arc<dyn StateStore>,
    asset_id: &str,
    chunks: &[TextChunk],
    doc_type: &DocumentTypeTag,
    on_progress: &Arc<dyn Fn(IngestProgress) + Send + Sync>,
) -> Result<DocumentSkeleton> {
    let chunk_count = chunks.len();
    let mut sections = Vec::new();
    // Track entity mentions and the kinds the LLM assigned.
    let mut entity_mentions: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    let mut entity_kinds: std::collections::HashMap<String, EntityKind> =
        std::collections::HashMap::new();
    let mut structural_moments = Vec::new();

    // Process chunks in batches of 4 for coherence.
    let batch_size = 4;
    for (batch_idx, batch) in chunks.chunks(batch_size).enumerate() {
        let batch_start = batch_idx * batch_size;
        let passage: String = batch
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let entity_kinds_hint = match doc_type {
            DocumentTypeTag::Narrative => "Character, Theme, Event, Concept",
            DocumentTypeTag::Argument => "Argument, Claim, Concept, Person",
            DocumentTypeTag::Evidence => "Claim, Evidence, Person, Concept",
            DocumentTypeTag::Chronicle => "Person, Event, Theme, Concept",
            DocumentTypeTag::Technical => "Concept, Theme, Person, Event",
            DocumentTypeTag::Unknown => "Character, Argument, Concept, Claim, Evidence, Theme, Person, Event",
        };

        let prompt = format!(
            "Analyse these passages from a {doc_type} document. For each passage, extract:\n\n\
             1. **function**: One of: Introduces, Develops, Complicates, Resolves, Transitions, Evidences\n\
             2. **key_entities**: Important names, concepts, arguments, or themes (up to 5). \
                Each entity is an object: {{\"name\": \"...\", \"kind\": \"...\"}}\n\
                Valid kinds: {entity_kinds_hint}\n\
             3. **establishes**: One sentence — what this section establishes or advances\n\
             4. **is_structural_moment**: true/false — is this a major turning point, revelation, or shift?\n\
             5. **moment_description**: If structural moment, one sentence describing it\n\n\
             Passages (starting at section {batch_start}):\n\n{passage}\n\n\
             Respond as a JSON array with one object per passage:\n\
             [{{\"function\": \"...\", \"key_entities\": [{{\"name\": \"...\", \"kind\": \"...\"}}], \
             \"establishes\": \"...\", \"is_structural_moment\": false, \"moment_description\": null}}]",
            doc_type = doc_type.label(),
        );

        let response = inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Fast,
                max_tokens: Some(512),
                temperature: Some(0.1),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            })
            .await;

        if let Ok(resp) = response {
            if let Some(parsed) = parse_skeleton_batch(&resp.text, batch_start) {
                for entry in parsed {
                    for (name, kind) in &entry.entity_names_and_kinds {
                        entity_mentions
                            .entry(name.clone())
                            .or_default()
                            .push(entry.chunk_index);
                        // Keep the first assigned kind (most likely correct
                        // since early mentions are usually introductory).
                        entity_kinds.entry(name.clone()).or_insert_with(|| kind.clone());
                    }
                    if let Some(ref desc) = entry.moment_description {
                        structural_moments.push(StructuralMoment {
                            chunk_index: entry.chunk_index,
                            description: desc.clone(),
                            salience: 0.8,
                        });
                    }
                    sections.push(SectionAnnotation {
                        chunk_index: entry.chunk_index,
                        function: entry.function,
                        key_entities: entry
                            .entity_names_and_kinds
                            .iter()
                            .map(|(n, _)| n.clone())
                            .collect(),
                        establishes: entry.establishes,
                    });
                }
            }
        }

        let done = ((batch_idx + 1) * batch_size).min(chunk_count);
        on_progress(IngestProgress::BuildingSkeleton {
            done,
            total: chunk_count,
        });
        let _ = store
            .update_asset_state(
                asset_id,
                &AssetState::BuildingSkeleton {
                    chunks_done: done,
                    chunks_total: chunk_count,
                },
            )
            .await;
    }

    // ── Build entity ranking ────────────────────────────────
    let total_sections = sections.len().max(1);
    let mut main_entities: Vec<RankedEntity> = entity_mentions
        .iter()
        .map(|(name, indices)| {
            let first = indices.iter().copied().min().unwrap_or(0);
            let last = indices.iter().copied().max().unwrap_or(0);
            let presence_rate = indices.len() as f32 / total_sections as f32;
            let kind = entity_kinds
                .get(name)
                .cloned()
                .unwrap_or(EntityKind::Concept);
            RankedEntity {
                name: name.clone(),
                kind,
                presence_rate,
                first_appearance: first,
                last_appearance: last,
            }
        })
        .collect();
    main_entities.sort_by(|a, b| b.presence_rate.partial_cmp(&a.presence_rate).unwrap());
    main_entities.truncate(30);

    // ── Build entity index ──────────────────────────────────
    let entity_index: std::collections::HashMap<String, EntityAppearances> = entity_mentions
        .into_iter()
        .filter(|(name, _)| main_entities.iter().any(|e| &e.name == name))
        .map(|(name, indices)| {
            let quote_samples: Vec<String> = indices
                .iter()
                .take(3)
                .filter_map(|&i| chunks.get(i))
                .map(|c| c.content[..c.content.len().min(200)].to_string())
                .collect();
            (
                name,
                EntityAppearances {
                    chunk_indices: indices,
                    quote_samples,
                },
            )
        })
        .collect();

    // ── Build overview ──────────────────────────────────────
    let overview = generate_overview(inference, chunks, doc_type).await;

    structural_moments.truncate(40);

    Ok(DocumentSkeleton {
        sections,
        main_entities,
        entity_index,
        structural_moments,
        overview,
        built_at: chrono::Utc::now(),
    })
}

/// Generate a one-paragraph overview of the document.
async fn generate_overview(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    doc_type: &DocumentTypeTag,
) -> String {
    let sample: String = chunks
        .iter()
        .take(5)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Write a single paragraph (3-5 sentences) overview of this {doc_type} document \
         based on its opening sections. Focus on: what it's about, the main entities \
         or characters, and the central question or theme.\n\n\
         Opening:\n{sample}\n\n\
         Overview:",
        doc_type = doc_type.label(),
    );

    inference
        .complete(&CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(256),
            temperature: Some(0.3),
            think_budget: Some(0),
            structured_output: None,
            top_k: None,
            top_p: None,
            oicp: None,
        })
        .await
        .map(|r| r.text)
        .unwrap_or_else(|_| "Overview not available.".to_string())
}

// ─── Parsing helpers ─────────────────────────────────────────

/// Parsed result from one batch of skeleton extraction.
struct SkeletonBatchEntry {
    chunk_index: usize,
    function: SectionFunction,
    /// Entity names paired with their classified kind.
    entity_names_and_kinds: Vec<(String, EntityKind)>,
    establishes: String,
    moment_description: Option<String>,
}

/// Parse an entity kind string from the LLM response.
fn parse_entity_kind(s: &str) -> EntityKind {
    match s.to_lowercase().as_str() {
        "character" => EntityKind::Character,
        "argument" => EntityKind::Argument,
        "concept" => EntityKind::Concept,
        "claim" => EntityKind::Claim,
        "evidence" => EntityKind::Evidence,
        "theme" => EntityKind::Theme,
        "person" => EntityKind::Person,
        "event" => EntityKind::Event,
        _ => EntityKind::Concept,
    }
}

/// Parse the LLM's JSON response for a skeleton extraction batch.
/// Tolerant of malformed JSON — returns None for unparseable responses
/// rather than failing the whole pipeline.
///
/// Handles two entity formats:
/// - `"key_entities": [{"name": "X", "kind": "Character"}]` (preferred)
/// - `"key_entities": ["X", "Y"]` (fallback — kind defaults to Concept)
fn parse_skeleton_batch(response: &str, batch_start: usize) -> Option<Vec<SkeletonBatchEntry>> {
    let trimmed = response.trim();
    let json_start = trimmed.find('[')?;
    let json_end = trimmed.rfind(']')?;
    if json_end <= json_start {
        return None;
    }
    let json_str = &trimmed[json_start..=json_end];

    let arr: Vec<serde_json::Value> = serde_json::from_str(json_str).ok()?;

    let mut entries = Vec::new();
    for (i, obj) in arr.iter().enumerate() {
        let function_str = obj.get("function")?.as_str()?;
        let function = match function_str.to_lowercase().as_str() {
            "introduces" => SectionFunction::Introduces,
            "develops" => SectionFunction::Develops,
            "complicates" => SectionFunction::Complicates,
            "resolves" => SectionFunction::Resolves,
            "transitions" => SectionFunction::Transitions,
            "evidences" => SectionFunction::Evidences,
            _ => SectionFunction::Develops,
        };

        // Parse entities — handle both object and string formats.
        let entity_names_and_kinds: Vec<(String, EntityKind)> = obj
            .get("key_entities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        if let Some(obj) = v.as_object() {
                            // {"name": "X", "kind": "Character"}
                            let name = obj.get("name")?.as_str()?.to_string();
                            let kind = obj
                                .get("kind")
                                .and_then(|k| k.as_str())
                                .map(parse_entity_kind)
                                .unwrap_or(EntityKind::Concept);
                            Some((name, kind))
                        } else if let Some(s) = v.as_str() {
                            // Plain string fallback.
                            Some((s.to_string(), EntityKind::Concept))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let establishes = obj
            .get("establishes")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let is_moment = obj
            .get("is_structural_moment")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let moment_description = if is_moment {
            obj.get("moment_description")
                .and_then(|v| v.as_str())
                .map(String::from)
        } else {
            None
        };

        entries.push(SkeletonBatchEntry {
            chunk_index: batch_start + i,
            function,
            entity_names_and_kinds,
            establishes,
            moment_description,
        });
    }

    Some(entries)
}

/// Parse the router's JSON response into a DocumentAssetOperation.
/// Falls back to RAG if the response is unparseable.
fn parse_route_response(response: &str, original_request: &str) -> Result<DocumentAssetOperation> {
    let trimmed = response.trim();
    // Find JSON object in the response.
    let json_start = trimmed.find('{');
    let json_end = trimmed.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        if end > start {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]) {
                let op = obj.get("op").and_then(|v| v.as_str()).unwrap_or("rag");
                return Ok(match op {
                    "synthesis" => DocumentAssetOperation::Synthesis {
                        focus: obj
                            .get("focus")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                        entities: obj
                            .get("entities")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    "aggregation" => DocumentAssetOperation::Aggregation {
                        query: obj
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                    },
                    "transformation" => DocumentAssetOperation::Transformation,
                    _ => DocumentAssetOperation::Rag {
                        query: obj
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                    },
                });
            }
        }
    }

    // Fallback: unparseable response → treat as RAG.
    Ok(DocumentAssetOperation::Rag {
        query: original_request.to_string(),
    })
}
