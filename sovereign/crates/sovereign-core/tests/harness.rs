// SPDX-License-Identifier: AGPL-3.0-or-later
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::error::{Error, Result};
use sovereign_core::executor::AutoApprovalChannel;
use sovereign_core::planner::LlmPlanner;
use sovereign_core::runtime::Runtime;
use sovereign_core::stubs::PassthroughRouter;
use sovereign_core::traits::*;
use sovereign_core::types::*;
use sovereign_core::SkillRegistry;
use sovereign_core::ToolRegistry;
use sovereign_store::sqlite::SqliteStateStore;

// ─── Deterministic Inference Provider ────────────────────────
//
// A rules-based inference provider that produces structured, predictable
// responses based on prompt content. Not a mock — it exercises real code
// paths. The runtime, executor, search pipeline, memory system, and store
// all run with real logic.

pub struct DeterministicInference;

#[async_trait]
impl InferenceProvider for DeterministicInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let prompt_lower = request.prompt.to_lowercase();

        let text = if prompt_lower.contains("classify this document into one category") {
            // detect_document_type — return a deterministic category.
            // Must be checked BEFORE the "categories:" router pattern, since
            // detect_document_type prompts also contain "Categories:".
            "Argument".to_string()
        } else if prompt_lower.contains("extract named entities mentioned in each of the") {
            // build_skeleton batch — lean lines format (May 2026 rewrite).
            // The grammar enforces N comma-separated capitalized names,
            // one line per chunk in the batch. Repeat "Test Entity" so
            // every chunk in the batch surfaces it; parser dedupes
            // entity mentions across chunks.
            let batch_size = if prompt_lower.contains("output exactly 4 lines") {
                4
            } else if prompt_lower.contains("output exactly 3 lines") {
                3
            } else if prompt_lower.contains("output exactly 2 lines") {
                2
            } else {
                1
            };
            std::iter::repeat_n("Test Entity", batch_size)
                .collect::<Vec<_>>()
                .join("\n")
        } else if prompt_lower.contains("write a single paragraph")
            && prompt_lower.contains("overview")
        {
            // generate_overview — short deterministic paragraph.
            "This is a deterministic test overview covering the document's main concept."
                .to_string()
        } else if prompt_lower.contains("a, b, or c")
            || prompt_lower.contains("a) simple")
            || prompt_lower.contains("categories:")
        {
            // Router classification — route to DeepQuery
            "B".to_string()
        } else if prompt_lower.contains("routine")
            && prompt_lower.contains("moderate")
            && prompt_lower.contains("hard")
        {
            // Difficulty estimation
            "moderate".to_string()
        } else if prompt_lower.contains("yes or no") {
            "yes".to_string()
        } else if prompt_lower.contains("\"steps\"") && prompt_lower.contains("\"edges\"") {
            // Plan generation
            r#"{"goal":"test","steps":[{"id":0,"description":"answer","kind":"reason","prompt":"Answer the question","speed":"slow"}],"edges":[]}"#.to_string()
        } else if prompt_lower.contains("how to search")
            && prompt_lower.contains("[search results for")
        {
            // ReasonWithTools: has search results — synthesize now
            "Based on what I found, here is the answer. [Source: sep] The knowledge base confirms this.".to_string()
        } else if prompt_lower.contains("how to search") && prompt_lower.contains("available tools")
        {
            // ReasonWithTools: first iteration — emit a tool call
            r#"Let me search for relevant information. <tool_call>{"tool":"search","query":"Bergson laughter humor"}</tool_call>"#.to_string()
        } else if prompt_lower.contains("you have used all available searches") {
            // ReasonWithTools: forced synthesis after hitting cap
            "Forced synthesis after reaching search limit. [Source: sep] Based on available findings.".to_string()
        } else if prompt_lower.contains("relevant knowledge:") {
            // Synthesis with knowledge context
            "Based on the provided knowledge, here is the answer. [Source: local knowledge] The sources indicate this is correct.".to_string()
        } else if prompt_lower.contains("search results") {
            "Based on the sources provided, [1] indicates the answer. [2] supports this."
                .to_string()
        } else if prompt_lower.contains("\"pass\"") && prompt_lower.contains("feedback") {
            r#"{"pass": true}"#.to_string()
        } else if prompt_lower.contains("select the best") {
            "1".to_string()
        } else if prompt_lower.contains("tension-detector")
            || (prompt_lower.contains("prior memories") && prompt_lower.contains("relation"))
        {
            // R3 temporal-tension pre-pass — return empty array
            // (no tensions found) so functional tests that
            // happen to exercise relational skills don't see
            // spurious tension cues.
            "[]".to_string()
        } else if prompt_lower.contains("extract") && prompt_lower.contains("memor") {
            // Memory extraction — return empty to avoid side effects
            "No new facts to extract.".to_string()
        } else if prompt_lower.contains("working memory") || prompt_lower.contains("current goal") {
            // Working memory compression
            r#"{"current_goal": null, "facts": [], "active_documents": []}"#.to_string()
        } else if prompt_lower.contains("short title") {
            // Title generation — deterministic output so tests can assert.
            // Matches the current prompt ("Give each conversation a short
            // title of a few words"); the prior matcher ("write a short,
            // specific title") drifted dead when the prompt was reworded.
            "Test conversation title".to_string()
        } else if prompt_lower.contains("extract the topic and domain") {
            // Topic context extraction — derive topic and domain from message content.
            let topic =
                if prompt_lower.contains("schrödinger") || prompt_lower.contains("schrodinger") {
                    "Schrödinger"
                } else if prompt_lower.contains("buddhis")
                    || prompt_lower.contains("theravada")
                    || prompt_lower.contains("zen")
                {
                    "Buddhist philosophy"
                } else {
                    "general topic"
                };
            let domain = if prompt_lower.contains("buddhis")
                || prompt_lower.contains("theravada")
                || prompt_lower.contains("zen")
            {
                "buddhism"
            } else if prompt_lower.contains("schrödinger")
                || prompt_lower.contains("schrodinger")
                || prompt_lower.contains("quantum")
            {
                "physics"
            } else {
                "general"
            };
            format!(r#"{{"topic": "{topic}", "domain": "{domain}"}}"#)
        } else if prompt_lower.contains("no relevant results")
            || prompt_lower.contains("no corpus results")
        {
            // Empty-results path — answer from parametric knowledge (new layered confidence behavior)
            "While no corpus results were found, from general knowledge: this topic is well-studied. Here is a substantive answer based on established knowledge.".to_string()
        } else {
            // Default: deterministic echo response
            let mut end = request.prompt.len().min(100);
            while end > 0 && !request.prompt.is_char_boundary(end) {
                end -= 1;
            }
            let snippet = &request.prompt[..end];
            format!("Response to: {snippet}")
        };

        Ok(CompletionResponse {
            text,
            tokens_used: 10,
            prompt_tokens: 0,
            model_id: "deterministic".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "Streaming not supported in deterministic inference".to_string(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        // Return a zero vector — FTS5 text search will be the primary retrieval method
        Ok(vec![0.0; 8])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

// ─── AlwaysSearchInference (for cap testing) ────────────────

/// An inference provider that always emits tool calls in a ReasonWithTools
/// loop, except when forced to synthesize after hitting the cap.
pub struct AlwaysSearchInference;

#[async_trait]
impl InferenceProvider for AlwaysSearchInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let prompt_lower = request.prompt.to_lowercase();
        let text = if prompt_lower.contains("you have used all available searches") {
            "Forced synthesis after cap.".to_string()
        } else if prompt_lower.contains("how to search") {
            r#"Searching again. <tool_call>{"tool":"search","query":"more results"}</tool_call>"#
                .to_string()
        } else {
            "fallback".to_string()
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 5,
            prompt_tokens: 0,
            model_id: "always-search".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("not supported".to_string()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

// ─── Test Harness ────────────────────────────────────────────

pub struct TestHarness {
    pub runtime: Runtime,
    pub store: Arc<SqliteStateStore>,
    /// Concrete approval channel for collaborate harnesses. `None` for
    /// the default harness (which uses `AutoApprovalChannel`). Tests
    /// that need to inspect `emit_message_refined` calls reach into
    /// this field.
    pub scripted_approval: Option<Arc<ScriptedApprovalChannel>>,
}

impl TestHarness {
    /// Create a harness with DeterministicInference, real in-memory SQLite,
    /// PassthroughRouter (SimpleQuery for all), and no tools.
    pub fn new() -> Self {
        Self::with_skills(SkillRegistry::new())
    }

    /// Construct a harness with a caller-supplied `SkillRegistry`.
    /// Used by tests that exercise skill-gated code paths (privacy
    /// tagging, active-skill routing hints, etc.).
    pub fn with_skills(skills: SkillRegistry) -> Self {
        let inference: Arc<dyn InferenceProvider> = Arc::new(DeterministicInference);
        let shared_store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        let store_trait: Arc<dyn StateStore> = Arc::clone(&shared_store) as Arc<dyn StateStore>;

        let skills = Arc::new(skills);
        let router: Box<dyn sovereign_core::traits::Router> = Box::new(PassthroughRouter);
        let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
        let tools = Arc::new(ToolRegistry::new());
        let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> =
            Arc::new(AutoApprovalChannel);

        let runtime = Runtime::new(
            inference,
            router,
            Box::new(planner),
            tools,
            store_trait,
            skills,
            approval,
            sovereign_core::types::InferenceConfig::default(),
        );

        Self {
            runtime,
            store: shared_store,
            scripted_approval: None,
        }
    }

    /// Send a message and return the response. Uses a random conversation ID.
    pub async fn send(&self, message: &str) -> Response {
        let conv_id = uuid::Uuid::new_v4().to_string();
        self.send_in(message, &conv_id).await
    }

    /// Send a message in a specific conversation.
    pub async fn send_in(&self, message: &str, conversation_id: &str) -> Response {
        self.runtime
            .handle_message(message, conversation_id)
            .await
            .expect("handle_message should not fail")
    }

    /// Extract provenance from a response's metadata.
    pub fn provenance(&self, response: &Response) -> ResponseProvenance {
        let metadata = response
            .message
            .metadata
            .as_ref()
            .expect("Response should have metadata");
        let prov_value = metadata
            .get("provenance")
            .expect("Metadata should contain provenance");
        serde_json::from_value(prov_value.clone()).expect("Provenance should deserialize")
    }

    /// Get the number of messages in a conversation.
    pub async fn conversation_length(&self, conversation_id: &str) -> usize {
        match self.store.get_conversation(conversation_id).await {
            Ok(conv) => conv.messages.len(),
            Err(_) => 0,
        }
    }

    /// Build a harness with `auto_collaborate=true`, a scriptable
    /// inference provider (for gap/refinement responses) and a
    /// scripted approval channel (for the user's pasted content).
    /// The caller owns the scripts and can mutate them between sends.
    pub fn new_with_collaborate(
        gap: GapScript,
        refine: RefineScript,
        info_response: InfoResponseScript,
    ) -> Self {
        let scriptable = Arc::new(ScriptableInference::new(gap, refine));
        let inference: Arc<dyn InferenceProvider> = scriptable.clone();
        let shared_store = Arc::new(SqliteStateStore::open_in_memory().unwrap());
        let store_trait: Arc<dyn StateStore> = Arc::clone(&shared_store) as Arc<dyn StateStore>;

        let skills = Arc::new(SkillRegistry::new());
        let router: Box<dyn sovereign_core::traits::Router> = Box::new(PassthroughRouter);
        let planner = LlmPlanner::new(Arc::clone(&inference), Arc::clone(&skills));
        let tools = Arc::new(ToolRegistry::new());
        let scripted_approval = Arc::new(ScriptedApprovalChannel::new(info_response));
        let approval: Arc<dyn sovereign_core::traits::ApprovalChannel> =
            Arc::clone(&scripted_approval) as Arc<dyn sovereign_core::traits::ApprovalChannel>;

        let mut config = sovereign_core::types::InferenceConfig::default();
        config.auto_collaborate = true;

        let runtime = Runtime::new(
            inference,
            router,
            Box::new(planner),
            tools,
            store_trait,
            skills,
            approval,
            config,
        );

        Self {
            runtime,
            store: shared_store,
            scripted_approval: Some(scripted_approval),
        }
    }

    /// Ingest test content directly into the store as corpus chunks.
    /// Each (source, content) pair becomes a DocumentChunk with the given corpus_id.
    /// FTS5 triggers fire, so text search works immediately.
    pub async fn ingest_test_corpus(&self, corpus_id: &str, chunks: Vec<(&str, &str)>) {
        let doc_chunks: Vec<DocumentChunk> = chunks
            .iter()
            .enumerate()
            .map(|(i, (source, content))| DocumentChunk {
                id: format!("{corpus_id}:{source}:{i}"),
                source: source.to_string(),
                content: content.to_string(),
                chunk_index: i,
                embedding: None,
                created_at: 0,
                source_type: SourceType::Corpus {
                    corpus_id: corpus_id.to_string(),
                },
                version: 0,
                deleted_at: None,
            })
            .collect();

        self.store.store_chunks(&doc_chunks).await.unwrap();

        self.store
            .save_corpus_state(&CorpusState {
                corpus_id: corpus_id.to_string(),
                installed_at: 0,
                source_date: "test".to_string(),
                chunks_count: chunks.len() as i64,
                index_size_mb: 0,
                last_updated: 0,
                version: 0,
                deleted_at: None,
                vector_index_ready: false,
            })
            .await
            .unwrap();
    }
}

// ─── Auto-Collaborate Test Doubles ───────────────────────────
//
// Tiny scriptable test doubles for Phase 2 auto-collaboration tests.
// The tests configure these before invoking the runtime, then assert
// on the resulting assistant-message content.
//
// Not intended as a general-purpose mock — every field encodes a
// narrow test scenario. Keep the surface small.

/// What the scriptable inference should return for a gap-check prompt
/// (one whose text contains "single most valuable").
#[derive(Clone)]
pub enum GapScript {
    /// Return `{"has_gap": false}` — auto-collaborate should pass the
    /// original answer through unchanged.
    NoGap,
    /// Return a full gap JSON with the provided `gap` field. Uses
    /// stubbed values for the other fields.
    Gap { gap: String },
    /// Simulate a transient inference failure. The helper is
    /// documented to fall back to the original answer on error.
    Error,
}

/// What the scriptable inference should return for a refinement
/// prompt (one whose text contains "Refine the answer to integrate").
#[derive(Clone)]
pub enum RefineScript {
    /// Return this exact string as the refined answer.
    Text(String),
    /// Fail the refinement inference call (simulates Decode Error -3
    /// / context-overflow / network errors). Used to exercise the
    /// stuck-UI fallback path: `run_post_stream_refinement` must
    /// still emit `message-refined` (with the original content) so
    /// the desktop's `m.refining` flag clears.
    Error,
    /// Should not be called in this scenario — panics if invoked.
    Unused,
}

/// What the scripted approval channel should return for
/// `request_information`. Encodes the user's three choices:
/// paste content, skip, or (indirectly) ignore.
#[derive(Clone)]
pub enum InfoResponseScript {
    /// User pasted this exact text.
    Pasted(String),
    /// User pressed Skip — returns `None`.
    Skip,
    /// Should not be called in this scenario — panics if invoked.
    Unused,
}

pub struct ScriptableInference {
    fallback: DeterministicInference,
    gap: GapScript,
    refine: RefineScript,
}

impl ScriptableInference {
    pub fn new(gap: GapScript, refine: RefineScript) -> Self {
        Self {
            fallback: DeterministicInference,
            gap,
            refine,
        }
    }
}

#[async_trait]
impl InferenceProvider for ScriptableInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let p = &request.prompt;

        // Gap-check prompt? See gap.rs — the prompt always contains
        // "single most valuable".
        if p.contains("single most valuable") {
            return match &self.gap {
                GapScript::NoGap => Ok(CompletionResponse {
                    text: r#"{"has_gap": false}"#.to_string(),
                    tokens_used: 5,
                    prompt_tokens: 0,
                    model_id: "scriptable-gap".to_string(),
                    latency_ms: 1,
                    oicp_meta: None,
                    finish_reason: None,
                    completion_tokens: None,
                }),
                GapScript::Gap { gap } => {
                    let body = format!(
                        r#"{{"has_gap": true, "current_understanding": "cu", "gap": "{gap}", "relevance": "r", "satisfying_source": "s", "search_hints": ["h"]}}"#,
                        gap = gap.replace('"', "\\\""),
                    );
                    Ok(CompletionResponse {
                        text: body,
                        tokens_used: 20,
                        prompt_tokens: 0,
                        model_id: "scriptable-gap".to_string(),
                        latency_ms: 1,
                        oicp_meta: None,
                        finish_reason: None,
                        completion_tokens: None,
                    })
                }
                GapScript::Error => Err(Error::Inference("scripted gap-check failure".to_string())),
            };
        }

        // Refinement prompt? See runtime.rs::maybe_collaborate.
        if p.contains("Refine the answer to integrate") {
            return match &self.refine {
                RefineScript::Text(t) => Ok(CompletionResponse {
                    text: t.clone(),
                    tokens_used: 15,
                    prompt_tokens: 0,
                    model_id: "scriptable-refine".to_string(),
                    latency_ms: 1,
                    oicp_meta: None,
                    finish_reason: None,
                    completion_tokens: None,
                }),
                RefineScript::Error => {
                    Err(Error::Inference("scripted refinement failure".to_string()))
                }
                RefineScript::Unused => {
                    panic!("refinement invoked unexpectedly; test configured RefineScript::Unused")
                }
            };
        }

        // Everything else — defer to the deterministic baseline so the
        // surrounding pipeline (routing, memory, titles, synthesis)
        // behaves exactly as in the existing tests.
        self.fallback.complete(request).await
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented(
            "Streaming not supported in scriptable inference".to_string(),
        ))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0; 8])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Moderate,
        }
    }
}

