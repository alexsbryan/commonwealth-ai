use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::Stream;

use sovereign_core::context::{build_context, format_history_as_prompt};
use sovereign_core::error::{Error, Result};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::runtime::Runtime;
use sovereign_core::skills::*;
use sovereign_core::stubs::{NoOpPlanner, PassthroughRouter};
use sovereign_core::traits::*;
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
            model_id: "mock".to_string(),
            latency_ms: 1,
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[async_trait]
impl StateStore for MockStore {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        self.messages.write().await.push(msg.clone());
        Ok(())
    }
    async fn get_conversation(&self, id: &str) -> Result<Conversation> {
        let msgs = self.messages.read().await;
        let conv_msgs: Vec<Message> = msgs.iter().filter(|m| m.conversation_id == id).cloned().collect();
        if conv_msgs.is_empty() {
            return Err(Error::NotFound(format!("Conversation {id}")));
        }
        Ok(Conversation {
            id: id.to_string(),
            title: None,
            messages: conv_msgs,
            created_at: now(),
            updated_at: now(),
        })
    }
    async fn list_conversations(&self, _limit: usize, _offset: usize) -> Result<Vec<Conversation>> {
        Ok(Vec::new())
    }
    async fn search_messages(&self, _query: &str) -> Result<Vec<Message>> {
        Ok(Vec::new())
    }
    async fn delete_conversation(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn save_task(&self, _task: &Task) -> Result<()> {
        Ok(())
    }
    async fn get_task(&self, _id: &str) -> Result<Task> {
        Err(Error::NotFound("task".to_string()))
    }
    async fn save_memory(&self, _memory: &Memory) -> Result<()> {
        Ok(())
    }
    async fn get_relevant_memories(&self, _context: &str, _limit: usize) -> Result<Vec<Memory>> {
        Ok(Vec::new())
    }
    async fn store_chunks(&self, _chunks: &[DocumentChunk]) -> Result<()> {
        Ok(())
    }
    async fn search_documents(&self, _qe: &[f32], _qt: &str, _l: usize) -> Result<Vec<DocumentChunk>> {
        Ok(Vec::new())
    }
    async fn get_permission(&self, _tool_id: &str, _scope: &str) -> Result<Option<bool>> {
        Ok(None)
    }
    async fn set_permission(&self, _tool_id: &str, _scope: &str, _granted: bool) -> Result<()> {
        Ok(())
    }
}

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
    reg.register(Box::new(DummyTool { id: "tool_a".to_string() }));
    reg.register(Box::new(DummyTool { id: "tool_b".to_string() }));

    assert_eq!(reg.count(), 2);
    assert_eq!(reg.descriptors().len(), 2);
    assert!(reg.get("tool_a").is_ok());
    assert!(reg.get("tool_b").is_ok());
    assert!(reg.get("tool_c").is_err());
}

// ─── SkillRegistry Tests ──────────────────────────────────────

fn make_skill(id: &str, trigger: &str, synthesis: Option<&str>) -> Skill {
    Skill {
        id: id.to_string(),
        name: id.to_string(),
        version: "0.1.0".to_string(),
        routing: RoutingHints {
            trigger_phrases: vec![trigger.to_string()],
            default_intent: Some("ComplexTask".to_string()),
            min_confidence: Some(0.8),
        },
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
        },
    }
}

#[test]
fn skill_registry_empty() {
    let reg = SkillRegistry::new();
    assert!(reg.list().is_empty());
    assert!(reg.active_skills().is_empty());
    assert!(reg.routing_hints().trigger_phrases.is_empty());
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

#[test]
fn skill_registry_merge_routing_hints() {
    let mut reg = SkillRegistry::new();
    reg.register(make_skill("a", "alpha", None));
    reg.register(make_skill("b", "beta", None));
    reg.activate("a");
    reg.activate("b");

    let hints = reg.routing_hints();
    assert_eq!(hints.trigger_phrases.len(), 2);
    assert_eq!(hints.min_confidence, Some(0.8));
}

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
    let ctx = build_context(&store, "new-convo").await.unwrap();
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
            created_at: now(),
            metadata: None,
        })
        .await
        .unwrap();

    let ctx = build_context(&store, "c1").await.unwrap();
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
        },
        memories: Vec::new(),
        working_memory: None,
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
                },
                Message {
                    id: "2".to_string(),
                    conversation_id: "c1".to_string(),
                    role: Role::Assistant,
                    content: "Hello!".to_string(),
                    created_at: 2,
                    metadata: None,
                },
            ],
            created_at: 0,
            updated_at: 0,
        },
        memories: Vec::new(),
        working_memory: None,
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
        })
        .collect();

    let ctx = ConversationContext {
        conversation: Conversation {
            id: "c1".to_string(),
            title: None,
            messages,
            created_at: 0,
            updated_at: 0,
        },
        memories: Vec::new(),
        working_memory: None,
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
        },
        memories: Vec::new(),
        working_memory: None,
    };

    let intent = router.classify("anything", &ctx, &[]).await.unwrap();
    assert!(matches!(intent, Intent::SimpleQuery));
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
        },
        memories: Vec::new(),
        working_memory: None,
    };

    let result = planner.plan("do something", &ctx, &[]).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotImplemented(_)));
}

// ─── Runtime Integration Tests ─────────────────────────────────

fn build_runtime(response: &str) -> (Runtime, Arc<MockStore>) {
    let store = Arc::new(MockStore::new());
    let runtime = Runtime::new(
        Arc::new(MockInference::new(response)),
        Box::new(PassthroughRouter),
        Box::new(NoOpPlanner),
        ToolRegistry::new(),
        store.clone(),
        SkillRegistry::new(),
    );
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
