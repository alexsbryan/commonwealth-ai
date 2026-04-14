use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{oneshot, RwLock};

use tauri::Emitter;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::ApprovalChannel;
use sovereign_core::types::*;

// ─── Event Payloads ──────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct StepStartedPayload {
    pub task_id: String,
    pub step_id: usize,
    pub description: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StepDonePayload {
    pub task_id: String,
    pub step_id: usize,
    pub description: String,
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApprovalRequestPayload {
    pub task_id: String,
    pub step_id: usize,
    pub key: String,
    pub tool_id: String,
    pub description: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserInputRequestPayload {
    pub task_id: String,
    pub key: String,
    pub question: String,
}

/// Sent to the frontend when the agent suspends the task to ask the user
/// for a specific external piece of information. Rendered as a card, not
/// a chat bubble — see InformationRequestCard.svelte.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InformationRequestPayload {
    pub task_id: String,
    pub step_id: usize,
    pub key: String,
    pub current_understanding: String,
    pub gap: String,
    pub relevance: String,
    pub satisfying_source: String,
    pub search_hints: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ErrorPayload {
    pub message: String,
}

// ─── TauriApprovalChannel ────────────────────────────────────

pub struct TauriApprovalChannel {
    app_handle: tauri::AppHandle,
    pending_approvals: Arc<RwLock<HashMap<String, oneshot::Sender<bool>>>>,
    pending_inputs: Arc<RwLock<HashMap<String, oneshot::Sender<String>>>>,
    /// Pending info-request waits, keyed by `{task_id}:info:{step_id}`.
    /// The Option<String> is None when the user pressed skip,
    /// Some(content) when they pasted something.
    pending_info: Arc<RwLock<HashMap<String, oneshot::Sender<Option<String>>>>>,
    task_id: RwLock<String>,
}

impl TauriApprovalChannel {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle,
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            pending_inputs: Arc::new(RwLock::new(HashMap::new())),
            pending_info: Arc::new(RwLock::new(HashMap::new())),
            task_id: RwLock::new(String::new()),
        }
    }

    pub async fn set_task_id(&self, task_id: &str) {
        *self.task_id.write().await = task_id.to_string();
    }

    pub async fn submit_approval(&self, key: &str, approved: bool) -> bool {
        if let Some(sender) = self.pending_approvals.write().await.remove(key) {
            let _ = sender.send(approved);
            true
        } else {
            false
        }
    }

    pub async fn submit_input(&self, key: &str, response: String) -> bool {
        if let Some(sender) = self.pending_inputs.write().await.remove(key) {
            let _ = sender.send(response);
            true
        } else {
            false
        }
    }

    /// Resolve a pending information-request. `content = None` means the
    /// user pressed skip; `Some(text)` means they pasted something.
    pub async fn submit_information_response(
        &self,
        key: &str,
        content: Option<String>,
    ) -> bool {
        if let Some(sender) = self.pending_info.write().await.remove(key) {
            let _ = sender.send(content);
            true
        } else {
            false
        }
    }

    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        if let Err(e) = self.app_handle.emit(event, payload) {
            tracing::warn!("Failed to emit event {event}: {e}");
        }
    }

    /// Emit an event to the frontend. Public so that tool progress
    /// callbacks (e.g. document operation progress) can use it.
    pub fn emit_event<S: serde::Serialize + Clone>(&self, event: &str, payload: S) {
        self.emit(event, payload);
    }
}

#[async_trait]
impl ApprovalChannel for TauriApprovalChannel {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool> {
        let task_id = self.task_id.read().await.clone();
        let key = format!("{task_id}:{}", step.id);

        self.emit(
            "approval-request",
            ApprovalRequestPayload {
                task_id: task_id.clone(),
                step_id: step.id,
                key: key.clone(),
                tool_id: preview.tool_id.clone(),
                description: preview.description.clone(),
                params: preview.params.clone(),
            },
        );

        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .write()
            .await
            .insert(key, tx);

        rx.await.map_err(|_| Error::Cancelled)
    }

    async fn ask_user(&self, question: &str) -> Result<String> {
        let task_id = self.task_id.read().await.clone();
        let key = format!("{task_id}:input");

        self.emit(
            "user-input-request",
            UserInputRequestPayload {
                task_id: task_id.clone(),
                key: key.clone(),
                question: question.to_string(),
            },
        );

        let (tx, rx) = oneshot::channel();
        self.pending_inputs
            .write()
            .await
            .insert(key, tx);

        rx.await.map_err(|_| Error::Cancelled)
    }

    async fn request_information(&self, request: &InformationRequest) -> Option<String> {
        // Prefer the request's task_id (stamped by the executor) but fall
        // back to the channel's last set_task_id call if it's empty.
        let task_id = if request.task_id.is_empty() {
            self.task_id.read().await.clone()
        } else {
            request.task_id.clone()
        };
        let key = format!("{task_id}:info:{}", request.step_id);

        self.emit(
            "information-request",
            InformationRequestPayload {
                task_id,
                step_id: request.step_id,
                key: key.clone(),
                current_understanding: request.current_understanding.clone(),
                gap: request.gap.clone(),
                relevance: request.relevance.clone(),
                satisfying_source: request.satisfying_source.clone(),
                search_hints: request.search_hints.clone(),
            },
        );

        let (tx, rx) = oneshot::channel();
        self.pending_info.write().await.insert(key, tx);

        // If the receiver errors (channel dropped, e.g. app shutdown),
        // treat as skip rather than propagating an error — the executor
        // can fall through to a corpus-only synthesis.
        rx.await.unwrap_or(None)
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        let task_id = self
            .task_id
            .try_read()
            .map(|t| t.clone())
            .unwrap_or_default();

        let status = match output {
            StepOutput::Text(_) | StepOutput::Json(_) | StepOutput::ReasonWithToolsResult { .. } => "done".to_string(),
            StepOutput::Jump(t) => format!("jump to {t}"),
            StepOutput::Skipped => "skipped".to_string(),
        };

        self.emit(
            "step-done",
            StepDonePayload {
                task_id,
                step_id: step.id,
                description: step.description.clone(),
                status,
            },
        );
    }
}
