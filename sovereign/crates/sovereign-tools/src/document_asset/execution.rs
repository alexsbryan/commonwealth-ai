// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 3 EXECUTE — run the routed operation and return the response with
//! its source citations. One executor per operation type.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

impl DocumentAssetManager {
    // ─── Execution ───────────────────────────────────────────

    /// Execute a routed operation against the document.
    ///
    /// Public so the `ask_document` Tauri command can orchestrate
    /// `route` + `execute_operation` and branch to the runtime conversation
    /// pipeline when the routing decision is `OffTopic` or RAG retrieval
    /// comes up empty.
    pub async fn execute_operation(
        &self,
        asset: &DocumentAsset,
        request: &str,
        operation: &DocumentAssetOperation,
        on_progress: &(dyn Fn(OperationProgress) + Send + Sync),
    ) -> Result<ExecutionOutput> {
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
            DocumentAssetOperation::OffTopic { .. } => {
                // The manager never executes OffTopic itself — the Tauri
                // handler is expected to detect it via the public `route()`
                // method and route the question through the normal
                // conversation pipeline (which gets corpus search, layered
                // confidence synthesis, etc.).
                //
                // Reaching this arm means a caller bypassed the pre-check
                // and called `ask()` with an OffTopic operation; return a
                // sentinel so the behavior is at least well-defined.
                Err(Error::Execution(
                    "OffTopic must be handled by the caller via runtime.handle_turn".into(),
                ))
            }
        }
    }

    /// RAG: retrieve relevant passages and synthesise an answer.
    ///
    /// When retrieval returns zero document-matching chunks this method
    /// returns an empty response + empty sources as a signal that the
    /// question wasn't really about the document. The caller (the
    /// `ask_document` Tauri command) detects the empty sources and falls
    /// through to the normal conversation pipeline.
    pub(super) async fn execute_rag(
        &self,
        source_id: &str,
        query: &str,
        original_request: &str,
    ) -> Result<ExecutionOutput> {
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
        let relevant: Vec<&DocumentChunk> =
            results.iter().filter(|c| c.source == source_id).collect();

        tracing::debug!(
            total_results = results.len(),
            relevant_count = relevant.len(),
            "execute_rag — retrieval done"
        );

        if relevant.is_empty() {
            // Empty sources signal to the Tauri handler that this turn should
            // fall through to the normal conversation pipeline (corpus search,
            // layered confidence synthesis). The router ideally classifies
            // such questions as OffTopic up front; this is the safety net.
            tracing::warn!(
                source_id = %source_id,
                "execute_rag — no relevant passages; caller should fall back to runtime"
            );
            return Ok(ExecutionOutput::empty());
        }

        // Build labeled passages. Each citation label ("passage 1") is what
        // the model will emit as [Source: passage 1] in its answer, and also
        // what the frontend matches against `retrieved_chunks[].title` when
        // rendering popovers.
        let citations: Vec<CitedChunk> = relevant
            .iter()
            .enumerate()
            .map(|(i, c)| CitedChunk {
                label: format!("passage {}", i + 1),
                chunk_index: c.chunk_index,
                snippet: short_snippet(&c.content, 200),
                content: c.content.clone(),
            })
            .collect();

        let passages: String = citations
            .iter()
            .map(|c| format!("[Source: {}] {}", c.label, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Answer the user's question based on these passages from the document.\n\n\
             Passages:\n{passages}\n\n\
             Question: {original_request}\n\n\
             Cite using [Source: passage N] notation — matching the labels above — \
             when referencing specific content. If the passages don't contain \
             enough information, say so honestly."
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            })
            .await?;

        Ok(ExecutionOutput {
            text: response.text,
            citations,
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }

    /// Synthesis: trace an entity or theme across the full document
    /// using the skeleton's entity index.
    pub(super) async fn execute_synthesis(
        &self,
        asset: &DocumentAsset,
        focus: &str,
        entities: &[String],
        original_request: &str,
    ) -> Result<ExecutionOutput> {
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
            return Ok(ExecutionOutput {
                text: "Document has no indexed content.".to_string(),
                citations: Vec::new(),
                model_id: String::new(),
                tokens_used: 0,
                finish_reason: None,
                completion_tokens: None,
                latency_ms: 0,
            });
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
                // Fallback: sample evenly across the document. The
                // `.max(1)` must wrap the DIVISION — `len.max(1) / 20`
                // is 0 for any document under 20 chunks, and
                // `step_by(0)` panics (caught 2026-06-09 by the
                // real-mode e2e: ask_document on a 2-chunk note
                // killed the worker mid-request).
                (0..all_chunks.len())
                    .step_by((all_chunks.len() / 20).max(1))
                    .collect()
            } else {
                indices
            }
        } else {
            // No skeleton — degrade to sampling.
            (0..all_chunks.len())
                .step_by((all_chunks.len() / 20).max(1))
                .collect()
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

        // Build citation metadata alongside the prompt. Each chunk gets a
        // label `§<chunk_index>` that serves as both the prompt marker
        // AND the `title` the frontend matches against when rendering
        // [Source: §N] popovers.
        let citations: Vec<CitedChunk> = selected
            .iter()
            .map(|c| {
                let truncated = short_snippet(&c.content, 500);
                CitedChunk {
                    label: format!("§{}", c.chunk_index),
                    chunk_index: c.chunk_index,
                    snippet: short_snippet(&c.content, 200),
                    content: truncated, // prompt-sized copy
                }
            })
            .collect();

        let passages: String = citations
            .iter()
            .map(|c| format!("[Source: {}] {}", c.label, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        // Keep the full content around for the assistant-message's
        // `sources` field (legacy UI), separate from the prompt-trimmed
        // text inside `citations`.
        let full_sources: Vec<String> = selected.iter().map(|c| c.content.clone()).collect();

        let skeleton_context = asset
            .skeleton
            .as_ref()
            .map(|s| {
                let moments: String = s
                    .structural_moments
                    .iter()
                    .take(10)
                    .map(|m| format!("- §{}: {}", m.chunk_index, m.description))
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
             Cite sections using [Source: §N] notation — use the exact \
             labels shown above (e.g. [Source: §4], [Source: §16]) when \
             referencing specific content."
        );

        // SLOT_POLICY §3 Synthesize: full-document synthesis composed for
        // the user (traces a focus across the text).
        let mut req = Workload::Synthesize
            .request(prompt)
            .with_output_budget(2048);
        req.temperature = Some(0.5);
        // POLICY-DEBT(SLOT_POLICY §3 Synthesize): Some(0) preserved for P1
        // neutrality (bundle is None); P5 confirms.
        req.think_budget = Some(0);
        let response = self.inference.complete(&req).await?;

        // Swap the prompt-trimmed `content` on each citation back out for
        // the full chunk content so the Tauri handler persists the real
        // source text alongside the snippet.
        let citations: Vec<CitedChunk> = citations
            .into_iter()
            .zip(full_sources)
            .map(|(mut c, full)| {
                c.content = full;
                c
            })
            .collect();

        Ok(ExecutionOutput {
            text: response.text,
            citations,
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }

    /// Aggregation: search every section for all instances matching
    /// the query.
    pub(super) async fn execute_aggregation(
        &self,
        source_id: &str,
        query: &str,
        original_request: &str,
    ) -> Result<ExecutionOutput> {
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

        if matching.is_empty() {
            tracing::warn!(query = %query, "execute_aggregation — no matches found");
            return Ok(ExecutionOutput {
                text: format!("No instances of \"{query}\" found in the document."),
                citations: Vec::new(),
                model_id: String::new(),
                tokens_used: 0,
                finish_reason: None,
                completion_tokens: None,
                latency_ms: 0,
            });
        }

        // Build citations for the first 50 matches. Each gets a label
        // `match N` that the model will cite as [Source: match N].
        let citations: Vec<CitedChunk> = matching
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, c)| CitedChunk {
                label: format!("match {}", i + 1),
                chunk_index: c.chunk_index,
                snippet: short_snippet(&c.content, 200),
                content: c.content.clone(),
            })
            .collect();

        let matches_text: String = citations
            .iter()
            .map(|c| {
                let excerpt = short_snippet(&c.content, 300);
                format!(
                    "[Source: {}] §{}: ...{}...",
                    c.label, c.chunk_index, excerpt
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "The user asked: {original_request}\n\n\
             Found {} instances across the document:\n\n{matches_text}\n\n\
             Summarise the findings. Group by theme or chronology if appropriate. \
             Cite using [Source: match N] notation — matching the labels above.",
            matching.len(),
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            })
            .await?;

        Ok(ExecutionOutput {
            text: response.text,
            citations,
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }

    /// Transformation: apply a user-requested transformation.
    pub(super) async fn execute_transformation(
        &self,
        source_id: &str,
        request: &str,
    ) -> Result<ExecutionOutput> {
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
                preferred_speed: Speed::Slow,
                max_tokens: Some(2048),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            })
            .await?;

        // Transformations consume the whole document; we don't surface
        // per-chunk citations because the output is the transformed text,
        // not a referenced answer.
        Ok(ExecutionOutput {
            text: response.text,
            citations: Vec::new(),
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }
}
