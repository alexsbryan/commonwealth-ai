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

/// System prompt for KnowledgeQuery synthesis — three-tier confidence framework.
///
/// Tier 1 (Retrieved): Claims grounded in passages, cited with [Source: title].
/// Tier 2 (Parametric): Well-established general knowledge, presented naturally.
/// Tier 3 (Inference): Reasoning beyond firm ground, hedged explicitly.
const KNOWLEDGE_SYNTHESIS_SYSTEM: &str = "\
You have been given retrieved passages from an installed knowledge base. \
Use them together with your general knowledge to answer the question.\n\
\n\
Use three tiers of knowledge, each presented differently:\n\
\n\
RETRIEVED — facts directly from the passages below.\n\
  Cite with [Source: title]. These are your strongest claims.\n\
\n\
PARAMETRIC — your general knowledge that is well-established and consistent \
  with or extends the retrieved content. Present naturally in prose. \
  No special label needed for claims that are widely accepted.\n\
\n\
INFERENCE — reasoning that goes beyond what sources or general knowledge \
  can firmly establish. Introduce with hedged language: \
  \"Drawing from this framework...\", \"This suggests...\", \
  \"The likely position would be...\"\n\
\n\
Guidelines:\n\
- Do not refuse to engage because retrieval was incomplete.\n\
- Do not use [unverified] tags.\n\
- If retrieval found nothing relevant, say so in one sentence, then answer \
  from your general knowledge.\n\
- Cite retrieved content with [Source: title]. Present confident general \
  knowledge naturally. Hedge genuine uncertainty.\n\
- NEVER invent or complete a list, roster, or statistic you do not fully know.";

/// Thinking directive — orients `<think>` toward substantive reasoning.
///
/// Without this, models default to source-adequacy bookkeeping in their
/// thinking blocks ("Source Analysis: [X] — no substantive content...").
/// This directive redirects the thinking budget toward the intellectual
/// content of the question.
const THINKING_DIRECTIVE: &str = "\
In your <think> block, reason about the SUBSTANCE of the question:\n\
1. What does this question actually ask? What would a complete answer contain?\n\
2. What do the retrieved sources contribute — which specific claims do they ground?\n\
3. What do I know well enough to state directly, even without retrieved support?\n\
4. Where are the genuine gaps — things I am uncertain about or where both \
   sources and my knowledge fall short?\n\
5. How should I frame what I know vs. what I'm inferring vs. what I'm uncertain about?\n\
\n\
Spend your thinking on the substance of the question.\n\
Source inventory (\"source X discusses Y\") belongs in a single brief scan, \
not as the primary content of your reasoning.";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Truncate a chunk's content to `MAX_CHUNK_CHARS`, breaking at a word boundary.
fn truncate_chunk_content(content: &str) -> String {
    if content.len() > MAX_CHUNK_CHARS {
        let truncated = &content[..MAX_CHUNK_CHARS];
        match truncated.rfind(' ') {
            Some(pos) => format!("{}...", &truncated[..pos]),
            None => format!("{truncated}..."),
        }
    } else {
        content.to_string()
    }
}

/// Rescale each chunk's `score` by the max score observed in its
/// own corpus. Result: every corpus's top hit lands at 1.0,
/// lower-ranked hits scale proportionally within their corpus, and
/// cross-corpus comparison becomes meaningful.
///
/// Why this is needed: FTS5 BM25 scores depend on corpus-specific
/// IDF weights and document-length statistics. On a two-corpus
/// setup where one is a 1k-row code index and the other is a
/// 188k-row SEP prose index, a query like
/// *"is free will compatible with determinism?"* can score a code
/// test function at 21 (because the test name's underscore-split
/// tokens are rare enough in the small corpus to fire high IDF)
/// while SEP's genuine `compatibilism` match scores 19. Naive
/// `sort_by(score)` then ranks the test function above the
/// philosophy passage — visible in the UI as a top-listed code
/// chunk for a pure-philosophy query.
///
/// Max-based normalisation is the least-surgical fix. Alternatives
/// considered and rejected:
///   * **Round-robin by corpus**: loses score fidelity; a corpus
///     with only mediocre hits gets equal billing to one with
///     strong hits.
///   * **Z-score**: needs mean + stddev, which are unreliable on
///     5-to-8-element distributions.
///   * **Query/corpus classifier**: the "right" answer, but needs
///     labelled training data and a judgement model.
///
/// This is a heuristic: it doesn't *know* philosophy queries should
/// prefer SEP, but it does prevent one outlier in a small corpus
/// from monopolising the top-N. In-context, that's enough — the
/// synthesis model sees evidence from every corpus that had a real
/// match, weighted by within-corpus rank.
pub(crate) fn normalise_scores_per_corpus(chunks: &mut [corpus_engine::ScoredChunk]) {
    use std::collections::HashMap;
    let mut max_per_corpus: HashMap<String, f32> = HashMap::new();
    for c in chunks.iter() {
        let entry = max_per_corpus.entry(c.corpus_id.clone()).or_insert(c.score);
        if c.score > *entry {
            *entry = c.score;
        }
    }
    for c in chunks.iter_mut() {
        if let Some(&max) = max_per_corpus.get(&c.corpus_id) {
            if max > 0.0 {
                c.score /= max;
            }
        }
    }
    tracing::debug!(
        corpora = ?max_per_corpus.keys().collect::<Vec<_>>(),
        "runtime: normalised per-corpus scores before global merge"
    );
}

/// Build a truncated knowledge context string from corpus-engine scored chunks,
/// grouped by provenance tier (corpus vs web) and staying within a character budget.
fn format_scored_chunks(chunks: &[corpus_engine::ScoredChunk], max_chars: usize) -> String {
    let mut corpus_parts = Vec::new();
    let mut web_parts = Vec::new();
    let mut total = 0;

    for c in chunks {
        let content = truncate_chunk_content(&c.content);
        let title = c.title.as_deref().unwrap_or(c.corpus_id.as_str());

        let (label, bucket) = if c.url.is_some() {
            (format!("[Web: {title}]"), &mut web_parts)
        } else {
            (format!("[Source: {title}]"), &mut corpus_parts)
        };

        let part = format!("{label}\n{content}");
        let part_len = part.len() + 5; // account for separator

        if total + part_len > max_chars {
            break;
        }

        total += part_len;
        bucket.push(part);
    }

    let mut sections = Vec::new();
    if !corpus_parts.is_empty() {
        sections.push(format!(
            "## From knowledge base\n\n{}",
            corpus_parts.join("\n\n---\n\n")
        ));
    }
    if !web_parts.is_empty() {
        sections.push(format!(
            "## From web search\n\n{}",
            web_parts.join("\n\n---\n\n")
        ));
    }

    if sections.is_empty() {
        String::new()
    } else {
        sections.join("\n\n")
    }
}

