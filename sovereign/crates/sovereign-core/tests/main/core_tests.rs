// SPDX-License-Identifier: AGPL-3.0-or-later
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::context::{build_context, format_history_as_prompt};
use sovereign_core::error::{Error, Result};
use sovereign_core::executor::{AutoApprovalChannel, Executor, TaskContext};
use sovereign_core::planner::LlmPlanner;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::runtime::Runtime;
use sovereign_core::skills::*;
use sovereign_core::stubs::{NoOpPlanner, PassthroughRouter};
use sovereign_core::time::unix_now;
use sovereign_core::traits::*;
use sovereign_core::types::TrustLevel;
use sovereign_core::types::*;

// ─── Mock InferenceProvider ────────────────────────────────────

struct MockInference {
    response_text: String,
}

impl MockInference {
    fn new(text: &str) -> Self {
        Self {
            response_text: text.to_string(),
        }
    }
}

#[async_trait]
impl InferenceProvider for MockInference {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: self.response_text.clone(),
            tokens_used: 10,
            prompt_tokens: 0,
            model_id: "mock".to_string(),
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
        Err(Error::NotImplemented("mock".to_string()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("mock".to_string()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

// ─── Mock StateStore (in-process, no external deps) ───────────

struct MockStore {
    messages: tokio::sync::RwLock<Vec<Message>>,
}

impl MockStore {
    fn new() -> Self {
        Self {
            messages: tokio::sync::RwLock::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ConversationStore for MockStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        self.messages.write().await.push(msg.clone());
        Ok(())
    }
    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let msgs = self.messages.read().await;
        let conv_msgs: Vec<Message> = msgs
            .iter()
            .filter(|m| m.conversation_id == id)
            .cloned()
            .collect();
        if conv_msgs.is_empty() {
            return Err(Error::NotFound(format!("Conversation {id}")));
        }
        Ok(Conversation {
            id: id.to_string(),
            title: None,
            messages: conv_msgs,
            created_at: unix_now(),
            updated_at: unix_now(),
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        })
    }
    async fn list_conversations(&self, _limit: usize, _offset: usize) -> Result<Vec<Conversation>> {
        Ok(Vec::new())
    }
    async fn search_messages(&self, _query: &str) -> Result<Vec<Message>> {
        Ok(Vec::new())
    }
    /// MockStore is a message log: `get_conversation` reconstructs a
    /// conversation from the messages in it, so there is no row to seed
    /// and an empty conversation cannot be represented here at all.
    ///
    /// It therefore refuses by name rather than returning `Ok(())`. The
    /// trait's no-op default did the latter until 2026-08-25 and that is
    /// the defect this refusal exists to keep out: a test that seeds a
    /// conversation against this double is testing nothing, and should
    /// fail saying so instead of passing (ARCH §18.3).
    async fn insert_empty_conversation(
        &self,
        _id: &str,
        _created_at: i64,
        _surface_skill_id: Option<&str>,
    ) -> Result<()> {
        Err(Error::Storage(
            "MockStore holds messages only and cannot seed a conversation row".into(),
        ))
    }
    async fn delete_conversation(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn update_conversation_title(&self, _id: &str, _title: &str) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl TaskStore for MockStore {
    async fn save_task(&self, _task: &Task) -> Result<()> {
        Ok(())
    }
    async fn get_task(&self, _id: &str) -> Result<Task> {
        Err(Error::NotFound("task".to_string()))
    }
}

#[async_trait]
impl MemoryStore for MockStore {
    async fn save_memory(&self, _memory: &Memory) -> Result<()> {
        Ok(())
    }
    async fn get_relevant_memories(&self, _context: &str, _limit: usize) -> Result<Vec<Memory>> {
        Ok(Vec::new())
    }
    async fn get_all_memories(&self) -> Result<Vec<Memory>> {
        Ok(Vec::new())
    }
    async fn delete_memory(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn update_memory_confidence(&self, _id: &str, _confidence: f64) -> Result<()> {
        Ok(())
    }
    async fn touch_memory(&self, _id: &str, _timestamp: i64) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl RoutingStore for MockStore {
    async fn log_routing(&self, _hash: &str, _classified: &str, _latency: i64) -> Result<()> {
        Ok(())
    }
    async fn get_routing_corrections(&self, _limit: usize) -> Result<Vec<RoutingCorrection>> {
        Ok(Vec::new())
    }
    async fn mark_routing_correct(&self, _hash: &str, _correct: bool) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl DocumentStore for MockStore {
    async fn store_chunks(&self, _chunks: &[DocumentChunk]) -> Result<()> {
        Ok(())
    }
    async fn search_documents(
        &self,
        _qe: &[f32],
        _qt: &str,
        _l: usize,
    ) -> Result<Vec<DocumentChunk>> {
        Ok(Vec::new())
    }
    async fn get_chunks_by_source(&self, _source: &str) -> Result<Vec<DocumentChunk>> {
        Ok(Vec::new())
    }
    async fn delete_chunks_by_corpus(&self, _corpus_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn list_sources(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl CorpusStateStore for MockStore {
    async fn save_corpus_state(&self, _state: &CorpusState) -> Result<()> {
        Ok(())
    }
    async fn get_corpus_state(&self, _id: &str) -> Result<CorpusState> {
        Err(Error::NotFound("corpus".to_string()))
    }
    async fn list_corpus_states(&self) -> Result<Vec<CorpusState>> {
        Ok(Vec::new())
    }
    async fn delete_corpus_state(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn set_vector_index_ready(&self, _corpus_id: &str, _ready: bool) -> Result<()> {
        Ok(())
    }
    async fn get_vector_index_ready(&self, _corpus_id: &str) -> Result<bool> {
        Ok(false)
    }
}

#[async_trait]
impl BudgetStore for MockStore {
    async fn get_search_budget(&self, _backend: &str) -> Result<Option<SearchBudget>> {
        Ok(None)
    }
    async fn update_search_budget(&self, _budget: &SearchBudget) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl PermissionStore for MockStore {
    async fn get_permission(&self, _tool_id: &str, _scope: &str) -> Result<Option<bool>> {
        Ok(None)
    }
    async fn set_permission(&self, _tool_id: &str, _scope: &str, _granted: bool) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl HealthStore for MockStore {}

#[async_trait::async_trait]
impl sovereign_core::traits::DocumentSessionStore for MockStore {
    async fn create_document_session(
        &self,
        _session: &sovereign_core::DocumentSession,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn get_document_session(
        &self,
        _session_id: &str,
    ) -> sovereign_core::error::Result<Option<sovereign_core::DocumentSession>> {
        Ok(None)
    }
    async fn get_document_session_by_conversation(
        &self,
        _conversation_id: &str,
    ) -> sovereign_core::error::Result<Option<sovereign_core::DocumentSession>> {
        Ok(None)
    }
    async fn update_document_session(
        &self,
        _session: &sovereign_core::DocumentSession,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl sovereign_core::traits::DocumentAssetStore for MockStore {
    async fn save_document_asset(
        &self,
        _asset: &sovereign_core::DocumentAsset,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn update_asset_state(
        &self,
        _id: &str,
        _state: &sovereign_core::AssetState,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn save_asset_skeleton(
        &self,
        _id: &str,
        _skeleton: &sovereign_core::DocumentSkeleton,
        _document_type: &sovereign_core::types::DocumentTypeTag,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn get_document_asset(
        &self,
        _id: &str,
    ) -> sovereign_core::error::Result<Option<sovereign_core::DocumentAsset>> {
        Ok(None)
    }
    async fn list_document_assets(
        &self,
    ) -> sovereign_core::error::Result<Vec<sovereign_core::DocumentAsset>> {
        Ok(Vec::new())
    }
    async fn delete_document_asset(&self, _id: &str) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn save_document_operation(
        &self,
        _message_id: &str,
        _asset_id: &str,
        _operation: &sovereign_core::DocumentAssetOperation,
        _duration_ms: u64,
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn save_raptor_nodes(
        &self,
        _asset_id: &str,
        _nodes: &[sovereign_core::types::RaptorNode],
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn list_raptor_nodes(
        &self,
        _asset_id: &str,
    ) -> sovereign_core::error::Result<Vec<sovereign_core::types::RaptorNode>> {
        Ok(Vec::new())
    }
    async fn get_raptor_node(
        &self,
        _node_id: &str,
    ) -> sovereign_core::error::Result<Option<sovereign_core::types::RaptorNode>> {
        Ok(None)
    }
    async fn save_asset_motifs(
        &self,
        _asset_id: &str,
        _motifs: &[sovereign_core::types::AssetMotif],
    ) -> sovereign_core::error::Result<()> {
        Ok(())
    }
    async fn list_asset_motifs(
        &self,
        _asset_id: &str,
    ) -> sovereign_core::error::Result<Vec<sovereign_core::types::AssetMotif>> {
        Ok(Vec::new())
    }
}

impl StepExecutionStore for MockStore {}

impl StateStore for MockStore {}

// ─── ToolRegistry Tests ────────────────────────────────────────

struct DummyTool {
    id: String,
}

#[async_trait]
impl Tool for DummyTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: self.id.clone(),
            name: self.id.clone(),
            description: "test tool".to_string(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: sovereign_core::types::Effect::Read,
            idempotency: sovereign_core::types::Idempotency::Idempotent,
            latency: sovereign_core::types::Latency::Instant,
            scope: sovereign_core::types::Scope::Session,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }
    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        Ok(StepOutput::Text("ok".to_string()))
    }
}

#[test]
fn tool_registry_empty() {
    let reg = ToolRegistry::new();
    assert_eq!(reg.count(), 0);
    assert!(reg.descriptors().is_empty());
    assert!(reg.get("missing").is_err());
}

#[test]
fn tool_registry_register_and_get() {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(DummyTool {
        id: "tool_a".to_string(),
    }));
    reg.register(Box::new(DummyTool {
        id: "tool_b".to_string(),
    }));

    assert_eq!(reg.count(), 2);
    assert_eq!(reg.descriptors().len(), 2);
    assert!(reg.get("tool_a").is_ok());
    assert!(reg.get("tool_b").is_ok());
    assert!(reg.get("tool_c").is_err());
}

// ─── SkillRegistry Tests ──────────────────────────────────────

fn make_skill(id: &str, _trigger: &str, synthesis: Option<&str>) -> Skill {
    // `_trigger` retained as a positional arg for call-site
    // compatibility but no longer threads into the Skill struct —
    // routing was retired alongside the trigger-phrase splice.
    Skill {
        id: id.to_string(),
        name: id.to_string(),
        version: "0.1.0".to_string(),
        description: String::new(),
        activation_kind: ActivationKind::default(),
        planner_templates: vec![PlanTemplate {
            name: "test".to_string(),
            trigger: "test trigger".to_string(),
            steps: "1. Do the thing".to_string(),
        }],
        tool_config: ToolPreferences::default(),
        prompts: PromptOverrides {
            synthesis: synthesis.map(|s| s.to_string()),
        },
        memory_rules: MemoryConfig {
            extract_prompt_addendum: Some(format!("Rules for {id}")),
            confidence_decay_per_month: None,
            prune_threshold: None,
        },
        inference: SkillInferenceConfig::default(),
        signature: None,
        signed_by: None,
        trust_level: TrustLevel::Unsigned,
    }
}

#[test]
fn skill_registry_empty() {
    let reg = SkillRegistry::new();
    assert!(reg.list().is_empty());
    assert!(reg.active_skills().is_empty());
    assert!(reg.prompt_overrides(&Intent::SimpleQuery).is_none());
    assert!(reg.memory_rules().extraction_addenda.is_empty());
}

#[test]
fn skill_registry_activate_deactivate() {
    let mut reg = SkillRegistry::new();
    reg.register(make_skill("research", "investigate", Some("Be thorough")));
    reg.register(make_skill("coding", "refactor", Some("Write clean code")));

    assert_eq!(reg.list().len(), 2);
    assert!(reg.active_skills().is_empty());

    reg.activate("research");
    assert_eq!(reg.active_skills().len(), 1);

    reg.activate("coding");
    assert_eq!(reg.active_skills().len(), 2);

    reg.deactivate("research");
    assert_eq!(reg.active_skills().len(), 1);
    assert_eq!(reg.active_skills()[0].id, "coding");
}

// `skill_registry_merge_routing_hints` retired alongside
// `routing_hints()` itself — see skills.rs for the migration note.

#[test]
fn skill_registry_merge_prompts() {
    let mut reg = SkillRegistry::new();
    reg.register(make_skill("a", "x", Some("Prompt A")));
    reg.register(make_skill("b", "y", Some("Prompt B")));
    reg.activate("a");
    reg.activate("b");

    let overrides = reg.prompt_overrides(&Intent::SimpleQuery).unwrap();
    assert!(overrides.contains("Prompt A"));
    assert!(overrides.contains("Prompt B"));
    assert!(overrides.contains("---"));
}

#[test]
fn skill_registry_merge_memory_rules() {
    let mut reg = SkillRegistry::new();
    reg.register(make_skill("a", "x", None));
    reg.register(make_skill("b", "y", None));
    reg.activate("a");
    reg.activate("b");

    let rules = reg.memory_rules();
    assert_eq!(rules.extraction_addenda.len(), 2);
}

// ─── Context Tests ─────────────────────────────────────────────

#[tokio::test]
async fn build_context_new_conversation() {
    let store = MockStore::new();
    let ctx = build_context(&store, "new-convo", "", None).await.unwrap();
    assert_eq!(ctx.conversation.id, "new-convo");
    assert!(ctx.conversation.messages.is_empty());
    assert!(ctx.memories.is_empty());
    assert!(ctx.working_memory.is_none());
}

#[tokio::test]
async fn build_context_existing_conversation() {
    let store = MockStore::new();
    store
        .save_message(&Message {
            id: "m1".to_string(),
            conversation_id: "c1".to_string(),
            role: Role::User,
            content: "hello".to_string(),
            created_at: unix_now(),
            metadata: None,
            version: 0,
        })
        .await
        .unwrap();

    let ctx = build_context(&store, "c1", "hello", None).await.unwrap();
    assert_eq!(ctx.conversation.messages.len(), 1);
    assert_eq!(ctx.conversation.messages[0].content, "hello");
}

#[test]
fn format_history_empty() {
    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages: Vec::new(),
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };
    assert_eq!(format_history_as_prompt(&ctx, 10), "");
}

#[test]
fn format_history_multi_turn() {
    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages: vec![
                Message {
                    id: "1".to_string(),
                    conversation_id: "c1".to_string(),
                    role: Role::User,
                    content: "Hi".to_string(),
                    created_at: 1,
                    metadata: None,
                    version: 0,
                },
                Message {
                    id: "2".to_string(),
                    conversation_id: "c1".to_string(),
                    role: Role::Assistant,
                    content: "Hello!".to_string(),
                    created_at: 2,
                    metadata: None,
                    version: 0,
                },
            ],
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    let prompt = format_history_as_prompt(&ctx, 10);
    assert!(prompt.contains("User: Hi"));
    assert!(prompt.contains("Assistant: Hello!"));
}

#[test]
fn format_history_truncates_to_max() {
    let messages: Vec<Message> = (0..20)
        .map(|i| Message {
            id: format!("m{i}"),
            conversation_id: "c1".to_string(),
            role: Role::User,
            content: format!("message {i}"),
            created_at: i,
            metadata: None,
            version: 0,
        })
        .collect();

    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages,
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    let prompt = format_history_as_prompt(&ctx, 3);
    assert!(!prompt.contains("message 16"));
    assert!(prompt.contains("message 17"));
    assert!(prompt.contains("message 18"));
    assert!(prompt.contains("message 19"));
}

// ─── Stubs Tests ───────────────────────────────────────────────

#[tokio::test]
async fn passthrough_router_always_simple_query() {
    let router = PassthroughRouter;
    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages: Vec::new(),
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    let outcome = router.classify("anything", &ctx, &[]).await.unwrap();
    assert!(matches!(outcome.primary.intent, Intent::SimpleQuery));
}

#[tokio::test]
async fn noop_planner_returns_not_implemented() {
    let planner = NoOpPlanner;
    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages: Vec::new(),
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    let result = planner.plan("do something", &ctx, &[]).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotImplemented(_)));
}

// ─── Runtime Integration Tests ─────────────────────────────────

fn build_runtime(response: &str) -> (Runtime, Arc<MockStore>) {
    let store = Arc::new(MockStore::new());
    let runtime = Runtime::new(sovereign_core::RuntimeParts::new(
        Arc::new(MockInference::new(response)),
        Box::new(PassthroughRouter),
        Box::new(NoOpPlanner),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        Arc::new(SkillRegistry::new()),
        Arc::new(AutoApprovalChannel),
        sovereign_core::types::InferenceConfig::default(),
        // Phase 4b: enrichment is a required argument, not eight
        // forgettable builders.
        sovereign_core::runtime::lane::LaneSources::none(),
    ));
    (runtime, store)
}

#[tokio::test]
async fn runtime_single_message() {
    let (runtime, store) = build_runtime("I am an assistant.");
    let response = runtime.handle_message("Hello", "c1").await.unwrap();

    assert_eq!(response.message.role, Role::Assistant);
    assert_eq!(response.message.content, "I am an assistant.");
    assert!(response.task.is_none());

    // Both user and assistant messages were saved.
    let msgs = store.messages.read().await;
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content, "Hello");
    assert_eq!(msgs[1].role, Role::Assistant);
}

#[tokio::test]
async fn runtime_multi_turn() {
    let (runtime, store) = build_runtime("response");

    runtime.handle_message("first", "c1").await.unwrap();
    runtime.handle_message("second", "c1").await.unwrap();
    runtime.handle_message("third", "c1").await.unwrap();

    let msgs = store.messages.read().await;
    // 3 user + 3 assistant = 6 messages
    assert_eq!(msgs.len(), 6);
    assert_eq!(msgs[0].content, "first");
    assert_eq!(msgs[2].content, "second");
    assert_eq!(msgs[4].content, "third");
}

#[tokio::test]
async fn runtime_message_metadata() {
    let (runtime, store) = build_runtime("ok");
    runtime.handle_message("test", "c1").await.unwrap();

    let msgs = store.messages.read().await;
    let assistant_msg = &msgs[1];
    let metadata = assistant_msg.metadata.as_ref().unwrap();
    assert_eq!(metadata["model"], "mock");
    assert_eq!(metadata["tokens"], 10);
    assert_eq!(metadata["latency_ms"], 1);
}

#[tokio::test]
async fn runtime_separate_conversations() {
    let (runtime, store) = build_runtime("reply");

    runtime.handle_message("msg1", "convo-a").await.unwrap();
    runtime.handle_message("msg2", "convo-b").await.unwrap();

    let msgs = store.messages.read().await;
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].conversation_id, "convo-a");
    assert_eq!(msgs[2].conversation_id, "convo-b");
}

// ─── Sequenced Mock (returns different responses per call) ─────

struct SequencedMockInference {
    responses: tokio::sync::Mutex<Vec<String>>,
    default: String,
}

impl SequencedMockInference {
    fn new(responses: Vec<&str>, default: &str) -> Self {
        Self {
            responses: tokio::sync::Mutex::new(
                responses.into_iter().map(|s| s.to_string()).collect(),
            ),
            default: default.to_string(),
        }
    }
}

#[async_trait]
impl InferenceProvider for SequencedMockInference {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        let mut queue = self.responses.lock().await;
        let text = if queue.is_empty() {
            self.default.clone()
        } else {
            queue.remove(0)
        };
        Ok(CompletionResponse {
            text,
            tokens_used: 10,
            prompt_tokens: 0,
            model_id: "mock-seq".to_string(),
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
        Err(Error::NotImplemented("mock".to_string()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("mock".to_string()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

// ─── Executor Integration Tests ────────────────────────────────

#[tokio::test]
async fn executor_linear_plan() {
    // Two-step linear plan: step 0 → step 1 (step 1 uses step 0's output).
    let inference = Arc::new(SequencedMockInference::new(
        vec!["Python is versatile", "Based on that, learn Python first"],
        "fallback",
    ));
    let store = Arc::new(MockStore::new());

    let plan = Plan {
        id: "t1".to_string(),
        goal: "recommend a language".to_string(),
        steps: vec![
            Step {
                id: 0,
                description: "Analyze Python".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "List Python strengths".to_string(),
                    speed: Speed::Slow,
                },
                requires_approval: false,
                inputs: vec![],
                sampling: None,
                evaluation: None,
            },
            Step {
                id: 1,
                description: "Recommend".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "Given: {0.output}. Recommend a language.".to_string(),
                    speed: Speed::Slow,
                },
                requires_approval: false,
                inputs: vec![StepInput {
                    step_id: 0,
                    key: "output".to_string(),
                }],
                sampling: None,
                evaluation: None,
            },
        ],
        edges: vec![(0, 1)],
    };

    let task = Task {
        id: "t1".to_string(),
        conversation_id: "c1".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();

    assert!(result.error.is_none());
    assert_eq!(result.completed.len(), 2);

    // Step 0 ran first.
    assert!(
        matches!(result.completed.get(&0), Some(StepOutput::Text(t)) if t == "Python is versatile")
    );
    // Step 1 used step 0's output.
    assert!(
        matches!(result.completed.get(&1), Some(StepOutput::Text(t)) if t.contains("learn Python"))
    );
}

#[tokio::test]
async fn executor_parallel_then_merge() {
    // Steps 0 and 1 are independent, step 2 depends on both.
    let inference = Arc::new(SequencedMockInference::new(
        vec!["Python pros", "Rust pros", "Comparison result"],
        "fallback",
    ));
    let store = Arc::new(MockStore::new());

    let plan = Plan {
        id: "t1".to_string(),
        goal: "compare".to_string(),
        steps: vec![
            Step {
                id: 0,
                description: "Python".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "List Python pros".to_string(),
                    speed: Speed::Fast,
                },
                requires_approval: false,
                inputs: vec![],
                sampling: None,
                evaluation: None,
            },
            Step {
                id: 1,
                description: "Rust".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "List Rust pros".to_string(),
                    speed: Speed::Fast,
                },
                requires_approval: false,
                inputs: vec![],
                sampling: None,
                evaluation: None,
            },
            Step {
                id: 2,
                description: "Compare".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "Compare {0.output} and {1.output}".to_string(),
                    speed: Speed::Slow,
                },
                requires_approval: false,
                inputs: vec![
                    StepInput {
                        step_id: 0,
                        key: "output".to_string(),
                    },
                    StepInput {
                        step_id: 1,
                        key: "output".to_string(),
                    },
                ],
                sampling: None,
                evaluation: None,
            },
        ],
        edges: vec![(0, 2), (1, 2)],
    };

    let task = Task {
        id: "t1".to_string(),
        conversation_id: "c1".to_string(),
        goal: "compare".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );
    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());
    assert_eq!(result.completed.len(), 3);
}

#[tokio::test]
async fn executor_branch_skips_non_taken_path() {
    let inference = Arc::new(SequencedMockInference::new(
        vec![
            "yes", // Branch evaluation → takes true path
            "True path result",
        ],
        "fallback",
    ));
    let store = Arc::new(MockStore::new());

    let plan = Plan {
        id: "t1".to_string(),
        goal: "test branch".to_string(),
        steps: vec![
            Step {
                id: 0,
                description: "Check condition".to_string(),
                kind: StepKind::Branch {
                    condition: "Is it sunny?".to_string(),
                    if_true: 1,
                    if_false: 2,
                },
                requires_approval: false,
                inputs: vec![],
                sampling: None,
                evaluation: None,
            },
            Step {
                id: 1,
                description: "Sunny path".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "Plan for sunny day".to_string(),
                    speed: Speed::Fast,
                },
                requires_approval: false,
                inputs: vec![],
                sampling: None,
                evaluation: None,
            },
            Step {
                id: 2,
                description: "Rainy path".to_string(),
                kind: StepKind::Reason {
                    prompt_template: "Plan for rainy day".to_string(),
                    speed: Speed::Fast,
                },
                requires_approval: false,
                inputs: vec![],
                sampling: None,
                evaluation: None,
            },
        ],
        edges: vec![(0, 1), (0, 2)],
    };

    let task = Task {
        id: "t1".to_string(),
        conversation_id: "c1".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );
    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());

    // Branch jumped to step 1.
    assert!(matches!(
        result.completed.get(&0),
        Some(StepOutput::Jump(1))
    ));
    // Step 1 (sunny) executed.
    assert!(matches!(
        result.completed.get(&1),
        Some(StepOutput::Text(_))
    ));
    // Step 2 (rainy) was skipped.
    assert!(matches!(
        result.completed.get(&2),
        Some(StepOutput::Skipped)
    ));
}

// ─── Planner + Executor Integration ────────────────────────────

#[tokio::test]
async fn planner_generates_valid_plan() {
    let plan_json = r#"{"goal": "compare languages", "steps": [{"id": 0, "description": "Analyze", "kind": "reason", "prompt": "Compare Python and Rust", "speed": "slow"}], "edges": []}"#;

    let inference = Arc::new(SequencedMockInference::new(
        vec![plan_json, "Python is great, Rust is fast"],
        "fallback",
    ));

    let planner = LlmPlanner::new(inference, Arc::new(SkillRegistry::new()));
    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages: vec![],
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: vec![],
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    let plan = planner.plan("compare languages", &ctx, &[]).await.unwrap();
    assert!(!plan.steps.is_empty());
    assert_eq!(plan.goal, "compare languages");
}

#[tokio::test]
async fn planner_fallback_on_garbage() {
    // Mock returns garbage that can't be parsed as JSON.
    let inference = Arc::new(SequencedMockInference::new(
        vec!["not json at all", "still not json", "nope"],
        "nope",
    ));

    let planner = LlmPlanner::new(inference, Arc::new(SkillRegistry::new()));
    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages: vec![],
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        },
        memories: vec![],
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    // Should succeed with fallback plan (single step).
    let plan = planner.plan("do something", &ctx, &[]).await.unwrap();
    assert_eq!(plan.steps.len(), 1);
}

// ─── ComplexTask Runtime Integration ───────────────────────────

struct ComplexTaskRouter;

#[async_trait]
impl Router for ComplexTaskRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<RouterClassification> {
        Ok(RouterClassification {
            primary: IntentCandidate {
                intent: Intent::ComplexTask,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: None,
            coarse_intent: None,
            self_assessment: None,
            timing: None,
            scope: None,
        })
    }
}

#[tokio::test]
async fn runtime_complex_task_end_to_end() {
    let plan_json = r#"{"goal": "compare", "steps": [{"id": 0, "description": "Think", "kind": "reason", "prompt": "Analyze the question", "speed": "slow"}], "edges": []}"#;

    // Responses: routing classification (unused for ComplexTaskRouter), plan JSON, step execution, synthesis
    let inference = Arc::new(SequencedMockInference::new(
        vec![
            plan_json,
            "Step result: analysis done",
            "Final synthesized answer",
        ],
        "default response",
    ));

    let store = Arc::new(MockStore::new());
    let skills = Arc::new(SkillRegistry::new());
    let runtime = Runtime::new(sovereign_core::RuntimeParts::new(
        inference,
        Box::new(ComplexTaskRouter),
        Box::new(LlmPlanner::new(
            Arc::new(MockInference::new(plan_json)),
            Arc::clone(&skills),
        )),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        skills,
        Arc::new(AutoApprovalChannel),
        sovereign_core::types::InferenceConfig::default(),
        // Phase 4b: enrichment is a required argument, not eight
        // forgettable builders.
        sovereign_core::runtime::lane::LaneSources::none(),
    ));

    let response = runtime
        .handle_message("compare Python and Rust", "c1")
        .await
        .unwrap();

    // Should have a task attached.
    assert!(response.task.is_some());
    let task = response.task.unwrap();
    assert!(matches!(task.status, TaskStatus::Completed));
    assert!(!task.completed_steps.is_empty());

    // I2-A invariant (I1): the complex-task surface persists the epistemic
    // ledger on the assistant message when the flag is on (default). Before
    // I2-A this surface discarded its gate claims and wrote no ledger.
    let meta = response
        .message
        .metadata
        .as_ref()
        .expect("assistant message should carry metadata");
    let ledger = meta
        .get("epistemic_state")
        .expect("complex-task must persist epistemic_state");
    assert!(
        ledger.get("verdict").is_some(),
        "epistemic_state must carry a derived verdict: {ledger}"
    );
    assert_eq!(
        ledger.get("version").and_then(|v| v.as_u64()),
        Some(1),
        "ledger schema version stamped"
    );
}

// ─── Executor Tool Step Tests ──────────────────────────────────

#[tokio::test]
async fn executor_tool_step() {
    let inference = Arc::new(MockInference::new("response"));
    let store = Arc::new(MockStore::new());

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(DummyTool {
        id: "test_tool".to_string(),
    }));

    let plan = Plan {
        id: "t1".to_string(),
        goal: "test tool".to_string(),
        steps: vec![Step {
            id: 0,
            description: "Run test tool".to_string(),
            kind: StepKind::Tool {
                tool_id: "test_tool".to_string(),
                params: serde_json::json!({"key": "value"}),
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };

    let task = Task {
        id: "t1".to_string(),
        conversation_id: "c1".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };

    let executor = Executor::new(
        inference,
        Arc::new(tools),
        store,
        Arc::new(AutoApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());
    assert_eq!(result.completed.len(), 1);
    assert!(matches!(
        result.completed.get(&0),
        Some(StepOutput::Text(t)) if t == "ok"
    ));
}

// ─── Permission Denial Test ────────────────────────────────────

struct DenyApprovalChannel;

#[async_trait]
impl ApprovalChannel for DenyApprovalChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(false)
    }
    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok("denied".to_string())
    }
    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
}

struct PermissionRequiringTool;

#[async_trait]
impl Tool for PermissionRequiringTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "restricted".to_string(),
            name: "Restricted".to_string(),
            description: "needs permission".to_string(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: sovereign_core::types::Effect::ReadWrite,
            idempotency: sovereign_core::types::Idempotency::NonIdempotent,
            latency: sovereign_core::types::Latency::Slow,
            scope: sovereign_core::types::Scope::Session,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Shell]
    }
    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        Ok(StepOutput::Text("executed".to_string()))
    }
}

#[tokio::test]
async fn executor_tool_denied_permission_skips() {
    let inference = Arc::new(MockInference::new("response"));
    let store = Arc::new(MockStore::new());

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(PermissionRequiringTool));

    let plan = Plan {
        id: "t1".to_string(),
        goal: "test denied".to_string(),
        steps: vec![Step {
            id: 0,
            description: "Run restricted tool".to_string(),
            kind: StepKind::Tool {
                tool_id: "restricted".to_string(),
                params: serde_json::json!({}),
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };

    let task = Task {
        id: "t1".to_string(),
        conversation_id: "c1".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };

    let executor = Executor::new(
        inference,
        Arc::new(tools),
        store,
        Arc::new(DenyApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());
    // Step was skipped because permission was denied.
    assert!(matches!(
        result.completed.get(&0),
        Some(StepOutput::Skipped)
    ));
}

// ─── UserInput Step Test ───────────────────────────────────────

struct AnsweringApprovalChannel;

#[async_trait]
impl ApprovalChannel for AnsweringApprovalChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }
    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok("42".to_string())
    }
    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
}

#[tokio::test]
async fn executor_user_input_step() {
    let inference = Arc::new(MockInference::new("response"));
    let store = Arc::new(MockStore::new());

    let plan = Plan {
        id: "t1".to_string(),
        goal: "ask user".to_string(),
        steps: vec![Step {
            id: 0,
            description: "Ask for a number".to_string(),
            kind: StepKind::UserInput {
                question: "What is the answer?".to_string(),
            },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };

    let task = Task {
        id: "t1".to_string(),
        conversation_id: "c1".to_string(),
        goal: "test".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        Arc::new(AnsweringApprovalChannel),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());
    assert!(matches!(
        result.completed.get(&0),
        Some(StepOutput::Text(t)) if t == "42"
    ));
}

// ─── AwaitUserInfo Step Tests ──────────────────────────────────

/// Approval channel that returns a canned `request_information` response.
/// Used to drive the AwaitUserInfo step deterministically.
struct InfoApprovalChannel {
    canned: Option<String>,
}

#[async_trait]
impl ApprovalChannel for InfoApprovalChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }
    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }
    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
    async fn request_information(
        &self,
        _request: &sovereign_core::types::InformationRequest,
    ) -> Option<String> {
        self.canned.clone()
    }
}

fn await_user_info_plan() -> (Plan, Task) {
    let request = sovereign_core::types::InformationRequest {
        current_understanding: "agent thinks X".to_string(),
        gap: "verify Y".to_string(),
        relevance: "Y decides the answer".to_string(),
        satisfying_source: "a 2024 paper".to_string(),
        search_hints: vec!["NEJM 2024".to_string()],
        task_id: String::new(),
        step_id: 0,
        kind: sovereign_core::types::InformationRequestKind::default(),
        task_title: String::new(),
        routes: Vec::new(),
    };
    let plan = Plan {
        id: "info-test".to_string(),
        goal: "collaborate".to_string(),
        steps: vec![Step {
            id: 0,
            description: "Surface info request".to_string(),
            kind: StepKind::AwaitUserInfo { request },
            requires_approval: false,
            inputs: vec![],
            sampling: None,
            evaluation: None,
        }],
        edges: vec![],
    };
    let task = Task {
        id: "info-task".to_string(),
        conversation_id: "c1".to_string(),
        goal: "collaborate".to_string(),
        plan: plan.clone(),
        status: TaskStatus::Running,
        completed_steps: Vec::new(),
        created_at: unix_now(),
        updated_at: unix_now(),
        version: 0,
    };
    (plan, task)
}

#[tokio::test]
async fn await_user_info_yields_user_content_on_fulfill() {
    let inference = Arc::new(MockInference::new("unused"));
    let store = Arc::new(MockStore::new());
    let (plan, task) = await_user_info_plan();

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        Arc::new(InfoApprovalChannel {
            canned: Some("here is a relevant paragraph from a 2024 paper".to_string()),
        }),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());
    match result.completed.get(&0) {
        Some(StepOutput::Text(t)) => {
            assert!(t.contains("2024 paper"));
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

/// Spy channel that captures the `InformationRequest` payload the
/// executor hands to `request_information`. Used to pin the executor's
/// stamping contract for `AwaitUserInfo` steps.
struct SpyInfoChannel {
    captured: tokio::sync::Mutex<Option<sovereign_core::types::InformationRequest>>,
}

#[async_trait]
impl ApprovalChannel for SpyInfoChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }
    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }
    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
    async fn request_information(
        &self,
        request: &sovereign_core::types::InformationRequest,
    ) -> Option<String> {
        *self.captured.lock().await = Some(request.clone());
        None
    }
}

#[tokio::test]
async fn await_user_info_stamps_step_block_kind_and_task_title() {
    // Pins the executor's contract that every `AwaitUserInfo` step
    // surfaces to the UI as a `StepBlock` card with `task_title`
    // populated from the task goal. The UI uses this to render the
    // "task paused" chrome distinct from the post-answer refinement
    // card produced by `run_collaboration` on abstained turns.
    let inference = Arc::new(MockInference::new("unused"));
    let store = Arc::new(MockStore::new());
    let (plan, task) = await_user_info_plan();
    let task_goal = task.goal.clone();
    let spy = Arc::new(SpyInfoChannel {
        captured: tokio::sync::Mutex::new(None),
    });

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        spy.clone(),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };
    let _ = executor.run(&plan, &mut ctx).await.unwrap();

    let captured = spy
        .captured
        .lock()
        .await
        .clone()
        .expect("executor must call request_information");
    assert_eq!(
        captured.kind,
        sovereign_core::types::InformationRequestKind::StepBlock,
        "AwaitUserInfo steps must be stamped StepBlock so the UI picks the task-paused chrome"
    );
    assert_eq!(
        captured.task_title, task_goal,
        "executor must copy task.goal into task_title so the card can show 'Task: <goal>'"
    );
    assert_eq!(captured.task_id, "info-task");
    assert_eq!(captured.step_id, 0);
}

#[tokio::test]
async fn await_user_info_yields_empty_on_skip() {
    let inference = Arc::new(MockInference::new("unused"));
    let store = Arc::new(MockStore::new());
    let (plan, task) = await_user_info_plan();

    let executor = Executor::new(
        inference,
        Arc::new(ToolRegistry::new()),
        store,
        Arc::new(InfoApprovalChannel { canned: None }),
        Arc::new(SkillRegistry::new()),
    );

    let mut ctx = TaskContext {
        task,
        completed: std::collections::HashMap::new(),
    };

    let result = executor.run(&plan, &mut ctx).await.unwrap();
    assert!(result.error.is_none());
    match result.completed.get(&0) {
        Some(StepOutput::Text(t)) => assert_eq!(t, ""),
        other => panic!("expected empty Text, got {other:?}"),
    }
}

// ─── Cutoff legibility E2E (cutoff-chip plumbing) ────────────────
//
// Pins the wire: when a non-streaming inference call returns
// `CompletionResponse { finish_reason: Some(Length), completion_tokens
// : Some(N), .. }`, the desktop chat surface sees
// `provenance.finish_reason == "length"` + `completion_tokens == N`
// on the persisted message metadata. The desktop cutoff chip in
// `AssistantMessage.svelte` reads exactly that — these tests are the
// guard that the wire stays intact under future refactors of
// `simple.rs::handle_message` / `ResponseProvenance` serialization.

struct TruncatingMockInference {
    response_text: String,
    finish_reason: Option<FinishReason>,
    completion_tokens: Option<u32>,
}

#[async_trait]
impl InferenceProvider for TruncatingMockInference {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: self.response_text.clone(),
            tokens_used: self.completion_tokens.map(|c| c as usize).unwrap_or(10),
            prompt_tokens: 0,
            model_id: "truncating-mock".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: self.finish_reason.clone(),
            completion_tokens: self.completion_tokens,
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        Err(Error::NotImplemented("mock".to_string()))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("mock".to_string()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

fn build_runtime_with_finish(
    response: &str,
    finish_reason: Option<FinishReason>,
    completion_tokens: Option<u32>,
) -> (Runtime, Arc<MockStore>) {
    let store = Arc::new(MockStore::new());
    let runtime = Runtime::new(sovereign_core::RuntimeParts::new(
        Arc::new(TruncatingMockInference {
            response_text: response.to_string(),
            finish_reason,
            completion_tokens,
        }),
        Box::new(PassthroughRouter),
        Box::new(NoOpPlanner),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        Arc::new(SkillRegistry::new()),
        Arc::new(AutoApprovalChannel),
        sovereign_core::types::InferenceConfig::default(),
        // Phase 4b: enrichment is a required argument, not eight
        // forgettable builders.
        sovereign_core::runtime::lane::LaneSources::none(),
    ));
    (runtime, store)
}

/// Length truncation flows from `CompletionResponse.finish_reason`
/// through the simple handler into `metadata.provenance.finish_reason`
/// as the OpenAI-compatible lowercase string the frontend reads.
#[tokio::test]
async fn cutoff_length_finish_reason_surfaces_in_provenance() {
    let (runtime, store) = build_runtime_with_finish(
        "...cut off mid-sentenc",
        Some(FinishReason::Length),
        Some(2047),
    );
    runtime
        .handle_message("Tell me everything", "c1")
        .await
        .unwrap();

    let msgs = store.messages.read().await;
    let assistant_msg = &msgs[1];
    let metadata = assistant_msg.metadata.as_ref().expect("metadata present");
    let provenance = &metadata["provenance"];
    assert_eq!(
        provenance["finish_reason"],
        serde_json::Value::String("length".to_string()),
        "Length truncation must surface as lowercase 'length' for the cutoff chip"
    );
    assert_eq!(
        provenance["completion_tokens"],
        serde_json::Value::from(2047),
        "completion_tokens from provider must reach provenance"
    );
    // max_tokens_budget comes from inference_config.max_tokens (default 2048).
    assert_eq!(
        provenance["max_tokens_budget"],
        serde_json::Value::from(2048),
        "max_tokens_budget must reflect the runtime's configured cap"
    );
}

/// Sibling test: clean Stop does NOT light up the chip. Negative
/// control — without this, a regression that hardcodes Length would
/// pass the truncation test while breaking the chip's specificity.
#[tokio::test]
async fn cutoff_clean_stop_does_not_signal_length() {
    let (runtime, store) =
        build_runtime_with_finish("complete answer.", Some(FinishReason::Stop), Some(50));
    runtime
        .handle_message("Short question?", "c1")
        .await
        .unwrap();

    let msgs = store.messages.read().await;
    let metadata = msgs[1].metadata.as_ref().unwrap();
    let provenance = &metadata["provenance"];
    assert_eq!(
        provenance["finish_reason"],
        serde_json::Value::String("stop".to_string()),
        "Stop reason must surface as 'stop' so the chip doesn't render on clean exits"
    );
    assert_ne!(
        provenance["finish_reason"],
        serde_json::Value::String("length".to_string()),
    );
}

/// Provider that doesn't track finish_reason (older test stubs,
/// remote APIs that don't expose it) must round-trip as
/// `null`/absent. The chip's `if cutoffInfo` guard reads this as
/// "don't render" — same outcome as `Stop`, but the metadata stays
/// honest about not knowing rather than synthesising a value.
#[tokio::test]
async fn cutoff_missing_finish_reason_serializes_absent() {
    let (runtime, store) = build_runtime_with_finish("best-effort answer", None, None);
    runtime.handle_message("question", "c1").await.unwrap();

    let msgs = store.messages.read().await;
    let metadata = msgs[1].metadata.as_ref().unwrap();
    let provenance = &metadata["provenance"];
    // `#[serde(skip_serializing_if = "Option::is_none")]` on the
    // ResponseProvenance field omits the key entirely when None.
    assert!(
        provenance.get("finish_reason").is_none()
            || provenance["finish_reason"] == serde_json::Value::Null,
        "finish_reason must be absent when provider didn't supply one, got: {provenance:?}"
    );
}

// ─── TEACHABLE lesson capture (conation handler) ───────────────────
//
// Pins the capture contract from TEACHABLE.md §4: durative coaching
// forks a detached `lesson-proposed` draft while the turn's normal
// conation behavior (cancel / transform) stays byte-identical; deictic
// conation never captures. The spy overrides the fire-and-forget
// `emit_lesson_proposed` default and forwards payloads over an mpsc so
// tests can await the detached spawn.

struct ConationRouter;

#[async_trait]
impl Router for ConationRouter {
    async fn classify(
        &self,
        _message: &str,
        _context: &ConversationContext,
        _available_tools: &[ToolDescriptor],
    ) -> Result<RouterClassification> {
        Ok(RouterClassification {
            primary: IntentCandidate {
                intent: Intent::ConationQuery,
                confidence: 1.0,
            },
            alternatives: Vec::new(),
            rationale: None,
            coarse_intent: None,
            self_assessment: None,
            timing: None,
            scope: None,
        })
    }
}

struct SpyLessonChannel {
    tx: tokio::sync::mpsc::UnboundedSender<sovereign_core::types::LessonProposedPayload>,
}

#[async_trait]
impl ApprovalChannel for SpyLessonChannel {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }
    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }
    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
    fn emit_lesson_proposed(&self, payload: sovereign_core::types::LessonProposedPayload) {
        let _ = self.tx.send(payload);
    }
}

fn build_conation_runtime(
    response: &str,
) -> (
    Runtime,
    Arc<MockStore>,
    tokio::sync::mpsc::UnboundedReceiver<sovereign_core::types::LessonProposedPayload>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(MockStore::new());
    let runtime = Runtime::new(sovereign_core::RuntimeParts::new(
        Arc::new(MockInference::new(response)),
        Box::new(ConationRouter),
        Box::new(NoOpPlanner),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        Arc::new(SkillRegistry::new()),
        Arc::new(SpyLessonChannel { tx }),
        sovereign_core::types::InferenceConfig::default(),
        // Phase 4b: enrichment is a required argument, not eight
        // forgettable builders.
        sovereign_core::runtime::lane::LaneSources::none(),
    ));
    (runtime, store, rx)
}

/// Seed a prior assistant reply directly — the non-streaming conation
/// path doesn't persist handler responses (the desktop streaming path
/// saves them at its own persist point), so a real prior turn can't be
/// simulated through `handle_message` alone.
async fn seed_prior_assistant(store: &MockStore, conversation_id: &str) {
    store
        .save_message(&Message {
            id: "prior-1".to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::Assistant,
            content: "a prior reply to transform".to_string(),
            created_at: 0,
            metadata: None,
            version: 0,
        })
        .await
        .unwrap();
}

async fn recv_lesson(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<sovereign_core::types::LessonProposedPayload>,
) -> Option<sovereign_core::types::LessonProposedPayload> {
    tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .ok()
        .flatten()
}

#[tokio::test]
async fn conation_durative_captures_param_lesson_and_answers_normally() {
    let (runtime, store, mut rx) = build_conation_runtime("shortened reply");
    seed_prior_assistant(&store, "c1").await;

    let response = runtime
        .handle_message("keep answers shorter from now on", "c1")
        .await
        .unwrap();
    // The turn itself is the normal conation transform — unchanged.
    assert_eq!(response.message.content, "shortened reply");

    let payload = recv_lesson(&mut rx)
        .await
        .expect("durative coaching must propose a lesson");
    assert_eq!(payload.enforcement, "param");
    assert_eq!(payload.conversation_id, "c1");
    assert_eq!(payload.params["soft_target_cap"], 300);
    assert!(
        payload.prompt_form.is_empty(),
        "param rung never rides the prompt"
    );
    assert!(payload.taught_from.contains("from now on"));
    assert_eq!(
        payload.message_id, "prior-1",
        "provenance points at the prior assistant reply"
    );
}

#[tokio::test]
async fn conation_deictic_does_not_capture() {
    let (runtime, store, mut rx) = build_conation_runtime("shortened reply");
    seed_prior_assistant(&store, "c1").await;

    let response = runtime
        .handle_message("make this shorter", "c1")
        .await
        .unwrap();
    assert_eq!(response.message.content, "shortened reply");

    // Deictic adjustment: obey and forget — no card, nothing stored.
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(400), rx.recv()).await;
    assert!(quiet.is_err(), "deictic conation must not propose a lesson");
}

#[tokio::test]
async fn conation_durative_stop_cancels_and_captures_transform_lesson() {
    let (runtime, _store, mut rx) = build_conation_runtime("unused");
    // "stop mentioning the corpus" takes the cancel sub-shape today
    // (accepted P0 quirk — TEACHABLE §9) AND proposes the term-avoid
    // lesson. Bare "stop" cancels without capturing.
    let response = runtime
        .handle_message("stop mentioning the corpus", "c1")
        .await
        .unwrap();
    assert_eq!(response.message.content, "Stopped.");

    let payload = recv_lesson(&mut rx)
        .await
        .expect("durative stop must propose a transform lesson");
    assert_eq!(payload.enforcement, "transform");
    assert_eq!(payload.params["terms"][0], "corpus");
    assert!(payload.display.starts_with("Don't use:"));

    let response = runtime.handle_message("stop", "c1").await.unwrap();
    assert_eq!(response.message.content, "Stopped.");
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(400), rx.recv()).await;
    assert!(quiet.is_err(), "bare cancel must not propose a lesson");
}

#[tokio::test]
async fn conation_prompt_rung_drafts_via_fast_slot_with_guarded_parse() {
    // Free-form durative coaching falls to the prompt rung; the mock
    // inference doubles as the drafter, returning the draft JSON.
    let draft = r#"{"display": "Explain things simply.", "prompt_form": "Explain like I'm five."}"#;
    let (runtime, store, mut rx) = build_conation_runtime(draft);
    seed_prior_assistant(&store, "c1").await;

    runtime
        .handle_message("from now on explain things like i am five", "c1")
        .await
        .unwrap();
    let payload = recv_lesson(&mut rx)
        .await
        .expect("prompt-rung coaching must propose a drafted lesson");
    assert_eq!(payload.enforcement, "prompt");
    assert_eq!(payload.prompt_form, "Explain like I'm five.");
    assert_eq!(payload.display, "Explain things simply.");
}

// ─── TEACHABLE lesson enforcement (rungs 1/2/4 + whisper) ───────────
//
// Pins TEACHABLE.md §7: a param lesson lowers the length DIRECTIVE (never
// the max_tokens ceiling), the K=1 prompt lesson rides the system message
// outermost, the term-avoid transform mutates the PERSISTED answer while
// `[Source: …]` spans survive, `lessons_applied` records influence, and
// the `kept_lesson` whisper fires exactly once per lesson.

/// Inference mock that records `(system_message, prompt)` per request —
/// visibility into what the synthesis request actually carried.
struct RecordingInference {
    response_text: String,
    requests: std::sync::Mutex<Vec<(Option<String>, String)>>,
}

impl RecordingInference {
    fn new(text: &str) -> Self {
        Self {
            response_text: text.to_string(),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl InferenceProvider for RecordingInference {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        self.requests
            .lock()
            .unwrap()
            .push((request.system_message.clone(), request.prompt.clone()));
        Ok(CompletionResponse {
            text: self.response_text.clone(),
            tokens_used: 10,
            prompt_tokens: 0,
            model_id: "mock".to_string(),
            latency_ms: 1,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }

    // Working single-chunk stream (unlike MockInference, which errors) —
    // the lesson streaming test drives the real deep/simple spawn.
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        self.requests
            .lock()
            .unwrap()
            .push((request.system_message.clone(), request.prompt.clone()));
        let text = self.response_text.clone();
        Ok(Box::pin(futures::stream::once(async move { Ok(text) })))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Err(Error::NotImplemented("mock".to_string()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

async fn lesson_note_store(
    payloads: &[serde_json::Value],
) -> (tempfile::TempDir, Arc<corpus_engine_notes::NoteStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store =
        Arc::new(corpus_engine_notes::NoteStore::open(&dir.path().join("notes.db")).unwrap());
    for payload in payloads {
        store
            .write_note_full(
                "lesson",
                payload["display"].as_str().unwrap_or("lesson"),
                vec![],
                vec![],
                "s1",
                corpus_engine_notes::NoteScope::Global,
                None,
                None,
                corpus_engine_notes::NoteSource::Agent,
                None,
                Some(&payload.to_string()),
            )
            .await
            .unwrap();
    }
    (dir, store)
}

fn param_lesson_payload() -> serde_json::Value {
    serde_json::json!({
        "display": "Keep answers short.",
        "prompt_form": "",
        "enforcement": "param",
        "params": {"soft_target_cap": 300},
        "taught_from": {"excerpt": "keep answers shorter from now on",
                        "conversation_id": "c1", "message_id": ""},
        "created": 1
    })
}

fn prompt_lesson_payload() -> serde_json::Value {
    serde_json::json!({
        "display": "Explain things simply.",
        "prompt_form": "Explain like I'm five.",
        "enforcement": "prompt",
        "params": {},
        "taught_from": {"excerpt": "always explain like I'm five",
                        "conversation_id": "c1", "message_id": ""},
        "created": 2
    })
}

fn transform_lesson_payload() -> serde_json::Value {
    serde_json::json!({
        "display": "Don't use: corpus.",
        "prompt_form": "",
        "enforcement": "transform",
        "params": {"terms": ["corpus"]},
        "taught_from": {"excerpt": "stop mentioning the corpus",
                        "conversation_id": "c1", "message_id": ""},
        "created": 3
    })
}

#[tokio::test]
async fn lessons_shape_the_synthesis_request() {
    // Param lesson caps the length directive; prompt lesson rides the
    // system message outermost. Verified on the non-streaming
    // SimpleQuery path, which shares `prepare_knowledge_context` with
    // the streaming path by construction.
    let (_dir, notes) = lesson_note_store(&[param_lesson_payload(), prompt_lesson_payload()]).await;
    let recording = Arc::new(RecordingInference::new("a fine answer"));
    let runtime = Runtime::new(sovereign_core::RuntimeParts {
        note_store: Some(notes),
        ..sovereign_core::RuntimeParts::new(
            recording.clone(),
            Box::new(PassthroughRouter),
            Box::new(NoOpPlanner),
            Arc::new(ToolRegistry::new()),
            Arc::new(MockStore::new()),
            Arc::new(SkillRegistry::new()),
            Arc::new(AutoApprovalChannel),
            sovereign_core::types::InferenceConfig::default(),
            // Phase 4b: enrichment is a required argument, not eight
            // forgettable builders.
            sovereign_core::runtime::lane::LaneSources::none(),
        )
    });

    runtime
        .handle_message("what is the meaning of x", "c1")
        .await
        .unwrap();

    let requests = recording.requests.lock().unwrap();
    let synth = requests
        .iter()
        .find_map(|(sys, _)| sys.as_deref().filter(|s| s.contains("standing rule")))
        .expect("a synthesis request must carry the K=1 lesson block");
    assert!(
        synth.contains("Explain like I'm five."),
        "compiled prompt_form must ride the system message: {synth}"
    );
    assert!(
        synth.contains("unless it conflicts with a safety or grounding rule"),
        "the lesson block must stay subordinate to the gate"
    );
    assert!(
        synth.contains("approximately 300 tokens"),
        "the length directive must reflect the lesson cap (default config \
         is 2048; the param lesson caps the soft target at 300): {synth}"
    );
}

/// Drain a stream handle fully (the spawn persists the message before
/// the channel closes, so the store is consistent after this returns).
async fn drain(handle: sovereign_core::runtime::StreamHandle) {
    use futures::StreamExt;
    let mut stream = handle.stream;
    while stream.next().await.is_some() {}
}

#[tokio::test]
async fn streaming_turn_applies_term_avoid_and_whispers_once() {
    let (_dir, notes) = lesson_note_store(&[transform_lesson_payload()]).await;
    let mock_answer = "The corpus helps here. [Source: Corpus Handbook] More corpus talk.";
    let store = Arc::new(MockStore::new());
    let runtime = Runtime::new(sovereign_core::RuntimeParts {
        note_store: Some(notes),
        ..sovereign_core::RuntimeParts::new(
            Arc::new(RecordingInference::new(mock_answer)),
            Box::new(PassthroughRouter),
            Box::new(NoOpPlanner),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(SkillRegistry::new()),
            Arc::new(AutoApprovalChannel),
            sovereign_core::types::InferenceConfig::default(),
            // Phase 4b: enrichment is a required argument, not eight
            // forgettable builders.
            sovereign_core::runtime::lane::LaneSources::none(),
        )
    });

    // Turn 1: transform applies, whisper fires.
    drain(
        runtime
            .handle_message_stream("tell me about it", "c1")
            .await
            .unwrap(),
    )
    .await;
    let (content, metadata) = {
        let msgs = store.messages.read().await;
        let assistant = msgs
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .expect("streamed turn must persist an assistant message");
        (
            assistant.content.clone(),
            assistant
                .metadata
                .clone()
                .expect("metadata must be stamped"),
        )
    };
    assert!(
        content.contains("[Source: Corpus Handbook]"),
        "citation span must survive the transform: {content}"
    );
    assert!(
        !content
            .replace("[Source: Corpus Handbook]", "")
            .contains("corpus"),
        "banned term must be stripped outside citations: {content}"
    );
    assert_eq!(
        metadata["lessons_applied"][0]["enforcement"], "transform",
        "metadata must record the transform's influence: {metadata}"
    );
    assert_eq!(
        metadata["kept_lesson"]["display"], "Don't use: corpus.",
        "first application must whisper: {metadata}"
    );

    // Turn 2: still applied, but the whisper fires exactly once.
    drain(
        runtime
            .handle_message_stream("tell me more", "c1")
            .await
            .unwrap(),
    )
    .await;
    let metadata2 = {
        let msgs = store.messages.read().await;
        msgs.iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| m.metadata.clone())
            .expect("second turn metadata")
    };
    assert_eq!(metadata2["lessons_applied"][0]["enforcement"], "transform");
    assert!(
        metadata2["kept_lesson"].is_null(),
        "the whisper must fire exactly once: {metadata2}"
    );
}

#[tokio::test]
async fn conation_prompt_rung_drops_malformed_draft_silently() {
    // Drafter output that fails the parse guards produces NO card —
    // no card at all beats a wrong card (TEACHABLE §4).
    let (runtime, store, mut rx) = build_conation_runtime("not json at all");
    seed_prior_assistant(&store, "c1").await;

    let response = runtime
        .handle_message("from now on explain things like i am five", "c1")
        .await
        .unwrap();
    // The turn still answers normally (the transform reply).
    assert_eq!(response.message.content, "not json at all");
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(400), rx.recv()).await;
    assert!(quiet.is_err(), "malformed draft must drop silently");
}
