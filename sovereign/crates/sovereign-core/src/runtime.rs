use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::{Stream, StreamExt};

use crate::context::{build_context, format_history_as_prompt};
use crate::error::{Error, Result};
use crate::executor::{Executor, TaskContext};
use crate::memory;
use crate::oicp::LatencyPreference;
use crate::registry::ToolRegistry;
use crate::skills::SkillRegistry;
use crate::traits::{ApprovalChannel, InferenceProvider, Planner, Router, StateStore};
use crate::types::*;

/// Maximum characters of knowledge context to inject into prompts.
/// ~1000 tokens at ~4 chars/token, leaving room for history + system + response.
const MAX_KNOWLEDGE_CHARS: usize = 4000;

/// Truncate per-chunk content to produce a budget for the total knowledge context.
const MAX_CHUNK_CHARS: usize = 600;

/// Prepended to all Primary-slot (Speed::Slow) completions.
/// Sets the epistemic contract for fact-based and synthesis responses.
const PRIMARY_BASE_SYSTEM_PROMPT: &str = "You are a precise local assistant with access to \
installed knowledge bases. Accuracy is your highest priority.\n\n\
On factual questions:\n\
- If you are not certain of a specific name, number, date, or list item, say so explicitly. \
\"I am not certain of the complete roster\" is a correct and useful answer. \
A confident but incomplete list is not.\n\
- Never complete a list you do not fully know. A partial list labelled as partial is more \
useful than a fabricated full list.\n\
- If a knowledge base search has been provided, prefer it over memory. \
If the search contradicts your training data, trust the search.\n\n\
On uncertainty:\n\
- \"I don't know\" is an acceptable answer. \"I'm not certain, but...\" followed by \
clearly-labelled general knowledge is acceptable.\n\
- Fabricating specific facts (names, statistics, dates, roster members) to fill a gap \
is never acceptable, even if it would make the response sound more complete.";

/// System prompt for KnowledgeQuery synthesis — anchors `<think>` and response to retrieved passages.
const KNOWLEDGE_SYNTHESIS_SYSTEM: &str = "\
You have been given retrieved passages from an installed knowledge base. \
Your answer must be grounded in these passages.\n\
\n\
In your <think> block: reason over the RETRIEVED PASSAGES provided in the prompt, \
not from training memory. Read each passage carefully before forming any conclusion.\n\
\n\
After reasoning:\n\
- If the passages answer the question: synthesise and cite [Source: title].\n\
- If the passages are partial (e.g. name some but not all items in a list): state \
  exactly what was found and what is missing. Do not complete the list from memory.\n\
- If the passages do not contain the answer: say so clearly. Do not fill gaps from memory.\n\
\n\
NEVER present information from your training weights as if it came from the retrieved passages.\n\
NEVER invent or complete a list, roster, or statistic.";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Build a truncated knowledge context string from corpus-engine scored chunks,
/// staying within a character budget.
fn format_scored_chunks(chunks: &[corpus_engine::ScoredChunk], max_chars: usize) -> String {
    let mut parts = Vec::new();
    let mut total = 0;

    for c in chunks {
        let content = if c.content.len() > MAX_CHUNK_CHARS {
            let truncated = &c.content[..MAX_CHUNK_CHARS];
            match truncated.rfind(' ') {
                Some(pos) => format!("{}...", &truncated[..pos]),
                None => format!("{truncated}..."),
            }
        } else {
            c.content.clone()
        };

        let title = c.title.as_deref().unwrap_or(c.corpus_id.as_str());
        let part = format!("[Source: {title}]\n{content}");
        let part_len = part.len() + 5; // account for separator

        if total + part_len > max_chars {
            break;
        }

        total += part_len;
        parts.push(part);
    }

    parts.join("\n\n---\n\n")
}

/// Pre-computed knowledge context shared between streaming and non-streaming
/// response paths. Produced by [`Runtime::prepare_knowledge_context`] so the
/// two paths cannot diverge in how they search, build prompts, or report
/// provenance.
struct KnowledgeContext {
    #[allow(dead_code)]
    chunks: Vec<corpus_engine::ScoredChunk>,
    prompt: String,
    system: String,
    speed: Speed,
    search_method: Option<String>,
    sources: Vec<SourceSummary>,
    /// Summaries of retrieved chunks for frontend source linking.
    retrieved_chunks: Vec<serde_json::Value>,
}

/// Streaming handle returned by [`Runtime::handle_message_stream`].
///
/// Holds the assistant message id (assigned up-front so callers can correlate
/// chunks) and a stream of text chunks. The runtime persists the full message
/// to the store after the stream is exhausted.
pub struct StreamHandle {
    pub message_id: String,
    pub stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>>,
}