/// Shared body of [`Runtime::maybe_collaborate`]. Factored out so the
/// streaming spawn (which doesn't hold a live `&self`) can invoke the
/// same logic via owned `Arc`s. See the method's doc comment for
/// behaviour; this function is called whether or not `auto_collaborate`
/// is enabled — it no-ops when disabled.
pub(crate) async fn run_collaboration(
    inference: &dyn InferenceProvider,
    approval: &dyn ApprovalChannel,
    inference_config: &InferenceConfig,
    conversation_id: &str,
    question: &str,
    response: &str,
    evidence: &str,
) -> String {
    if !inference_config.auto_collaborate {
        return response.to_string();
    }

    let t_start = std::time::Instant::now();

    // 1. Ask the gap-identifier whether anything external would sharpen
    //    the answer. Conservative on any error — we never want this
    //    hook to fail the turn.
    let gap = match crate::gap::identify_gap(inference, question, response, evidence).await {
        Ok(Some(req)) => req,
        Ok(None) => {
            tracing::info!(
                latency_ms = t_start.elapsed().as_millis() as u64,
                "maybe_collaborate: no gap identified — passing through"
            );
            return response.to_string();
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "maybe_collaborate: gap check failed — passing through"
            );
            return response.to_string();
        }
    };

    // 2. Stamp task/step on the request so the UI can correlate it
    //    with the current conversation.
    let mut req = gap;
    req.task_id = conversation_id.to_string();
    req.step_id = 0;

    tracing::info!(
        gap_chars = req.gap.len(),
        "maybe_collaborate: surfacing information request"
    );

    // 3. Surface the card and wait for the user.
    let user_content = approval.request_information(&req).await;
    let content = match user_content {
        Some(c) if !c.trim().is_empty() => c,
        _ => {
            tracing::info!(
                latency_ms = t_start.elapsed().as_millis() as u64,
                "maybe_collaborate: user skipped or provided no content"
            );
            return response.to_string();
        }
    };

    // 4. Refinement synthesis — integrate the user's source. The prompt
    //    asks the model to distinguish corpus-derived content from
    //    user-provided content so provenance stays visible.
    let refine_prompt = format!(
        "The user asked: {question}\n\n\
         Your initial answer (drawn from the local corpus):\n{response}\n\n\
         Additional source the user provided:\n{content}\n\n\
         Refine the answer to integrate the user's source. Be explicit \
         about what came from the corpus vs. what came from the user's \
         source. Mark anything that remains uncertain."
    );

    let refine_req = CompletionRequest {
        prompt: refine_prompt,
        system_message: None,
        preferred_speed: Speed::Slow,
        max_tokens: Some(inference_config.max_tokens),
        temperature: Some(inference_config.temperature),
        think_budget: Some(inference_config.think_budget),
        structured_output: None,
        top_k: inference_config.top_k,
        top_p: None,
        oicp: None,
                tools: None,
                tool_choice: None,
    };

    match inference.complete(&refine_req).await {
        Ok(c) => {
            tracing::info!(
                had_user_content = true,
                latency_ms = t_start.elapsed().as_millis() as u64,
                refined_chars = c.text.len(),
                "maybe_collaborate: refined answer produced"
            );
            c.text
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "maybe_collaborate: refinement inference failed — falling back to original"
            );
            response.to_string()
        }
    }
}

/// Post-stream refinement primitive: run the gap check and, if the
/// user provides content, overwrite the saved assistant message and
/// emit `message-refined`. Called both from `handle_message_stream`'s
/// spawn (which has owned `Arc`s but no live `&self`) and from the
/// corresponding method on `Runtime`.
pub(crate) async fn run_post_stream_refinement(
    inference: &dyn InferenceProvider,
    approval: &dyn ApprovalChannel,
    store: &dyn StateStore,
    inference_config: &InferenceConfig,
    conversation_id: &str,
    message_id: &str,
    question: &str,
    original_content: &str,
    evidence: &str,
    original_metadata: Option<serde_json::Value>,
) -> Option<String> {
    let refined = run_collaboration(
        inference,
        approval,
        inference_config,
        conversation_id,
        question,
        original_content,
        evidence,
    )
    .await;
    if refined == original_content {
        return None;
    }

    let updated = Message {
        id: message_id.to_string(),
        conversation_id: conversation_id.to_string(),
        role: Role::Assistant,
        content: refined.clone(),
        created_at: now(),
        metadata: original_metadata,
        version: now(),
    };
    if let Err(e) = store.save_message(&updated).await {
        tracing::warn!(
            error = %e,
            message_id = %message_id,
            "post-stream refinement: save_message failed"
        );
        return None;
    }

    approval.emit_message_refined(MessageRefinedPayload {
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        new_content: refined.clone(),
    });
    Some(refined)
}