pub struct ScriptedApprovalChannel {
    response: InfoResponseScript,
    /// Records every `emit_message_refined` call so tests can assert
    /// that post-stream refinement actually fired. Uses std::sync::Mutex
    /// since the trait method is synchronous.
    refined_emissions: std::sync::Mutex<Vec<MessageRefinedPayload>>,
}

impl ScriptedApprovalChannel {
    pub fn new(response: InfoResponseScript) -> Self {
        Self {
            response,
            refined_emissions: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Snapshot of `message-refined` events emitted so far. Used by
    /// streaming-path tests to verify both the content and the id of
    /// the refined message.
    pub fn refined_emissions(&self) -> Vec<MessageRefinedPayload> {
        self.refined_emissions.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalChannel for ScriptedApprovalChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }

    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }

    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}

    fn emit_message_refined(&self, payload: MessageRefinedPayload) {
        self.refined_emissions.lock().unwrap().push(payload);
    }

    async fn request_information(&self, _request: &InformationRequest) -> Option<String> {
        match &self.response {
            InfoResponseScript::Pasted(c) => Some(c.clone()),
            InfoResponseScript::Skip => None,
            InfoResponseScript::Unused => panic!(
                "request_information invoked unexpectedly; test configured InfoResponseScript::Unused"
            ),
        }
    }
}
