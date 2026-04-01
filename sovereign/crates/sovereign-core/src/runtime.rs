use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::{build_context, format_history_as_prompt};
use crate::error::Result;
use crate::executor::{Executor, TaskContext};
use crate::registry::ToolRegistry;
use crate::skills::SkillRegistry;
use crate::traits::{ApprovalChannel, InferenceProvider, Planner, Router, StateStore};
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
    pub tools: Arc<ToolRegistry>,
    pub store: Arc<dyn StateStore>,
    pub skills: SkillRegistry,
    pub approval: Arc<dyn ApprovalChannel>,
}

impl Runtime {
    pub fn new(
        inference: Arc<dyn InferenceProvider>,
        router: Box<dyn Router>,
        planner: Box<dyn Planner>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn StateStore>,
        skills: SkillRegistry,
        approval: Arc<dyn ApprovalChannel>,
    ) -> Self {
        Self {
            inference,
            router,
            planner,
            tools,
            store,
            skills,
            approval,
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
        match intent {
            Intent::ComplexTask => {
                self.handle_complex_task(message, conversation_id, &context, &tool_descriptors)
                    .await
            }
            Intent::KnowledgeQuery => {
                self.handle_knowledge_query(message, conversation_id, &context)
                    .await
            }
            _ => {
                self.handle_simple(message, conversation_id, &context, &intent)
                    .await
            }
        }
    }

    /// Handle SimpleQuery, DeepQuery, and other non-plan intents.
    async fn handle_simple(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        intent: &Intent,
    ) -> Result<Response> {
        let speed = match intent {
            Intent::SimpleQuery => Speed::Fast,
            Intent::DeepQuery => Speed::Slow,
            _ => Speed::Medium,
        };

        let history = format_history_as_prompt(context, 10);
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
            })),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    /// Handle KnowledgeQuery: search documents → inject into prompt → synthesize.
    async fn handle_knowledge_query(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
    ) -> Result<Response> {
        // 1. Try to embed the query for vector search.
        let embedding = self.inference.embed(message).await.unwrap_or_default();

        // 2. Search documents (hybrid: FTS5 + vector if embedding available).
        let chunks = self
            .store
            .search_documents(&embedding, message, 5)
            .await
            .unwrap_or_default();

        // 3. Build prompt with retrieved context.
        let history = format_history_as_prompt(context, 6);

        let doc_context = if chunks.is_empty() {
            "No relevant documents found.".to_string()
        } else {
            chunks
                .iter()
                .map(|c| format!("[Source: {}]\n{}", c.source, c.content))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n")
        };

        let prompt = format!(
            "{history}\n\nRelevant documents:\n{doc_context}\n\nUser: {message}\n\nAssistant:"
        );

        let request = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a helpful assistant. Answer the user's question based on the provided documents. \
                 Cite the source when referencing document content. If the documents don't contain \
                 relevant information, say so and answer from general knowledge."
                    .to_string(),
            ),
            preferred_speed: Speed::Slow,
            max_tokens: Some(1024),
            temperature: Some(0.7),
            structured_output: None,
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
                "documents_found": chunks.len(),
            })),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: None,
        })
    }

    /// Handle ComplexTask: plan → execute → (replan on failure) → synthesize.
    async fn handle_complex_task(
        &self,
        message: &str,
        conversation_id: &str,
        context: &ConversationContext,
        tool_descriptors: &[ToolDescriptor],
    ) -> Result<Response> {
        // 1. Generate plan.
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
        };
        self.store.save_task(&task).await?;

        // 3. Execute.
        let executor = Executor::new(
            Arc::clone(&self.inference),
            Arc::clone(&self.tools),
            Arc::clone(&self.store),
            Arc::clone(&self.approval),
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
                _ => None,
            })
            .collect();

        let synthesis_prompt = format!(
            "Goal: {message}\n\nStep results:\n{}\n\nProvide a comprehensive final answer that synthesizes all the step results above.",
            step_summaries.join("\n\n")
        );

        let synthesis = self
            .inference
            .complete(&CompletionRequest {
                prompt: synthesis_prompt,
                system_message: Some(
                    "You are a helpful assistant. Synthesize the given step results into a clear, comprehensive answer."
                        .to_string(),
                ),
                preferred_speed: Speed::Slow,
                max_tokens: Some(2048),
                temperature: Some(0.7),
                structured_output: None,
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

        // 7. Save and return assistant message.
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
            })),
        };
        self.store.save_message(&assistant_msg).await?;

        Ok(Response {
            message: assistant_msg,
            task: Some(task),
        })
    }
}