/// Pre-computed knowledge context shared between streaming and non-streaming
/// response paths. Produced by [`Runtime::prepare_knowledge_context`] so the
/// two paths cannot diverge in how they search, build prompts, or report
/// provenance.
struct KnowledgeContext {
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

/// Intent-implied OICP defaults. The classified intent carries a
/// capability signal — "DeepQuery" literally means "reasoning-
/// heavy" — and translating it into `CapabilityRequirements` lets
/// the mesh routing layer pick an appropriate backend even when no
/// skill has been activated to declare requirements explicitly.
///
/// These are `preferred` (not `required`) targets: local models
/// that don't hit the level get deprioritised against peers that
/// do, but aren't excluded outright. A Joiner with a 3B can still
/// answer a DeepQuery locally if no beefier peer is reachable.
///
/// Returns `None` for intents that don't imply a capability
/// profile — small-model defaults (SimpleQuery, Continuation,
/// SimpleAction) where cross-network latency wouldn't be worth
/// trading for a marginal quality bump.
fn default_oicp_for_intent(intent: &Intent) -> Option<crate::oicp::InferenceRequirements> {
    use crate::oicp::{Capability, CapabilityRequirements, InferenceRequirements};
    let mut preferred = std::collections::HashMap::new();
    match intent {
        Intent::DeepQuery => {
            // Reasoning-heavy. The user is asking a question that
            // rewards analytical depth; prefer Analysis ≥ 3 + a
            // general-purpose floor so the backend can compose
            // well-structured prose.
            preferred.insert(Capability::Analysis, 3);
            preferred.insert(Capability::General, 3);
        }
        Intent::ComplexTask => {
            // Multi-step execution. The planner will decompose,
            // the executor will run tools; both need strong
            // instruction-following + analytical grounding to
            // stay coherent across steps.
            preferred.insert(Capability::Analysis, 3);
            preferred.insert(Capability::Instruction, 3);
        }
        Intent::KnowledgeQuery => {
            // Retrieval-driven synthesis. Analysis matters less
            // than the raw quality of citing retrieved chunks —
            // moderate Analysis is the floor, with General for
            // coherent prose over the chunks.
            preferred.insert(Capability::Analysis, 2);
            preferred.insert(Capability::General, 2);
        }
        Intent::SimpleQuery
        | Intent::SimpleAction { .. }
        | Intent::Continuation { .. } => {
            return None;
        }
    }
    let caps = CapabilityRequirements {
        required: Default::default(),
        preferred,
    };
    Some(InferenceRequirements::new().with_capabilities(caps))
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
    /// Optional mesh-knowledge client. Populated by the desktop
    /// bootstrap when an `EmbeddedDaemon` is running — the Runtime
    /// fans out knowledge queries through its local Commonwealth
    /// daemon at `127.0.0.1:9741/v1/knowledge/search`, which then
    /// searches local + peer corpora. `None` means "no mesh" — the
    /// standalone (pre-mesh) behavior is preserved exactly.
    pub mesh_knowledge: Option<Arc<dyn crate::traits::MeshKnowledgeSource>>,
    /// Optional [`LandscapeDigestProvider`][crate::traits::LandscapeDigestProvider]
    /// (typically the `sovereign-tools` `KnowledgeViewManager`). When
    /// present, Runtime calls it after routing to splice
    /// `knowledge_view_digests` onto the `ConversationContext` — the
    /// landscape-of-terrain summary consumed by the prompt assembly
    /// layer. `None` = pre-KnowledgeView behaviour preserved exactly;
    /// digests stay `None` and the context carries only memories and
    /// corpus chunks.
    pub landscape_digests: Option<Arc<dyn crate::traits::LandscapeDigestProvider>>,
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
            mesh_knowledge: None,
            landscape_digests: None,
        }
    }

    pub fn with_corpus_engine(mut self, engine: Arc<corpus_engine::CorpusEngine>) -> Self {
        self.corpus_engine = Some(engine);
        self
    }

    /// Install a `KnowledgeView` landscape-digest provider. Typically
    /// the `sovereign-tools::knowledge_view::KnowledgeViewManager`,
    /// constructed alongside the `StateStore` so the same `Arc` can
    /// also be passed as a `StateStoreObserver`.
    ///
    /// Opt-in: leaving this `None` preserves the pre-KnowledgeView
    /// behaviour exactly. Test harnesses that don't wire KnowledgeView
    /// inherit the no-op.
    pub fn with_landscape_digests(
        mut self,
        provider: Arc<dyn crate::traits::LandscapeDigestProvider>,
    ) -> Self {
        self.landscape_digests = Some(provider);
        self
    }

    /// Install a mesh-knowledge client. Only called when the desktop
    /// has an `EmbeddedDaemon` actually running — tests and the
    /// bare CLI path leave this `None`, in which case
    /// `prepare_knowledge_context` behaves exactly as before
    /// (local-only search, `search_method = "LocalOnly"`).
    pub fn with_mesh_knowledge(
        mut self,
        mesh: Arc<dyn crate::traits::MeshKnowledgeSource>,
    ) -> Self {
        self.mesh_knowledge = Some(mesh);
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
        // corpus_id → human-readable peer name, used at the end to
        // stamp `SourceSummary.from_peer` on any corpus whose hits
        // came in via the mesh. Only populated for corpora we
        // don't host locally (so a corpus present both sides stays
        // tagged as local — we don't pretend to "serve from
        // BeefyMac" a corpus we have right here).
        let mut peer_attribution: HashMap<String, String> = HashMap::new();
        // How many hits came from local (before mesh). Drives the
        // computed `search_method` label. `mesh_hits` is derived
        // later from the peer-attribution map after dedupe.
        let mut local_hits: usize = 0;

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

            // Run the local corpus search and the mesh fan-out
            // concurrently — the mesh call does HTTP (up to ~3s
            // budget per peer), the local call is LanceDB disk I/O,
            // so there's no point serialising them. `tokio::join!`
            // waits for both.
            let local_corpora_fut =
                self.search_corpus_indexes(&corpus_embedding, message, 5, &label);
            let mesh_fut = async {
                match &self.mesh_knowledge {
                    Some(m) => m.search(message, &corpus_embedding, 8).await,
                    None => Vec::new(),
                }
            };
            let (local_scored, mesh_scored) = tokio::join!(local_corpora_fut, mesh_fut);
            local_hits = local_scored.len();
            // Glass-box log: how many hits from local vs. mesh, and
            // which corpora did mesh claim to serve? If mesh_hits > 0
            // but `peer_tagged` is 0, the mesh is only round-tripping
            // local corpora — meaning no peer actually hosts anything
            // we're missing. If both are 0 with a live mesh, the
            // handler on :9741 is either not running or returning
            // empty. Reading this line is how you tell.
            let peer_tagged = mesh_scored
                .iter()
                .filter(|h| h.peer_name.is_some())
                .count();
            let mesh_corpora: std::collections::BTreeSet<&str> = mesh_scored
                .iter()
                .map(|h| h.corpus_id.as_str())
                .collect();
            tracing::info!(
                local_hits = local_scored.len(),
                mesh_hits = mesh_scored.len(),
                mesh_peer_tagged = peer_tagged,
                mesh_corpora = ?mesh_corpora,
                "runtime: knowledge fan-out summary"
            );
            all_chunks.extend(local_scored);

            // Fold mesh hits in, tagging peer attribution per corpus.
            // A corpus that already appears locally doesn't get
            // tagged — we own it, mesh is just parroting.
            let local_corpora_ids: std::collections::HashSet<String> =
                all_chunks.iter().map(|c| c.corpus_id.clone()).collect();
            for hit in mesh_scored {
                if !local_corpora_ids.contains(&hit.corpus_id) {
                    if let Some(name) = &hit.peer_name {
                        peer_attribution
                            .entry(hit.corpus_id.clone())
                            .or_insert_with(|| name.clone());
                    }
                }
                all_chunks.push(corpus_engine::ScoredChunk {
                    content: hit.content,
                    title: hit.title,
                    url: hit.url,
                    corpus_id: hit.corpus_id,
                    score: hit.score,
                    metadata: HashMap::new(),
                });
            }

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

        // Put chunks on a comparable score scale before the global
        // merge. See `normalise_scores_per_corpus` for the full
        // rationale — short version: raw BM25 scores aren't
        // comparable across corpora, and on a philosophy query
        // that landed on both a large SEP prose corpus and a small
        // code corpus, the code corpus can produce an outlier
        // score that drowns out the real semantic matches.
        normalise_scores_per_corpus(&mut all_chunks);

        // Dedupe by (corpus_id, content) before truncating so a
        // corpus that appears both locally and via mesh doesn't
        // waste context budget on duplicate chunks.
        all_chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        {
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            all_chunks.retain(|c| seen.insert((c.corpus_id.clone(), c.content.clone())));
        }
        all_chunks.truncate(8);

        // Count mesh hits that survived dedupe so the search_method
        // label reflects what's actually in the prompt.
        let mesh_hits: usize = all_chunks
            .iter()
            .filter(|c| peer_attribution.contains_key(&c.corpus_id))
            .count();

        // 4. Provenance metadata.
        let installed_corpora = self
            .store
            .list_corpus_states()
            .await
            .unwrap_or_default();
        let corpora_searched = !installed_corpora.is_empty() || self.corpus_engine.is_some();

        // Compose a human-readable label that describes *where* the
        // hits came from. This replaces the old hardcoded "LocalOnly"
        // string — the UI surface is unchanged (still a string in
        // `provenance.search_method`), but the content is now
        // truthful.
        let search_method = if all_chunks.is_empty() {
            if self.mesh_knowledge.is_some() {
                if corpora_searched {
                    Some("LocalAndMesh (no matches)".to_string())
                } else {
                    Some("Mesh (no matches)".to_string())
                }
            } else if corpora_searched {
                Some("LocalOnly (no matches)".to_string())
            } else {
                None
            }
        } else if mesh_hits > 0 && local_hits > 0 {
            Some("LocalAndMesh".to_string())
        } else if mesh_hits > 0 {
            Some("MeshOnly".to_string())
        } else {
            Some("LocalOnly".to_string())
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
            .map(|(origin, count)| {
                let from_peer = peer_attribution.get(&origin).cloned();
                SourceSummary {
                    origin,
                    count,
                    from_peer,
                }
            })
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

        // 6. System message — layered confidence when knowledge is present.
        let system = if !all_chunks.is_empty() {
            self.build_primary_system_message(
                &format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{THINKING_DIRECTIVE}"),
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
                    "provenance_tier": if c.url.is_some() { "web" } else { "corpus" },
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

    /// Build OICP requirements for non-Fast requests. Composes two
    /// sources — active skills' declared requirements and a set of
    /// intent-implied defaults — and takes the max per-capability so
    /// a skill can always refine beyond what the intent implies
    /// (e.g. `code-review` asking for `code=3` on a ComplexTask
    /// still keeps its `code=3`, and ComplexTask's `Instruction=3`
    /// merges on top).
    ///
    /// Why intent defaults matter: without them, a user asking
    /// "is free will compatible with determinism?" with no active
    /// skill would send a bare `CompletionRequest` with no OICP,
    /// and the mesh `MeshInferenceProvider` would have nothing to
    /// match against. DeepQuery carries a real capability signal
    /// ("reasoning-heavy") that the OICP layer should see.
    ///
    /// Returns `None` when neither source produces any requirements
    /// — e.g. SimpleQuery with no skill activation; the caller keeps
    /// the request local.
    fn build_oicp(
        &self,
        latency: LatencyPreference,
        intent: &Intent,
    ) -> Option<crate::oicp::InferenceRequirements> {
        let from_skills = self.skills.inference_requirements();
        let from_intent = default_oicp_for_intent(intent);

        let skills_empty =
            from_skills.required().is_empty() && from_skills.preferred().is_empty();
        let intent_empty = from_intent.is_none();
        if skills_empty && intent_empty {
            return None;
        }

        // Merge: max per capability key. Skills are the baseline;
        // intent defaults fill in capabilities the skills didn't
        // mention, without ever downgrading a skill-declared level.
        let mut required = from_skills.required().clone();
        let mut preferred = from_skills.preferred().clone();
        if let Some(ref defaults) = from_intent {
            for (cap, level) in defaults.required().iter() {
                let entry = required.entry(*cap).or_insert(0);
                *entry = (*entry).max(*level);
            }
            for (cap, level) in defaults.preferred().iter() {
                let entry = preferred.entry(*cap).or_insert(0);
                *entry = (*entry).max(*level);
            }
        }

        let caps = crate::oicp::CapabilityRequirements {
            required,
            preferred,
        };
        // Preserve whatever sharding `from_skills` resolved to —
        // `SkillRegistry::inference_requirements` defaults to
        // `MeshAllowed` and flips to `LocalOnly` only when an
        // active skill has declared `privacy = "local_only"`
        // (e.g. `inner-work`). Rebuilding via
        // `InferenceRequirements::new()` would silently reset it
        // to `LocalOnly` (the OICP spec default) and block every
        // cross-mesh route, so we copy the skill-resolved value
        // through.
        let sharding = from_skills.sharding();
        Some(
            crate::oicp::InferenceRequirements::new()
                .with_capabilities(caps)
                .with_latency(latency)
                .with_sharding(sharding),
        )
    }

    /// Spawn a background task that generates an auto-title for the
    /// conversation if one isn't already set. Non-blocking — failures are
    /// logged and do not affect the caller.
    ///
    /// `try_auto_title` is idempotent: safe to call after every assistant
    /// message save. It exits early when the title is already set or the
    /// conversation doesn't have enough messages yet.
    fn spawn_auto_title(&self, conversation_id: &str) {
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let cid = conversation_id.to_string();
        tokio::spawn(async move {
            if let Err(e) =
                crate::title::try_auto_title(inference.as_ref(), store.as_ref(), &cid).await
            {
                tracing::warn!(
                    conversation_id = %cid,
                    error = %e,
                    "auto-title: generation failed"
                );
            }
        });
    }

    /// Build a system message that includes memory context.
    fn build_system_message(&self, base: &str, context: &ConversationContext) -> String {
        // Invariant check: the Runtime is required to splice
        // `knowledge_view_digests` after routing (via
        // `LandscapeDigestProvider::splice_landscape_digests`). If we
        // reach system-message assembly with the field still `None`,
        // something skipped the splice — most likely a new code path
        // that builds its own ConversationContext and went straight
        // to the LLM. Debug-builds panic loudly so the oversight is
        // caught in tests; release builds proceed without the digest.
        //
        // The guard is tolerant of the no-KnowledgeView configuration
        // (the field stays `None` when `Runtime::with_landscape_digests`
        // wasn't called — e.g. unit-test harnesses). We only assert
        // when a provider is installed.
        if self.landscape_digests.is_some() {
            context.debug_assert_routed();
        }

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

        // KnowledgeView landscape digests — the person's recurring
        // terrain (clusters, fault lines, open questions) that the
        // model reads before answering. Bounded at splice time by
        // `KnowledgeViewManager::splice_into`'s per-view token budget
        // (300 + 200 in v1).
        if let Some(digests) = context.knowledge_view_digests.as_ref() {
            for d in digests {
                let body = d.body.trim();
                if !body.is_empty() {
                    parts.push(body.to_string());
                }
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

    /// Epistemic humility hook: audit the just-produced answer against
    /// its evidence and, if the model judges a specific external source
    /// would materially sharpen the answer, surface an
    /// [`InformationRequest`] card via the approval channel. If the
    /// user pastes content, re-synthesise the answer with that content
    /// folded in. Otherwise return the original response unchanged.
    ///
    /// **Pure-additive**: never makes the answer worse than the
    /// corpus-only baseline — any failure (inference error, parse
    /// failure, user skip) falls back to `response` unchanged. Gated
    /// by `InferenceConfig::auto_collaborate` (default on) so the
    /// whole path is a no-op when disabled.
    ///
    /// Callers pass `evidence` as a plain-text summary of whatever
    /// corpus material grounded the original answer. Empty string is
    /// acceptable (e.g. when corpus retrieval returned nothing).
    pub async fn maybe_collaborate(
        &self,
        conversation_id: &str,
        question: &str,
        response: &str,
        evidence: &str,
    ) -> String {
        run_collaboration(
            self.inference.as_ref(),
            self.approval.as_ref(),
            &self.inference_config,
            conversation_id,
            question,
            response,
            evidence,
        )
        .await
    }

    /// Post-stream refinement hook: runs the gap check against the
    /// already-streamed answer; if the user pastes content, overwrites
    /// the saved assistant message and emits a `message-refined` event
    /// so the UI can replace the bubble. Returns `Some(refined_text)`
    /// when refinement produced new content, `None` otherwise.
    ///
    /// Delegates to `run_post_stream_refinement` so the streaming
    /// spawn (which doesn't hold `&self`) and tests share one code
    /// path.
    pub async fn apply_post_stream_refinement(
        &self,
        conversation_id: &str,
        message_id: &str,
        question: &str,
        original_content: &str,
        evidence: &str,
        original_metadata: Option<serde_json::Value>,
    ) -> Option<String> {
        run_post_stream_refinement(
            self.inference.as_ref(),
            self.approval.as_ref(),
            self.store.as_ref(),
            &self.inference_config,
            conversation_id,
            message_id,
            question,
            original_content,
            evidence,
            original_metadata,
        )
        .await
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

        tracing::info!(count = extracted.len(), "memory: extracted long-term memories");
        for mut mem in extracted {
            // Tag each extracted memory with the conversation it
            // came from. Enables the `personal-knowledge`
            // KnowledgeView to surface cluster membership
            // alongside conversation-level metadata (title, skill)
            // at digest time, and makes `memories.source_conversation_id`
            // no longer NULL on fresh writes post-migration.
            mem.source_conversation_id = Some(conversation_id.to_string());
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
            tracing::info!(pruned, "memory: pruned decayed memories");
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
    #[tracing::instrument(
        name = "runtime.handle_message_stream",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_message_stream(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<StreamHandle> {
        tracing::info!("runtime: stream turn begin");
        // 1. Build context.
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;
        tracing::debug!(
            messages = context.conversation.messages.len(),
            memories = context.memories.len(),
            installed_corpora = context.installed_corpora.len(),
            "runtime: stream context built"
        );

        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 1b. Update topic context for turn-aware routing.
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
        )
        .await
        .ok();
        context.topic_context = topic_context;

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

        // 2b. Tag the conversation with the skill that was active
        // when it started. The store upsert is idempotent — only
        // the first call with a non-NULL skill wins, later calls
        // are no-ops. The KnowledgeView conversational acquirer
        // reads this column to exclude `privacy = local_only`
        // skills (e.g. `inner-work`) from the shared corpus.
        if let Some(skill_id) = self.skills.primary_skill_id_for_conversation() {
            if let Err(e) = self
                .store
                .set_conversation_skill_if_unset(conversation_id, &skill_id)
                .await
            {
                tracing::debug!(
                    conversation_id,
                    error = %e,
                    "failed to tag conversation with skill_id; continuing"
                );
            }
        }

        // 3. Route.
        let tool_descriptors = self.tools.descriptors();
        let RoutingOutcome { intent, coarse_intent, self_assessment } = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            "runtime: stream routed"
        );

        // Document attached or ComplexTask → fall back to non-streaming.
        if message.starts_with("[Document attached: ")
            || matches!(intent, Intent::ComplexTask | Intent::KnowledgeQuery)
        {
            tracing::info!(
                intent = ?intent,
                "runtime: stream not supported for this intent — falling back"
            );
            return Err(Error::NotImplemented(
                "Streaming not supported for this intent".into(),
            ));
        }

        // 3b. Splice KnowledgeView landscape digests now that routing
        // has resolved. The provider (typically the sovereign-tools
        // KnowledgeViewManager) reads the enriched indexes for each
        // built-in view and writes a markdown summary into
        // `context.knowledge_view_digests` so prompt assembly can
        // surface "here's the person's terrain" before synthesis.
        // v1 passes `active_skill=None` — per-skill digest filtering
        // is v2 work. The acquirer-level privacy separation already
        // keeps `local_only` skill content out of the conversational
        // corpus.
        if let Some(provider) = &self.landscape_digests {
            provider
                .splice_landscape_digests(&mut context, None)
                .await;
        }

        // 4. Search knowledge + build prompt (shared with handle_simple).
        let kc = self
            .prepare_knowledge_context(message, &context, &intent)
            .await;

        let oicp = if matches!(intent, Intent::SimpleQuery) {
            None
        } else {
            self.build_oicp(LatencyPreference::BestEffort, &intent)
        };

        // Model ID is captured from `complete_stream_with_id` once
        // the provider has committed to a routing decision — see
        // the trait docs on that method. Using the pre-stream sync
        // `model_id_for` here would miss peer attribution (the
        // mesh wrapper can only report "I routed to peer X" after
        // its async `select_peer` pass has run).
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
                    tools: None,
                    tool_choice: None,
        };

        let search_method = kc.search_method;
        let sources = kc.sources;
        let retrieved_chunks = kc.retrieved_chunks;

        // Format the corpus evidence now so the post-stream epistemic-
        // humility hook can feed it to the gap checker. Moved into the
        // streaming spawn; not used before the synthesis completes.
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let question = message.to_string();

        let intent_label = format!("{intent:?}");
        let message_id = uuid::Uuid::new_v4().to_string();

        // 5. Spawn streaming task.
        let inference = Arc::clone(&self.inference);
        let store = Arc::clone(&self.store);
        let approval = Arc::clone(&self.approval);
        let inference_config = self.inference_config.clone();
        let conversation_id_owned = conversation_id.to_string();
        let message_id_owned = message_id.clone();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String>>(64);

        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut full_text = String::new();

            let (mut s, model_id) = match inference
                .complete_stream_with_id(&request)
                .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };
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
            let metadata_json = serde_json::json!({
                "streamed": true,
                "provenance": provenance,
                "retrieved_chunks": retrieved_chunks,
            });
            let assistant_msg = Message {
                id: message_id_owned.clone(),
                conversation_id: conversation_id_owned.clone(),
                role: Role::Assistant,
                content: full_text.clone(),
                created_at: now(),
                metadata: Some(metadata_json.clone()),
                version: now(),
            };
            let _ = store.save_message(&assistant_msg).await;

            // Epistemic-humility hook (post-stream): audit the streamed
            // answer and, if the user provides additional content, rewrite
            // the persisted message and emit a `message-refined` event so
            // the UI can update the bubble in place. Runs concurrently
            // with auto-title so neither blocks the other.
            let collab_inference = Arc::clone(&inference);
            let collab_store = Arc::clone(&store);
            let collab_approval = Arc::clone(&approval);
            let collab_config = inference_config.clone();
            let collab_cid = conversation_id_owned.clone();
            let collab_mid = message_id_owned.clone();
            let collab_question = question.clone();
            let collab_evidence = evidence.clone();
            let collab_original = full_text.clone();
            let collab_metadata = metadata_json;
            tokio::spawn(async move {
                run_post_stream_refinement(
                    collab_inference.as_ref(),
                    collab_approval.as_ref(),
                    collab_store.as_ref(),
                    &collab_config,
                    &collab_cid,
                    &collab_mid,
                    &collab_question,
                    &collab_original,
                    &collab_evidence,
                    Some(collab_metadata),
                )
                .await;
            });

            // Auto-title after first exchange. Non-blocking; the stream has
            // already delivered the response to the user.
            let title_inference = Arc::clone(&inference);
            let title_store = Arc::clone(&store);
            let title_cid = conversation_id_owned.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::title::try_auto_title(
                    title_inference.as_ref(),
                    title_store.as_ref(),
                    &title_cid,
                )
                .await
                {
                    tracing::warn!(
                        conversation_id = %title_cid,
                        error = %e,
                        "auto-title: generation failed (stream path)"
                    );
                }
            });
        });

        let stream: Pin<Box<dyn Stream<Item = Result<String>> + Send>> =
            Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));

        Ok(StreamHandle { message_id, stream })
    }

    #[tracing::instrument(
        name = "runtime.handle_message",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_message(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        // Save the user message first so `handle_turn` sees it in the
        // conversation history during context building and routing.
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

        // Tag the conversation with the active skill on first message
        // (idempotent — see the streaming-path equivalent).
        if let Some(skill_id) = self.skills.primary_skill_id_for_conversation() {
            if let Err(e) = self
                .store
                .set_conversation_skill_if_unset(conversation_id, &skill_id)
                .await
            {
                tracing::debug!(
                    conversation_id,
                    error = %e,
                    "failed to tag conversation with skill_id; continuing"
                );
            }
        }

        self.handle_turn(message, conversation_id).await
    }

    /// Run a conversation turn assuming the user message has **already** been
    /// saved as the latest message in the conversation.
    ///
    /// Callers that need to save the user message with custom metadata — for
    /// example the `ask_document` Tauri command which tags the message with
    /// the attached asset id — can call this entry point directly. The
    /// runtime pipeline (context build, working-memory compression, topic
    /// context, routing, synthesis, auto-title) then proceeds identically
    /// to [`Self::handle_message`].
    ///
    /// Build-context reads all existing messages from the store, so the
    /// pre-saved user message is included in the in-memory context without
    /// the caller having to push it explicitly.
    #[tracing::instrument(
        name = "runtime.handle_turn",
        skip(self, message),
        fields(conversation_id = %conversation_id, message_chars = message.len())
    )]
    pub async fn handle_turn(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        let turn_start = std::time::Instant::now();
        let has_doc_prefix = message.starts_with("[Document attached: ");
        tracing::info!(has_doc_prefix, "runtime: turn begin");

        // 1. Build context from store (use message text for memory retrieval).
        //    The user message is already persisted so it shows up here.
        let mut context = build_context(self.store.as_ref(), conversation_id, message).await?;
        tracing::debug!(
            messages = context.conversation.messages.len(),
            memories = context.memories.len(),
            installed_corpora = context.installed_corpora.len(),
            has_document_session = context.document_session.is_some(),
            "runtime: context built"
        );

        // 1b. Compress working memory from conversation history (now including
        //     the latest user message — gives working-memory extraction a
        //     crisper view of current intent).
        let working_memory = memory::compress_working_memory(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.working_memory.as_ref(),
        )
        .await
        .ok();
        context.working_memory = working_memory;

        // 1c. Update topic context for turn-aware routing. Latest user
        //     message is part of the extraction input.
        let topic_context = crate::context::update_topic_context(
            self.inference.as_ref(),
            &context.conversation.messages,
            context.topic_context.as_ref(),
            context.document_session.as_ref(),
        )
        .await
        .ok();
        context.topic_context = topic_context;

        // 2. Route.
        let tool_descriptors = self.tools.descriptors();
        let RoutingOutcome { intent, coarse_intent, self_assessment } = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;

        tracing::info!(
            intent = ?intent,
            coarse = ?coarse_intent,
            self_assessment = ?self_assessment,
            "runtime: routed"
        );

        // 2b. Splice KnowledgeView landscape digests (same hook as
        // handle_message_stream). No-op when
        // `Runtime::with_landscape_digests` wasn't called at build
        // time. See the streaming path for rationale on `active_skill`.
        if let Some(provider) = &self.landscape_digests {
            provider
                .splice_landscape_digests(&mut context, None)
                .await;
        }

        // When a legacy [Document attached: ...] prefix is used, bypass the
        // planner entirely and route to the map-reduce document_operation path.
        if let Some(rest) = message.strip_prefix("[Document attached: ") {
            if let Some(end) = rest.find(']') {
                let source = rest[..end].to_string();
                let user_query = rest[end + 1..].trim().to_string();
                tracing::info!(
                    source = %source,
                    user_query_chars = user_query.len(),
                    "runtime: dispatching to handle_document_operation"
                );
                let result = self
                    .handle_document_operation(
                        &source,
                        &user_query,
                        message,
                        conversation_id,
                        &context,
                    )
                    .await;
                tracing::info!(
                    success = result.is_ok(),
                    total_latency_ms = turn_start.elapsed().as_millis() as u64,
                    "runtime: turn end (document_operation)"
                );
                return result;
            }
        }

        // 3. Dispatch based on intent.
        let dispatch = match intent {
            Intent::ComplexTask => "handle_complex_task",
            Intent::KnowledgeQuery => "handle_knowledge_query",
            _ => "handle_simple",
        };
        tracing::info!(dispatch, "runtime: dispatching");

        let result = match intent {
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
        };

        tracing::info!(
            dispatch,
            success = result.is_ok(),
            total_latency_ms = turn_start.elapsed().as_millis() as u64,
            "runtime: turn end"
        );
        result
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
            self.build_oicp(LatencyPreference::BestEffort, &intent)
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
                    tools: None,
                    tool_choice: None,
        };

        let completion = self.inference.complete(&request).await?;

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // No-ops when disabled. Evidence is the same formatted-chunks text
        // that was injected into the synthesis prompt (or empty if no
        // corpus material was retrieved).
        let evidence = format_scored_chunks(&kc.chunks, MAX_KNOWLEDGE_CHARS);
        let final_content = self
            .maybe_collaborate(conversation_id, message, &completion.text, &evidence)
            .await;

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
            content: final_content,
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
        self.spawn_auto_title(conversation_id);

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

        tracing::info!(message_chars = message.len(), "handle_knowledge_query: begin");

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

        tracing::info!(
            chunks_found = chunks.len(),
            search_ms = t_search.elapsed().as_millis() as u64,
            "handle_knowledge_query: corpus search done"
        );

        // 4a. Empty results path — answer from parametric knowledge.
        if chunks.is_empty() {
            tracing::info!("KnowledgeQuery: no chunks — answering from parametric knowledge");
            let corpora = context.installed_corpora_display();
            let prompt = format!(
                "The user asked: \"{message}\"\n\n\
                 You searched these installed knowledge sources: {corpora}\n\
                 The search returned no relevant results.\n\n\
                 Answer the question from your general knowledge. \
                 Note briefly that no corpus results were found, but do not refuse \
                 to answer or dwell on the absence of sources. \
                 If you are confident about the topic, answer directly and substantively. \
                 If you are genuinely uncertain, say so and suggest web search or \
                 installing an additional corpus."
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
                tools: None,
                tool_choice: None,
            };
            let completion = self.inference.complete(&request).await?;

            // Auto-collaboration hook: corpus was empty so the evidence
            // string is empty too — this is the strongest case for asking
            // the user to supply something.
            let final_content = self
                .maybe_collaborate(conversation_id, message, &completion.text, "")
                .await;

            let assistant_msg = Message {
                id: uuid::Uuid::new_v4().to_string(),
                conversation_id: conversation_id.to_string(),
                role: Role::Assistant,
                content: final_content,
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
            self.spawn_auto_title(conversation_id);
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

        let system = self.build_primary_system_message(
            &format!("{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{THINKING_DIRECTIVE}"),
            context,
        );

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
            oicp: self.build_oicp(LatencyPreference::BestEffort, &Intent::KnowledgeQuery),
            tools: None,
            tool_choice: None,
        };

        let completion = self.inference.complete(&request).await?;

        // Auto-collaboration hook: re-use the same formatted-chunks text
        // that was fed to synthesis as the evidence for the gap check.
        let final_content = self
            .maybe_collaborate(conversation_id, message, &completion.text, &doc_context)
            .await;

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }
        let provenance = ResponseProvenance {
            intent: "KnowledgeQuery".to_string(),
            search_method: Some("CorpusEngine".to_string()),
            sources: source_map
                .into_iter()
                .map(|(origin, count)| SourceSummary {
                    origin,
                    count,
                    from_peer: None,
                })
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
            content: final_content,
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
        self.spawn_auto_title(conversation_id);

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
        tracing::info!(source_hint = %source_hint, "runtime: document_operation — resolving source");

        // 1. Resolve actual source path from the store.
        let sources = self.store.list_sources().await.unwrap_or_default();
        let source_lower = source_hint.to_lowercase();
        let resolved_source = sources
            .iter()
            .find(|s| s.to_lowercase().contains(&source_lower))
            .cloned()
            .unwrap_or_else(|| source_hint.to_string());

        tracing::debug!(
            resolved_source = %resolved_source,
            available_sources = sources.len(),
            "runtime: document_operation — source resolved"
        );

        // Get chunk count for the prompt.
        let chunks = self.store.get_chunks_by_source(&resolved_source).await.unwrap_or_default();
        let chunk_count = chunks.len();
        let word_count: usize = chunks.iter().map(|c| c.content.split_whitespace().count()).sum();
        drop(chunks);

        if chunk_count == 0 {
            tracing::warn!(
                source = %resolved_source,
                "runtime: document_operation — no chunks found for source"
            );
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
            self.spawn_auto_title(conversation_id);
            return Ok(Response { message: assistant_msg, task: None });
        }

        tracing::info!(
            source = %resolved_source,
            chunks = chunk_count,
            words = word_count,
            user_query_chars = user_query.len(),
            "runtime: document_operation — generating map/reduce prompts"
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
            // Use the primary model for prompt generation — it's a one-time
            // cost and the 0.6B fast model can't reliably produce JSON.
            preferred_speed: Speed::Slow,
            max_tokens: Some(512),
            temperature: Some(0.0),
            think_budget: Some(0), // no thinking — just produce the JSON
            structured_output: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "map_prompt": { "type": "string" },
                    "reduce_prompt": { "type": "string" }
                },
                "required": ["map_prompt", "reduce_prompt"]
            })),
            // think_budget already set above
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
        };

        let prompt_response = self.inference.complete(&prompt_request).await?;
        let prompt_text = prompt_response.text.trim();

        // Parse the generated prompts. Strip think tags and code fences
        // before parsing — models often wrap JSON in these.
        let cleaned = prompt_text
            // Strip <think>...</think> blocks (Qwen3 thinking mode).
            .split("</think>")
            .last()
            .unwrap_or(prompt_text)
            .trim()
            // Strip markdown code fences.
            .strip_prefix("```json")
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(
                prompt_text
                    .split("</think>")
                    .last()
                    .unwrap_or(prompt_text)
                    .trim()
            )
            .trim();

        let (map_prompt, reduce_prompt) = match serde_json::from_str::<serde_json::Value>(cleaned) {
            Ok(v) => {
                let mp = v.get("map_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Extract key information relevant to the user's question from this passage."
                ).to_string();
                let rp = v.get("reduce_prompt").and_then(|v| v.as_str()).unwrap_or(
                    "Synthesize all extracted information into a comprehensive answer."
                ).to_string();
                (mp, rp)
            }
            Err(e) => {
                // Fallback: use specific prompts tailored to the user's question.
                tracing::warn!(
                    error = %e,
                    raw_output = %prompt_text,
                    "Failed to parse prompt JSON — using tailored fallback prompts"
                );
                (
                    format!(
                        "Read this passage carefully. The user asked: \"{user_query}\"\n\n\
                         Extract ALL information from this passage that is relevant to \
                         answering the user's question. Include:\n\
                         - Key facts, events, or arguments\n\
                         - Character names and their actions (if narrative)\n\
                         - Direct quotes that are significant\n\
                         If nothing relevant appears, respond with just: null"
                    ),
                    format!(
                        "The user asked: \"{user_query}\"\n\n\
                         You have been given extracted notes from across an entire document. \
                         Synthesize ALL the extracted information into a comprehensive, \
                         well-organized answer to the user's question. \
                         Be thorough — include every relevant detail from the notes. \
                         Organize logically with clear sections."
                    ),
                )
            }
        };

        tracing::debug!(
            map_prompt_chars = map_prompt.len(),
            reduce_prompt_chars = reduce_prompt.len(),
            "runtime: document_operation — prompts generated"
        );
        tracing::info!("runtime: document_operation — invoking map/reduce");

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

        tracing::info!(
            output_chars = result_text.len(),
            "runtime: document_operation — complete"
        );

        // 4. Build response.
        let provenance = ResponseProvenance {
            intent: "DocumentOperation".to_string(),
            search_method: Some("document_operation".to_string()),
            sources: vec![SourceSummary {
                origin: "user_document".to_string(),
                count: chunk_count,
                from_peer: None,
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
        self.spawn_auto_title(conversation_id);

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

        tracing::info!("runtime: complex_task — generating plan");
        let plan = self
            .planner
            .plan(message, context, tool_descriptors)
            .await?;

        tracing::info!(
            steps = plan.steps.len(),
            "runtime: complex_task — plan generated"
        );
        for step in &plan.steps {
            tracing::debug!(
                step_id = step.id,
                description = %step.description,
                kind = ?std::mem::discriminant(&step.kind),
                "runtime: complex_task — step"
            );
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
            tracing::warn!(
                step_id = error.step_id,
                error = %error.message,
                "runtime: complex_task — step failed, attempting replan"
            );

            let completed_vec: Vec<(usize, StepOutput)> =
                result.completed.iter().map(|(&k, v)| (k, v.clone())).collect();

            match self.planner.replan(&plan, &completed_vec, error).await {
                Ok(new_plan) => {
                    tracing::info!(
                        steps = new_plan.steps.len(),
                        "runtime: complex_task — replan generated"
                    );
                    task.plan = new_plan.clone();
                    task.status = TaskStatus::Running;
                    task.updated_at = now();

                    let mut retry_ctx = TaskContext {
                        task: task.clone(),
                        completed: HashMap::new(),
                    };

                    result = executor.run(&new_plan, &mut retry_ctx).await?;

                    if result.error.is_some() {
                        tracing::warn!("runtime: complex_task — replan also failed");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "runtime: complex_task — replan failed");
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
                oicp: self.build_oicp(LatencyPreference::Throughput, &Intent::ComplexTask),
            tools: None,
            tool_choice: None,
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
                                    from_peer: None,
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
                            from_peer: None,
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

        // Epistemic-humility hook (see Runtime::maybe_collaborate).
        // Evidence is the same `step_summaries` the synthesis prompt saw
        // — keeps the gap check grounded in exactly what the model had.
        let evidence = step_summaries.join("\n\n");
        let final_content = self
            .maybe_collaborate(conversation_id, message, &synthesis.text, &evidence)
            .await;

        let assistant_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: final_content,
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
        self.spawn_auto_title(conversation_id);

        Ok(Response {
            message: assistant_msg,
            task: Some(task),
        })
    }
}

#[cfg(test)]
mod score_normalisation_tests {
    use super::normalise_scores_per_corpus;
    use corpus_engine::ScoredChunk;
    use std::collections::HashMap;

    fn chunk(corpus: &str, content: &str, score: f32) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some(content.into()),
            url: None,
            corpus_id: corpus.into(),
            score,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn sep_beats_code_after_normalisation() {
        // Reconstruct the observed scenario from the 08:40 demo
        // logs: a BM25 outlier from a small code corpus (score
        // 21.37) was out-ranking SEP's genuine top match (19.25)
        // in the merged list even though SEP had 8 consistently-
        // strong hits and code had 1 outlier + a long tail.
        //
        // After per-corpus max normalisation the code outlier
        // reduces to 1.0, SEP's top also reduces to 1.0, but
        // SEP's *second* chunk at 0.954 outranks code's *second*
        // chunk at 0.700 — so top-8 ends up dominated by SEP.
        let mut chunks = vec![
            // SEP: 8 strong, clustered hits (realistic shape).
            chunk("sep", "compatibilism", 19.25),
            chunk("sep", "incompatibilism-arguments-1", 18.37),
            chunk("sep", "locke-freedom", 18.16),
            chunk("sep", "providence-divine", 16.95),
            chunk("sep", "incompatibilism-arguments-2", 16.83),
            chunk("sep", "incompatibilism-arguments-3", 16.57),
            chunk("sep", "frankfurt-aim", 15.65),
            chunk("sep", "moral-responsibility", 15.48),
            // corpus-engine: 1 spurious outlier + long tail.
            chunk("corpus-engine", "extract_questions_prefers_canonical", 21.37),
            chunk("corpus-engine", "test_skeleton", 14.96),
            chunk("corpus-engine", "mock_inference_fn", 14.80),
            // sovereign: a code corpus with middling matches.
            chunk("sovereign", "needs_deep_reasoning", 16.44),
            chunk("sovereign", "LlmRouter", 12.91),
        ];

        normalise_scores_per_corpus(&mut chunks);
        chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let top8: Vec<&str> = chunks
            .iter()
            .take(8)
            .map(|c| c.corpus_id.as_str())
            .collect();

        // The code outlier still appears (score 1.0, tied for
        // top) — that's correct: it IS its corpus's top hit, so
        // the merge can't hide it without losing evidence from
        // that corpus entirely. But SEP should contribute at
        // least 5 of the top 8, because its within-corpus ranks
        // 2..=6 all score above code's rank 2 after rescaling.
        let sep_count = top8.iter().filter(|&&c| c == "sep").count();
        assert!(
            sep_count >= 5,
            "expected SEP to dominate top-8 after normalisation; \
             got corpus list {top8:?}"
        );
    }

    #[test]
    fn preserves_within_corpus_ranking() {
        // Within a single corpus, the rescaling must not reorder
        // hits — just compress the scale to [0, 1].
        let mut chunks = vec![
            chunk("one", "best", 20.0),
            chunk("one", "mid", 10.0),
            chunk("one", "worst", 5.0),
        ];
        normalise_scores_per_corpus(&mut chunks);
        assert_eq!(chunks[0].content, "best");
        assert!((chunks[0].score - 1.0).abs() < 1e-6);
        assert!((chunks[1].score - 0.5).abs() < 1e-6);
        assert!((chunks[2].score - 0.25).abs() < 1e-6);
    }

    #[test]
    fn empty_input_is_a_noop() {
        // Guard against divide-by-zero or panic on empty slice —
        // `search_corpus_indexes` can legitimately return zero
        // hits when the query embedding is empty AND FTS returns
        // nothing.
        let mut chunks: Vec<ScoredChunk> = Vec::new();
        normalise_scores_per_corpus(&mut chunks);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_corpus_zero_max_stays_zero() {
        // If every hit in a corpus scored 0.0 (shouldn't happen
        // in practice — ScoredChunk implies at least a bare
        // match), the rescaler must not divide by zero.
        let mut chunks = vec![
            chunk("empty", "a", 0.0),
            chunk("empty", "b", 0.0),
        ];
        normalise_scores_per_corpus(&mut chunks);
        assert_eq!(chunks[0].score, 0.0);
        assert_eq!(chunks[1].score, 0.0);
    }
}
