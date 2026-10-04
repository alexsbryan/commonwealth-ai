// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 2 ROUTE — classify a question into one of four operation types
//! (RAG, Synthesis, Aggregation, Transformation) from the skeleton overview.
//!
//! A second `impl DocumentAssetManager` block: inherent impls may span modules
//! within a crate, so each phase the module doc declares gets its own file
//! without the type moving.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

impl DocumentAssetManager {
    // ─── Routing ─────────────────────────────────────────────

    /// Classify a question into an operation type using the document's
    /// skeleton overview and the question text. Uses the fast model
    /// for low latency.
    ///
    /// Public so callers (the `ask_document` Tauri command) can inspect the
    /// routing decision before executing. In particular, when the router
    /// returns `OffTopic`, the caller can route the question through the
    /// normal conversation pipeline instead of the document operation path.
    pub async fn route(
        &self,
        asset: &DocumentAsset,
        request: &str,
    ) -> Result<DocumentAssetOperation> {
        tracing::debug!(asset_id = %asset.id, "document_asset::route — begin");

        // Deterministic pre-check: if the question explicitly references the
        // attached document ("this document", "summarize this paper", etc.)
        // we don't need an LLM to tell us the user wants a document answer.
        // Skip the Fast-slot call and go straight to Synthesis — which works
        // whether or not the skeleton has been built (execute_synthesis
        // samples chunks evenly when the skeleton is absent).
        //
        // Without this check, a skeleton-less asset would have a placeholder
        // overview ("Document structure not yet available.") and the Fast
        // classifier would often default to off_topic even for clearly
        // document-directed questions like "summarize this document".
        if detect_self_reference(request) {
            tracing::info!(
                asset_id = %asset.id,
                "document_asset::route — self-reference detected, defaulting to Synthesis"
            );
            return Ok(DocumentAssetOperation::Synthesis {
                focus: request.to_string(),
                entities: Vec::new(),
            });
        }

        // Filename / title grounding: if the question mentions a content
        // word from the document's filename or title (author name, key
        // concept, etc.), the user is almost certainly asking about this
        // document. Route to Synthesis without a Fast-slot classification
        // call — more reliable than depending on the model's judgment when
        // the skeleton isn't built yet.
        let tokens = filename_tokens(asset);
        if mentions_document(&tokens, request) {
            tracing::info!(
                asset_id = %asset.id,
                tokens = ?tokens.iter().take(5).collect::<Vec<_>>(),
                "document_asset::route — filename grounding matched, defaulting to Synthesis"
            );
            return Ok(DocumentAssetOperation::Synthesis {
                focus: request.to_string(),
                entities: Vec::new(),
            });
        }

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
             - {{\"op\": \"transformation\"}}\n\
             - {{\"op\": \"off_topic\", \"reason\": \"<brief why>\"}}\n\n\
             Guidelines:\n\
             - Use \"rag\" for questions about specific passages, chapters, or facts \
               in THIS document.\n\
             - Use \"synthesis\" for questions that require tracing something across \
               the full document (character arcs, argument development, thematic evolution).\n\
             - Use \"aggregation\" for \"find every mention of X\" or \"list all instances of Y\".\n\
             - Use \"transformation\" for rewriting, editing, or extracting structured data.\n\
             - Use \"off_topic\" when the question is clearly about a different \
               domain AND makes no reference to the attached document — for \
               example, the document is about physics and the user asks about \
               Buddhism without mentioning the document. A question that says \
               \"this document\", \"this text\", \"the paper\", \"summarize this\", \
               or similar self-referential phrasing is NEVER off_topic.\n\
             - When you're unsure whether the topic is in the document, prefer \
               \"synthesis\" over \"off_topic\". Synthesis still works when the \
               document hasn't been fully analysed yet, so it's the safer default.\n\n\
             Respond with only the JSON object, no other text.",
            entities = entity_names.join(", "),
            doc_type = asset.document_type.label(),
        );

        // SLOT_POLICY §3 Route: operation classification consumed by
        // parse_route_response (control flow), never shown to the user.
        // Route's Some(0) think budget matches this site verbatim.
        let mut req = Workload::Route.request(prompt).with_output_budget(128);
        req.temperature = Some(0.0);
        let response = self.inference.complete(&req).await?;

        parse_route_response(&response.text, request)
    }
}
