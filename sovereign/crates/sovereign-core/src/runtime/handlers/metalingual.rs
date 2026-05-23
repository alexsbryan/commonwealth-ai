//! `MetalingualQuery` dispatch — source-anchored vocabulary lookup.
//!
//! Distinct from KnowledgeQuery: filters retrieval to the source the
//! locator points to ("according to SEP" → only sep; "in this
//! codebase" → only Code corpora). When the locator names a source
//! that isn't indexed locally, we surface the gap explicitly rather
//! than falling through to general knowledge — silent confabulation
//! against the wrong source is exactly what this carve-out prevents.

use crate::error::Result;
use crate::types::*;

use super::super::{
    cap_chunks_per_article, cross_corpus_sort_cmp, format_scored_chunks_with_kinds, now,
    parse_metalingual_locator, reweight_by_query_relevance, MetalingualLocator, Runtime,
    FAST_KNOWLEDGE_MAX_TOKENS, KNOWLEDGE_SYNTHESIS_SYSTEM, KQ_MERGED_LIMIT, KQ_PER_CORPUS_LIMIT,
    MAX_CHUNKS_PER_ARTICLE_AT_MERGE, MAX_KNOWLEDGE_CHARS,
};

impl Runtime {
    /// Handle MetalingualQuery: source-anchored vocabulary lookup.
    ///
    /// Distinct from KnowledgeQuery: rather than retrieving across all
    /// installed knowledge corpora, this filters to the source the
    /// locator points to ("according to SEP" → only sep; "in this
    /// codebase" → only Code corpora; "earlier in this conversation"
    /// → conversation-history corpus). Synthesis is fast slot with a
    /// source-attribution-heavy prompt — the answer says how *that
    /// source* uses the term, not a generic dictionary entry.
    ///
    /// Empty-state behaviour is intentional: when the locator points
    /// to a source that isn't indexed, we surface that explicitly and
    /// suggest the operator command to enable it. We do *not* fall
    /// through to general knowledge retrieval — that would defeat the
    /// whole point of the metalingual carve-out (silent confabulation
    /// against the wrong source).
    pub(crate) async fn handle_metalingual_query(
        &self,
        message: &str,
        _conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        let locator = parse_metalingual_locator(message);
        tracing::info!(?locator, "MetalingualQuery: parsed locator");

        // Resolve locator → (kind_filter, name_match).
        let (kind_filter, name_match): (Option<corpus_engine::CorpusKind>, Option<String>) =
            match &locator {
                MetalingualLocator::SystemCode => {
                    (Some(corpus_engine::CorpusKind::Code), None)
                }
                MetalingualLocator::Conversation => {
                    // sovereign's conversation-history corpus is a
                    // Knowledge-kind corpus with a known id substring.
                    (None, Some("conversation".to_string()))
                }
                MetalingualLocator::NamedSource(name) => {
                    (None, Some(name.clone()))
                }
                MetalingualLocator::Ambient | MetalingualLocator::Unknown => {
                    // Best-effort: prefer Code if any code corpus is
                    // installed (most common ambient locator in a dev
                    // chat); if none, the search returns empty and the
                    // empty-state message handles it.
                    (Some(corpus_engine::CorpusKind::Code), None)
                }
            };

        let locator_phrase = match &locator {
            MetalingualLocator::SystemCode => "this codebase".to_string(),
            MetalingualLocator::Conversation => "this conversation".to_string(),
            MetalingualLocator::NamedSource(n) => n.clone(),
            MetalingualLocator::Ambient | MetalingualLocator::Unknown => {
                "this system".to_string()
            }
        };

        let embedding = self.inference.embed_query(message).await.unwrap_or_default();
        let mut chunks = self
            .search_corpora_filtered(
                &embedding,
                message,
                KQ_PER_CORPUS_LIMIT,
                kind_filter,
                name_match.as_deref(),
                "MetalingualQuery",
            )
            .await;

        // Reweight + sort + cap mirror KnowledgeQuery's conditioning so
        // chunk quality is on the same scale.
        reweight_by_query_relevance(&mut chunks, message);
        chunks.sort_by(cross_corpus_sort_cmp);
        let mut chunks = cap_chunks_per_article(chunks, MAX_CHUNKS_PER_ARTICLE_AT_MERGE);
        chunks.truncate(KQ_MERGED_LIMIT);

        if chunks.is_empty() {
            // No indexed source matches the locator. Surface the gap
            // honestly — the alternative (parametric fallback) is
            // exactly the failure mode that motivated this carve-out.
            let empty_message = match &locator {
                MetalingualLocator::SystemCode => format!(
                    "I read this as a question about *this codebase*, but I don't \
                     have a code corpus indexed locally. Run `sovereign code \
                     index <path>` against the relevant repo to enable in-system \
                     vocabulary lookups, then ask again.\n\n\
                     If you meant something else by \"in this codebase\", let me \
                     know — I can re-route to general knowledge retrieval."
                ),
                MetalingualLocator::Conversation => format!(
                    "I read this as a question about something we discussed \
                     earlier in this conversation, but I couldn't find that \
                     reference. Could you quote or paraphrase the part you're \
                     asking about?"
                ),
                MetalingualLocator::NamedSource(n) => format!(
                    "I read this as a question about how `{n}` uses the term, \
                     but I don't have a corpus matching `{n}` indexed locally. \
                     Run `sovereign corpus install <id>` (or the relevant \
                     ingest recipe) and ask again. Available corpora: \
                     {corpora}.",
                    corpora = context.installed_corpora_display()
                ),
                MetalingualLocator::Ambient | MetalingualLocator::Unknown => format!(
                    "I read this as a question about how *this system* uses \
                     the term, but I couldn't find a matching internal source. \
                     Could you tell me which source you meant — the codebase, \
                     a specific corpus, our notes?"
                ),
            };
            let response_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: _conversation_id.to_string(),
                role: Role::Assistant,
                content: empty_message,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "intent": "MetalingualQuery",
                    "locator": format!("{:?}", locator),
                    "result_quality": "no_source",
                })),
                version: 0,
            };
            return Ok(Response {
                message: response_msg,
                task: None,
                metrics: None,
            });
        }

        // Build the synthesis prompt — emphasise that the answer
        // describes how the located source uses the term, and that
        // citations should attribute claims to the source.
        let (kinds, display_categories): (
            std::collections::HashMap<String, corpus_engine::CorpusKind>,
            std::collections::HashMap<String, String>,
        ) = if let Some(engine) = &self.corpus_engine {
            let mut kinds_map = std::collections::HashMap::new();
            let mut display_map = std::collections::HashMap::new();
            for info in engine.installed_indexes().await.unwrap_or_default() {
                if let Some(d) = &info.display {
                    if let Some(cat) = &d.category {
                        display_map.insert(info.corpus_id.clone(), cat.clone());
                    }
                }
                kinds_map.insert(info.corpus_id, info.kind);
            }
            (kinds_map, display_map)
        } else {
            Default::default()
        };
        let folder_meta = self.folder_metadata_snapshot().await;
        self.rerank_conv_chunks_via_ppr(message, &mut chunks, &display_categories)
            .await;
        let conv_briefing = self
            .build_conv_briefing_block(&chunks, &display_categories)
            .await;
        let doc_context = format_scored_chunks_with_kinds(
            &chunks,
            MAX_KNOWLEDGE_CHARS,
            Some(&kinds),
            None,
            if folder_meta.is_empty() {
                None
            } else {
                Some(&folder_meta)
            },
            if display_categories.is_empty() {
                None
            } else {
                Some(&display_categories)
            },
        );
        let knowledge_block = if conv_briefing.is_empty() {
            doc_context
        } else {
            format!("{conv_briefing}\n{doc_context}")
        };
        let prompt = format!(
            "RETRIEVED FROM {locator_phrase}:\n\n{knowledge_block}\n\n\
             ════════════════════════════════════\n\n\
             Question: {message}\n\n\
             Answer how *{locator_phrase}* uses the term(s) in this question. \
             Quote and cite source titles. If the retrieved passages don't \
             cover the term, say so explicitly — do not substitute generic \
             knowledge. Source attribution is the whole point of this answer."
        );
        let system = self.build_system_message(KNOWLEDGE_SYNTHESIS_SYSTEM, context);
        let request = CompletionRequest {
            prompt,
            system_message: Some(system),
            preferred_speed: Speed::Fast,
            max_tokens: Some(FAST_KNOWLEDGE_MAX_TOKENS as usize),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(0),
            structured_output: None,
            top_k: self.inference_config.top_k,
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
        };

        let completion = self.inference.complete(&request).await?;
        let sources: Vec<String> = chunks
            .iter()
            .filter_map(|c| c.title.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let response_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: _conversation_id.to_string(),
            role: Role::Assistant,
            content: completion.text,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "intent": "MetalingualQuery",
                "locator": format!("{:?}", locator),
                "sources": sources,
                "chunks_used": chunks.len(),
            })),
            version: 0,
        };
        Ok(Response {
            message: response_msg,
            task: None,
            metrics: None,
        })
    }
}