pub struct Runtime {
    pub inference: Arc<dyn InferenceProvider>,
    pub router: Box<dyn Router>,
    pub planner: Box<dyn Planner>,
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub skills: Arc<SkillRegistry>,
    pub approval: Arc<dyn ApprovalChannel>,
    pub inference_config: InferenceConfig,
    pub corpus_engine: Option<Arc<corpus_engine::CorpusEngine>>,
}

impl Runtime {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        router: Box<dyn Router>,
        planner: Box<dyn Planner>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
        skills: Arc<SkillRegistry>,
        approval: Arc<dyn ApprovalChannel>,
        inference_config: InferenceConfig,
    ) -> Self {
        Self {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
            approval,
            inference_config,
            corpus_engine: None,
        }
    }

    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
    }

    /// Search all installed corpus-engine LanceDB indexes.
    ///
    /// Returns scored chunks from every installed corpus. If the IVF-PQ
    /// vector index is not built for a corpus, passes an empty embedding
    /// to trigger FTS-only mode (fast Tantivy, avoids the 20–60 second
    /// O(n) full-scan fallback).
    ///
    /// Used by both `handle_knowledge_query` and `handle_simple` so that
    /// installed corpora enrich all intent types, not just KnowledgeQuery.
    async fn search_corpus_indexes(
        &self,
        embedding: &[f32],
        query_text: &str,
        limit: usize,
        label: &str,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let mut chunks = Vec::new();
        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => {
                tracing::warn!("{label}: corpus_engine is None — no corpus search possible");
                return chunks;
            }
        };
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(error = %e, "{label}: installed_indexes() failed");
                return chunks;
            }
        };
        if indexes.is_empty() {
            tracing::warn!("{label}: installed_indexes() returned 0 indexes — nothing to search");
        } else {
            tracing::info!(count = indexes.len(), "{label}: found corpus indexes");
        }
        for info in &indexes {
            tracing::info!(
                corpus = %info.corpus_id,
                path = %info.path.display(),
                chunks = info.chunk_count,
                dims = info.embedding_dimensions,
                embedding_model = %info.embedding_model,
                "{label}: opening index"
            );
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: open_index failed");
                    continue;
                }
            };
            match idx.search(embedding, query_text, limit).await {
                Ok(scored) => {
                    tracing::info!(
                        corpus = %info.corpus_id,
                        results = scored.len(),
                        "{label}: search complete"
                    );
                    chunks.extend(scored);
                }
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: search failed");
                }
            }
        }
        chunks
    }

    /// Search all knowledge sources, build the prompt with retrieved context,
    /// and assemble provenance metadata. Shared between the streaming and
    /// non-streaming response paths so they cannot diverge.
    async fn prepare_knowledge_context(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
    ) -> KnowledgeContext {
        // Check if a document is attached. When present, retrieve chunks
        // from that specific document rather than doing a general search.
        // The prefix format is: [Document attached: filename]\n\n<actual question>
        let (attached_source, query_text) = if let Some(rest) = message.strip_prefix("[Document attached: ") {
            if let Some(end) = rest.find(']') {
                let source = rest[..end].to_string();
                let query = rest[end + 1..].trim().to_string();
                (Some(source), if query.is_empty() { message.to_string() } else { query })
            } else {
                (None, message.to_string())
            }
        } else {
            (None, message.to_string())
        };

        let mut all_chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();

        if attached_source.is_some() {
            // Document-attached messages are routed to ComplexTask and should
            // never reach this path — the planner invokes DocumentOperationTool
            // for full map-reduce across all chunks. If we somehow get here,
            // return empty context rather than stuffing a few search results
            // into the prompt.
            tracing::debug!("prepare_knowledge_context called with attached document — skipping (should be ComplexTask)");
        } else {
            // Normal mode: search installed corpora (corpus-engine LanceDB)
            // and corpus-type documents in StateStore. User-uploaded documents
            // are NOT included — they are only surfaced when explicitly
            // attached via [Document attached: ...].
            let corpus_embedding = self.inference.embed_query(message).await.unwrap_or_default();
            let label = format!("{intent:?}");
            all_chunks = self
                .search_corpus_indexes(&corpus_embedding, message, 5, &label)
                .await;

            // Also search StateStore for corpus-type documents (used by test
            // harness and for corpora ingested directly into the store).
            let embedding = self.inference.embed(message).await.unwrap_or_default();
            let store_chunks = self
                .store
                .search_documents(&embedding, message, 5)
                .await
                .unwrap_or_default();
            for doc in &store_chunks {
                // Only include corpus-type documents, not user uploads.
                if matches!(doc.source_type, SourceType::Corpus { .. }) {
                    all_chunks.push(corpus_engine::ScoredChunk {
                        content: doc.content.clone(),
                        title: Some(doc.source.clone()),
                        url: None,
                        corpus_id: match &doc.source_type {
                            SourceType::Corpus { corpus_id } => corpus_id.clone(),
                            _ => "unknown".to_string(),
                        },
                        score: 0.5,
                        metadata: HashMap::new(),
                    });
                }
            }
        }

        all_chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_chunks.truncate(8);

        // 4. Provenance metadata.
        let installed_corpora = self
            .store
            .list_corpus_states()
            .await
            .unwrap_or_default();
        let corpora_searched = !installed_corpora.is_empty() || self.corpus_engine.is_some();

        let search_method = if !all_chunks.is_empty() {
            Some("LocalOnly".to_string())
        } else if corpora_searched {
            Some("LocalOnly (no matches)".to_string())
        } else {
            None
        };

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &all_chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }
        if all_chunks.is_empty() && corpora_searched {
            for cs in &installed_corpora {
                source_map.entry(cs.corpus_id.clone()).or_insert(0);
            }
        }
        let sources: Vec<SourceSummary> = source_map
            .into_iter()
            .map(|(origin, count)| SourceSummary { origin, count })
            .collect();

        // 5. Build prompt with knowledge context.
        let history = format_history_as_prompt(context, 10);
        let prompt = if !all_chunks.is_empty() {
            let doc_context = format_scored_chunks(&all_chunks, MAX_KNOWLEDGE_CHARS);
            if history.is_empty() {
                format!(
                    "Relevant knowledge:\n{doc_context}\n\nUser: {message}\n\nAssistant:"
                )
            } else {
                let short_history = format_history_as_prompt(context, 4);
                format!(
                    "{short_history}\n\nRelevant knowledge:\n{doc_context}\n\nAssistant:"
                )
            }
        } else if history.is_empty() {
            message.to_string()
        } else {
            format!("{history}\n\nAssistant:")
        };

        // 6. System message — epistemic contract when knowledge is present.
        let system = if !all_chunks.is_empty() {
            self.build_primary_system_message(
                "Answer based on the provided knowledge sources when relevant. \
                 Cite sources when referencing them using [Source: name] notation. \
                 If you make a claim NOT directly supported by the provided sources, \
                 mark it with [unverified].",
                context,
            )
        } else {
            self.build_system_message(
                "You are a helpful AI assistant. Respond concisely and accurately.",
                context,
            )
        };

        // 7. Speed upgrade: if knowledge found for SimpleQuery, use Slow.
        let speed = match intent {
            Intent::SimpleQuery => {
                if !all_chunks.is_empty() {
                    Speed::Slow
                } else {
                    Speed::Fast
                }
            }
            Intent::DeepQuery => Speed::Slow,
            _ => Speed::Medium,
        };

        // 8. Build chunk summaries for frontend source linking.
        let retrieved_chunks: Vec<serde_json::Value> = all_chunks
            .iter()
            .map(|c| {
                let snippet = if c.content.len() > 200 {
                    let truncated = &c.content[..200];
                    match truncated.rfind(' ') {
                        Some(pos) => format!("{}...", &truncated[..pos]),
                        None => format!("{truncated}..."),
                    }
                } else {
                    c.content.clone()
                };
                serde_json::json!({
                    "title": c.title.as_deref().unwrap_or(""),
                    "corpus_id": c.corpus_id,
                    "url": c.url,
                    "snippet": snippet,
                })
            })
            .collect();

        KnowledgeContext {
            chunks: all_chunks,
            prompt,
            system,
            speed,
            search_method,
            sources,
            retrieved_chunks,
        }
    }

    /// Build OICP requirements from active skills for non-Fast requests.
    /// Returns None if no skills have OICP capability configuration.
    fn build_oicp(
        &self,
        latency: LatencyPreference,
    ) -> Option<crate::oicp::InferenceRequirements> {
        let req = self.skills.inference_requirements();
        // Skip if there are no capability requirements to express.
        if req.required().is_empty() && req.preferred().is_empty() {
            return None;
        }
        Some(req.with_latency(latency))
    }

    /// Build a system message that includes memory context.
    fn build_system_message(&self, base: &str, context: &ConversationContext) -> String {
        let mut parts = vec![base.to_string()];

        if let Some(mem_section) = memory::format_memories_for_prompt(&context.memories) {
            parts.push(mem_section);
        }

        if let Some(wm) = &context.working_memory {
            if let Some(goal) = &wm.current_goal {
                parts.push(format!("Current user goal: {goal}"));
            }
            if !wm.facts.is_empty() {
                parts.push(format!(
                    "Session context:\n- {}",
                    wm.facts.join("\n- ")
                ));
            }
        }

        parts.join("\n\n")
    }

    /// Build a system message for Primary-slot (Speed::Slow) completions.
    /// Prepends `PRIMARY_BASE_SYSTEM_PROMPT` before the caller-supplied base text
    /// so all Primary calls carry the epistemic accuracy contract.
    fn build_primary_system_message(&self, base: &str, context: &ConversationContext) -> String {
        self.build_system_message(
            &format!("{PRIMARY_BASE_SYSTEM_PROMPT}\n\n{base}"),
            context,
        )
    }

    /// Extract long-term memories from a conversation and save them.
    /// Call this when a conversation ends (user quits or session ends).
    pub async fn end_conversation(&self, conversation_id: &str) -> Result<()> {
        let context = build_context(self.store.as_ref(), conversation_id, "").await?;
        if context.conversation.messages.len() < 4 {
            return Ok(());
        }

        let memory_rules = self.skills.memory_rules();
        let extracted = memory::extract_long_term_memories(
            self.inference.as_ref(),
            &context.conversation.messages,
            &memory_rules,
        )
        .await?;

        eprintln!("[memory] Extracted {} memories", extracted.len());
        for mem in extracted {
            memory::save_with_contradiction_check(
                self.inference.as_ref(),
                self.store.as_ref(),
                mem,
            )
            .await?;
        }

        let pruned = memory::prune_decayed_memories(self.store.as_ref(), now())
            .await
            .unwrap_or(0);
        if pruned > 0 {
            eprintln!("[memory] Pruned {pruned} decayed memories");
        }

        Ok(())
    }

    /// Stream a chat response token-by-token.
    ///
    /// Builds context, saves the user message, routes the intent, and starts
    /// streaming inference for SimpleQuery / DeepQuery / KnowledgeQuery. The
    /// returned [`StreamHandle`] yields response chunks; once the stream
    /// completes, the assistant message is persisted under `message_id`.
    ///
    /// Returns [`Error::NotImplemented`] for ComplexTask intents — callers
    /// should fall back to [`Self::handle_message`] in that case.
    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        // 1. Build context.
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;

        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 2. Save user message.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;
        context.conversation.messages.push(user_msg);

        // 3. Route.
        let tool_descriptors = self.tools.descriptors();
        let RoutingOutcome { intent, coarse_intent, self_assessment } = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;

        // Document attached or ComplexTask → fall back to non-streaming.
        if message.starts_with("[Document attached: ")
            || matches!(intent, Intent::ComplexTask | Intent::KnowledgeQuery)
        {
            return Err(Error::NotImplemented(
                "Streaming not supported for this intent".into(),
            ));
        }

        // 4. Search knowledge + build prompt (shared with handle_simple).
        let kc = self
            .prepare_knowledge_context(message, &context, &intent)
            .await;

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(LatencyPreference::BestEffort)
        };

        // Capture model ID before spawning — complete_stream returns no metadata.
        let model_id = self.inference.model_id_for(kc.speed);

        let request = CompletionRequest {
            prompt: kc.prompt,
            system_message: Some(kc.system),
            preferred_speed: kc.speed,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp,
        };

        let search_method = kc.search_method;
        let sources = kc.sources;
        let retrieved_chunks = kc.retrieved_chunks;

        let intent_label = format!("{intent:?}");
        let message_id = uuid::Uuid::new_v4().to_string();

        // 5. Spawn streaming task.
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut full_text = String::new();

            let stream_result = inference.complete_stream(&request).await;
            match stream_result {
                Ok(mut s) => {
                    while let Some(item) = s.next().await {
                        match item {
                            Ok(chunk) => {
                                full_text.push_str(&chunk);
                                if tx.send(Ok(chunk)).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }

            // Persist final assistant message.
            let provenance = ResponseProvenance {
                intent: intent_label,
                search_method,
                sources,
                inference_backend: model_id,
                oicp_match: None,
                total_latency_ms: started.elapsed().as_millis() as u64,
                tokens_used: 0,
                coarse_intent,
                self_assessment,
            };
            let assistant_msg = Message {
                id: message_id_owned,
                conversation_id: conversation_id_owned,
                role: Role::Assistant,
                content: full_text,
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "streamed": true,
                    "provenance": provenance,
                    "retrieved_chunks": retrieved_chunks,
                })),
                version: now(),
            };
            let _ = store.save_message(&assistant_msg).await;
        });

        let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));

        Ok(StreamHandle { message_id, stream })
    }

    pub async fn handle_message(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        // 1. Build context from store (use message text for memory retrieval).
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;

        // 1b. Compress working memory from conversation history.
        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 2. Save user message.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
            version: now(),
        };
        self.store.save_message(&user_msg).await?;
        context.conversation.messages.push(user_msg);

        // 3. Route.
        let tool_descriptors = self.tools.descriptors();
        let RoutingOutcome { mut intent, coarse_intent, self_assessment } = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;

        // When a document is attached, bypass the planner entirely and
        // call document_operation directly. The user's message is the
        // operation; we generate map/reduce prompts with a focused
        // inference call and inject the source deterministically.
        if let Some(rest) = message.strip_prefix("[Document attached: ") {
            if let Some(end) = rest.find(']') {
                let source = rest[..end].to_string();
                let user_query = rest[end + 1..].trim().to_string();
                return self
                    .handle_document_operation(
                        &source,
                        &user_query,
                        message,
                        conversation_id,
                        &context,
                    )
                    .await;
            }
        }

        // 4. Dispatch based on intent.
        match intent {
            Intent::ComplexTask => {
                self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                    .await
            }
            Intent::KnowledgeQuery => {
                self.handle_knowledge_query(
                    message, conversation_id, &context, coarse_intent, self_assessment,
                )
                .await
            }
            _ => {
                self.handle_simple(
                    message, conversation_id, &context, &intent, coarse_intent, self_assessment,
                )
                .await
            }
        }
    }

    /// Handle SimpleQuery, DeepQuery, and other non-plan intents.
    /// Searches all knowledge sources before generating a response.
    async fn handle_simple(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        intent: &Intent,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
    ) -> Result<Response> {
        // Search knowledge + build prompt (shared with handle_message_stream).
        let kc = self
            .prepare_knowledge_context(message, context, intent)
            .await;

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(LatencyPreference::BestEffort)
        };

        let request = CompletionRequest {
            prompt: kc.prompt,
            system_message: Some(kc.system),
            preferred_speed: kc.speed,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp,
        };

        let completion = self.inference.complete(&request).await?;

        let provenance = ResponseProvenance {
            intent: format!("{intent:?}"),
            search_method: kc.search_method,
            sources: kc.sources,
            inference_backend: completion.model_id.clone(),
            oicp_match: completion
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: completion.latency_ms,
            tokens_used: completion.tokens_used,
            coarse_intent,
            self_assessment,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: completion.text.clone(),
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": completion.model_id,
                "tokens": completion.tokens_used,
                "latency_ms": completion.latency_ms,
                "provenance": provenance,
                "retrieved_chunks": kc.retrieved_chunks,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    /// Handle KnowledgeQuery: search corpus-engine LanceDB indexes → inject into prompt → synthesize.
    async fn handle_knowledge_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        coarse_intent: Option<String>,
        self_assessment: Option<String>,
    ) -> Result<Response> {
        use std::cmp::Ordering;

        // 1. Embed the query using the query-side function (applies instruction prefix
        //    for asymmetric models like Qwen3-Embedding).
        let t_search = std::time::Instant::now();
        let embedding = self.inference.embed_query(message).await.unwrap_or_default();

        // 2. Search corpus-engine LanceDB indexes.
        let mut chunks = self
            .search_corpus_indexes(&embedding, message, 5, "KnowledgeQuery")
            .await;

        // 3. Sort by score, keep top 8.
        chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        chunks.truncate(8);

        // 4a. Empty results path — Fast slot, honest not-found response.
        if chunks.is_empty() {
            tracing::info!("KnowledgeQuery: no chunks — returning empty-results response");
            let corpora = context.installed_corpora_display();
            let prompt = format!(
                "The user asked: \"{message}\"\n\n\
                 You searched these installed knowledge sources: {corpora}\n\
                 The search returned no relevant results.\n\n\
                 Respond briefly and helpfully:\n\
                 - Tell the user you searched but didn't find a specific answer\n\
                 - If you have confident general knowledge about this topic, share it \
                   briefly but clearly label it as general knowledge, not from the search\n\
                 - If web search or installing an additional corpus might help, suggest it"
            );
            let request = CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Fast,
                max_tokens: Some(300),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
            };
            let completion = self.inference.complete(&request).await?;
            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: completion.text.clone(),
                created_at: now(),
                metadata: Some(serde_json::json!({
                    "model": completion.model_id,
                    "tokens": completion.tokens_used,
                    "latency_ms": completion.latency_ms,
                    "intent": "knowledge_query",
                    "documents_found": 0,
                    "result_quality": "empty",
                })),
                version: now(),
            };
            self.store.save_message(&assistant_msg).await?;
            return Ok(Response { message: assistant_msg, task: None });
        }

        // 4b. Build prompt — retrieved content FIRST, question LAST.
        // Putting retrieved passages before the question prevents the model from
        // reasoning from training weights during its <think> phase.
        let doc_context = format_scored_chunks(&chunks, MAX_KNOWLEDGE_CHARS);
        let corpus_display = context.installed_corpora_display();
        let prompt = format!(
            "RETRIEVED FROM {corpus_display}:\n\n{doc_context}\n\n\
             ════════════════════════════════════\n\n\
             Question: {message}"
        );

        let system = self.build_primary_system_message(KNOWLEDGE_SYNTHESIS_SYSTEM, context);

        let request = CompletionRequest {
            prompt,
            system_message: Some(system),
            preferred_speed: Speed::Slow,
            max_tokens: Some(self.inference_config.max_tokens),
            temperature: Some(self.inference_config.temperature),
            think_budget: Some(self.inference_config.think_budget),
            structured_output: None,
            top_k: self.inference_config.top_k,
            top_p: None,
            oicp: self.build_oicp(LatencyPreference::BestEffort),
        };

        let completion = self.inference.complete(&request).await?;

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }
        let provenance = ResponseProvenance {
            intent: "KnowledgeQuery".to_string(),
            search_method: Some("CorpusEngine".to_string()),
            sources: source_map
                .into_iter()
                .map(|(origin, count)| SourceSummary { origin, count })
                .collect(),
            inference_backend: completion.model_id.clone(),
            oicp_match: completion
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: completion.latency_ms,
            tokens_used: completion.tokens_used,
            coarse_intent,
            self_assessment,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: completion.text.clone(),
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": completion.model_id,
                "tokens": completion.tokens_used,
                "latency_ms": completion.latency_ms,
                "intent": "knowledge_query",
                "documents_found": chunks.len(),
                "search_ms": t_search.elapsed().as_millis() as u64,
                "provenance": provenance,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    /// Handle ComplexTask: plan → execute → (replan on failure) → synthesize.
    /// Handle document analysis: bypass planner, call document_operation directly.
    ///
    /// 1. Resolve the source path from the store
    /// 2. Generate map/reduce prompts with a single inference call
    /// 3. Call document_operation tool directly with deterministic params
    /// 4. Synthesize the result into a response
    async fn handle_document_operation(
        &self,
        source_hint: &str,
        user_query: &str,
        original_message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        eprintln!("[runtime] Document operation: resolving source...");

        // 1. Resolve actual source path from the store.
        let sources = self.store.list_sources().await.unwrap_or_default();
        let source_lower = source_hint.to_lowercase();
        let resolved_source = sources
            .iter()
            .find(|s| s.to_lowercase().contains(&source_lower))
            .cloned()
            .unwrap_or_else(|| source_hint.to_string());

        // Get chunk count for the prompt.
        let chunks = self.store.get_chunks_by_source(&resolved_source).await.unwrap_or_default();
        let chunk_count = chunks.len();
        let word_count: usize = chunks.iter().map(|c| c.content.split_whitespace().count()).sum();
        drop(chunks);

        if chunk_count == 0 {
            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: format!(
                    "No document chunks found for '{}'. The document may not have been ingested correctly.",
                    source_hint
                ),
                created_at: now(),
                metadata: None,
                version: now(),
            };
            self.store.save_message(&assistant_msg).await?;
            return Ok(Response { message: assistant_msg, task: None });
        }

        eprintln!(
            "[runtime] Document: {} ({} chunks, ~{} words). Generating prompts...",
            resolved_source, chunk_count, word_count
        );

        // 2. Generate map/reduce prompts with a single focused inference call.
        let prompt_request = CompletionRequest {
            prompt: format!(
                "The user uploaded a document ({chunk_count} chunks, ~{word_count} words) and asked:\n\
                 \"{user_query}\"\n\n\
                 Write two prompts for a map-reduce analysis of this document.\n\n\
                 MAP PROMPT — applied to each chunk of the document:\n\
                 - Extract only what's present in that chunk\n\
                 - Produce structured notes relevant to the user's request\n\
                 - Do NOT invent or assume content not in the chunk\n\n\
                 REDUCE PROMPT — merges all extracted notes into one result:\n\
                 - Synthesize into a coherent, comprehensive answer\n\
                 - Deduplicate and organize logically\n\n\
                 Respond in JSON only:\n\
                 {{\"map_prompt\": \"...\", \"reduce_prompt\": \"...\"}}"
            ),
            system_message: Some(
                "You write analysis prompts. Output ONLY the JSON object, nothing else.".to_string()
            ),
            preferred_speed: Speed::Fast,
            max_tokens: Some(512),
            temperature: Some(0.0), // deterministic — this is structured output
            // Grammar-constrain to produce exactly {"map_prompt":"...","reduce_prompt":"..."}
            structured_output: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "map_prompt": { "type": "string" },
                    "reduce_prompt": { "type": "string" }
                },
                "required": ["map_prompt", "reduce_prompt"]
            })),
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
        };

        let prompt_response = self.inference.complete(&prompt_request).await?;
        let prompt_text = prompt_response.text.trim();

        // Parse the generated prompts. Grammar constraint should guarantee
        // valid JSON, but fallback handles edge cases.
        let (map_prompt, reduce_prompt) = match serde_json::from_str::<serde_json::Value>(
            prompt_text
                .strip_prefix("```json")
                .and_then(|s| s.strip_suffix("```"))
                .unwrap_or(prompt_text)
                .trim()
        ) {
            Ok(v) => {
                let mp = v.get("map_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Extract key information relevant to the user's question from this passage."
                ).to_string();
                let rp = v.get("reduce_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Synthesize all extracted information into a comprehensive answer."
                ).to_string();
                (mp, rp)
            }
            Err(_) => {
                // Fallback prompts if JSON parsing fails.
                tracing::warn!("Failed to parse prompt JSON, using defaults");
                (
                    format!("Extract information relevant to this question from the passage: {user_query}"),
                    "Synthesize all extracted information into a comprehensive, well-organized answer.".to_string(),
                )
            }
        };

        eprintln!("[runtime] Prompts generated. Running document_operation...");

        // 3. Call document_operation tool directly.
        let tool = self.tools.get("document_operation")?;
        let params = serde_json::json!({
            "source": resolved_source,
            "operation": user_query,
            "map_prompt": map_prompt,
            "reduce_prompt": reduce_prompt,
            "conversation_id": conversation_id,
        });

        let tool_ctx = ToolContext {
            conversation_id: conversation_id.to_string(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
        };

        let result = tool.execute(&params, &tool_ctx).await?;
        let result_text = match &result {
            StepOutput::Text(t) => t.clone(),
            StepOutput::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            _ => String::new(),
        };

        eprintln!("[runtime] Document operation complete ({} chars output)", result_text.len());

        // 4. Build response.
        let provenance = ResponseProvenance {
            intent: "DocumentOperation".to_string(),
            search_method: Some("document_operation".to_string()),
            sources: vec![SourceSummary {
                origin: "user_document".to_string(),
                count: chunk_count,
            }],
            inference_backend: prompt_response.model_id.clone(),
            oicp_match: None,
            total_latency_ms: 0,
            tokens_used: 0,
            coarse_intent: None,
            self_assessment: None,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: result_text,
            created_at: now(),
            metadata: Some(serde_json::json!({
                "provenance": provenance,
                "document_source": resolved_source,
                "document_chunks": chunk_count,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    async fn handle_complex_task(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        // Document-attached messages are handled by handle_document_operation
        // before reaching this point. This path is for non-document ComplexTasks.

        eprintln!("[runtime] Generating plan...");
        let plan = self
            .planner
            .plan(message, context, tool_descriptors)
            .await?;

        eprintln!(
            "[runtime] Plan: {} steps",
            plan.steps.len(),
        );
        for step in &plan.steps {
            eprintln!("  [step {}] {}", step.id, step.description);
        }

        // 2. Create task.
        let mut task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            goal: message.to_string(),
            plan: plan.clone(),
            status: TaskStatus::Running,
            completed_steps: Vec::new(),
            created_at: now(),
            updated_at: now(),
            version: now(),
        };
        self.store.save_task(&task).await?;

        // 3. Execute.
        let executor = Executor::new(
            Arc::clone(&self.inference),
            Arc::clone(&self.tools),
            Arc::clone(&self.store),
            Arc::clone(&self.approval),
            Arc::clone(&self.skills),
        );

        let mut ctx = TaskContext {
            task: task.clone(),
            completed: HashMap::new(),
        };

        let mut result = executor.run(&plan, &mut ctx).await?;

        // 4. Replan on failure (one retry).
        if let Some(ref error) = result.error {
            eprintln!(
                "[runtime] Step {} failed: {}. Attempting replan...",
                error.step_id, error.message
            );

            let completed_vec: Vec<(usize, StepOutput)> =
                result.completed.iter().map(|(&k, v)| (k, v.clone())).collect();

            match self.planner.replan(&plan, &completed_vec, error).await {
                Ok(new_plan) => {
                    eprintln!("[runtime] Replan: {} steps", new_plan.steps.len());
                    task.plan = new_plan.clone();
                    task.status = TaskStatus::Running;
                    task.updated_at = now();

                    let mut retry_ctx = TaskContext {
                        task: task.clone(),
                        completed: HashMap::new(),
                    };

                    result = executor.run(&new_plan, &mut retry_ctx).await?;

                    if result.error.is_some() {
                        eprintln!("[runtime] Replan also failed.");
                    }
                }
                Err(e) => {
                    eprintln!("[runtime] Replan failed: {e}");
                }
            }
        }

        // 5. Synthesize final answer from step outputs.
        let step_summaries: Vec<String> = result
            .completed
            .iter()
            .filter_map(|(id, output)| match output {
                StepOutput::Text(t) => Some(format!("Step {id}: {t}")),
                StepOutput::Json(v) => {
                    // For search tool output, use the "answer" field.
                    let text = v
                        .get("answer")
                        .and_then(|a| a.as_str())
                        .unwrap_or_else(|| {
                            // Fallback: serialize the whole JSON.
                            ""
                        });
                    if text.is_empty() {
                        Some(format!("Step {id}: {}", serde_json::to_string_pretty(v).unwrap_or_default()))
                    } else {
                        Some(format!("Step {id}: {text}"))
                    }
                }
                StepOutput::ReasonWithToolsResult { ref text, iterations, capped, .. } => {
                    let note = if *capped { " (search cap reached)" } else { "" };
                    Some(format!("Step {id} ({iterations} searches{note}): {text}"))
                }
                _ => None,
            })
            .collect();

        let synthesis_prompt = format!(
            "Goal: {message}\n\nStep results:\n{}\n\nProvide a comprehensive final answer that synthesizes all the step results above.",
            step_summaries.join("\n\n")
        );

        let synthesis_system = self.build_primary_system_message(
            "Synthesize the given step results into a clear, comprehensive answer.",
            context,
        );

        let synthesis = self
            .inference
            .complete(&CompletionRequest {
                prompt: synthesis_prompt,
                system_message: Some(synthesis_system),
                preferred_speed: Speed::Slow,
                max_tokens: Some(self.inference_config.max_tokens),
                temperature: Some(self.inference_config.temperature),
                think_budget: Some(self.inference_config.think_budget),
                structured_output: None,
                top_k: self.inference_config.top_k,
                top_p: None,
                oicp: self.build_oicp(LatencyPreference::Throughput),
            })
            .await?;

        // 6. Update task status.
        task.completed_steps = result.completed.iter().map(|(&k, v)| (k, v.clone())).collect();
        task.status = if result.error.is_some() {
            TaskStatus::Failed
        } else {
            TaskStatus::Completed
        };
        task.updated_at = now();
        self.store.save_task(&task).await?;

        // 7. Extract search provenance from tool step outputs.
        let mut search_method: Option<String> = None;
        let mut all_sources: Vec<SourceSummary> = Vec::new();
        for (_step_idx, output) in &task.completed_steps {
            match output {
                StepOutput::Json(ref val) => {
                    if let Some(method) = val.get("search_method").and_then(|v| v.as_str()) {
                        search_method = Some(method.to_string());
                    }
                    if let Some(sources) = val.get("sources").and_then(|v| v.as_array()) {
                        for src in sources {
                            if let (Some(origin), Some(count)) = (
                                src.get("origin").and_then(|v| v.as_str()),
                                src.get("count").and_then(|v| v.as_u64()),
                            ) {
                                all_sources.push(SourceSummary {
                                    origin: origin.to_string(),
                                    count: count as usize,
                                });
                            }
                        }
                    }
                }
                StepOutput::ReasonWithToolsResult {
                    search_log,
                    iterations,
                    ..
                } => {
                    search_method = Some(format!("ReasonWithTools ({iterations} iterations)"));
                    // Aggregate search log into source summaries.
                    let mut tool_counts: HashMap<String, usize> = HashMap::new();
                    for entry in search_log {
                        *tool_counts
                            .entry(entry.tool_id.clone())
                            .or_insert(0) += entry.result_count;
                    }
                    for (tool_id, count) in tool_counts {
                        all_sources.push(SourceSummary {
                            origin: tool_id,
                            count,
                        });
                    }
                }
                _ => {}
            }
        }

        // Save and return assistant message.
        let provenance = ResponseProvenance {
            intent: "ComplexTask".to_string(),
            search_method,
            sources: all_sources,
            inference_backend: synthesis.model_id.clone(),
            oicp_match: synthesis
                .oicp_meta
                .as_ref()
                .and_then(|m| m.match_quality.as_ref())
                .map(|q| format!("{q:?}")),
            total_latency_ms: synthesis.latency_ms,
            tokens_used: synthesis.tokens_used,
            coarse_intent: None,
            self_assessment: None,
        };

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: synthesis.text.clone(),
            created_at: now(),
            metadata: Some(serde_json::json!({
                "model": synthesis.model_id,
                "tokens": synthesis.tokens_used,
                "latency_ms": synthesis.latency_ms,
                "task_id": task.id,
                "steps_completed": task.completed_steps.len(),
                "provenance": provenance,
            })),
            version: now(),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: Some(task),
        })
    }
}
