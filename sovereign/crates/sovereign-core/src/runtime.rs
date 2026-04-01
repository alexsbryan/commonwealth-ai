use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::{build_context, format_history_as_prompt};
use crate::error::Result;
use crate::registry::ToolRegistry;
use crate::skills::SkillRegistry;
use crate::traits::{InferenceProvider, Planner, Router, StateStore};
use crate::types::*;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub struct Runtime {
    pub inference: Arc<dyn InferenceProvider>,
    pub router: Box<dyn Router>,
    pub planner: Box<dyn Planner>,
    pub tools: ToolRegistry,
    pub store: Arc<dyn StateStore>,
    pub skills: SkillRegistry,
}

impl Runtime {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        router: Box<dyn Router>,
        planner: Box<dyn Planner>,
        tools: ToolRegistry,
        store: Arc<dyn StateStore>,
        skills: SkillRegistry,
    ) -> Self {
        Self {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
        }
    }

    pub async fn handle_message(
        &self,
        message: &str,
        conversation_id: &str,
    ) -> Result<Response> {
        // 1. Build context from store.
        let mut context = build_context(self.store.as_ref(), conversation_id).await?;

        // 2. Save user message.
        let user_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: Role::User,
            content: message.to_string(),
            created_at: now(),
            metadata: None,
        };
        self.store.save_message(&user_msg).await?;
        context.conversation.messages.push(user_msg);

        // 3. Route.
        let tool_descriptors = self.tools.descriptors();
        let intent = self
            .router
            .classify(message, &context, &tool_descriptors)
            .await?;

        // 4. Dispatch based on intent.
        let speed = match &intent {
            Intent::SimpleQuery => Speed::Fast,
            Intent::DeepQuery => Speed::Slow,
            _ => Speed::Medium,
        };

        // 5. Build prompt from conversation history.
        let history = format_history_as_prompt(&context, 10);
        let prompt = if history.is_empty() {
            message.to_string()
        } else {
            format!("{history}\n\nAssistant:")
        };

        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a helpful AI assistant. Respond concisely and accurately.".to_string(),
            ),
            preferred_speed: speed,
            max_tokens: Some(1024),
            temperature: Some(0.7),
            structured_output: None,
        };

        // 6. Call inference.
        let completion = self.inference.complete(&request).await?;

        // 7. Save assistant message.
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
            })),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }
}
