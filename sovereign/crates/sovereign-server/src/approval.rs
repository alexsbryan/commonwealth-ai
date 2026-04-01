use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{broadcast, oneshot, RwLock};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::ApprovalChannel;
use sovereign_core::types::*;

/// Event emitted by the server for WebSocket/SSE consumers.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum ServerEvent {
    StepStarted {
        task_id: String,
        step: StepSummary,
    },
    StepDone {
        task_id: String,
        step: StepSummary,
        status: String,
    },
    ApprovalReq {
        task_id: String,
        step_id: usize,
        preview: ActionPreview,
    },
    UserInput {
        task_id: String,
        step_id: usize,
        question: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StepSummary {
    pub id: usize,
    pub description: String,
}

/// Pending approval request waiting for a response.
struct PendingApproval {
    sender: oneshot::Sender<bool>,
}

/// Pending user input request waiting for a response.
struct PendingInput {
    sender: oneshot::Sender<String>,
}

/// Server-side approval channel backed by tokio channels.
///
/// When the Executor calls `request_approval()`, this channel:
/// 1. Broadcasts an `ApprovalReq` event (for WebSocket consumers)
/// 2. Stores a oneshot sender keyed by `"{task_id}:{step_id}"`
/// 3. Awaits the oneshot receiver (blocks the Executor step)
///
/// The REST handler `POST /v1/tasks/{id}/approve` or a WebSocket `approve`
/// event calls `submit_approval()` to unblock the waiting task.
pub struct ServerApprovalChannel {
    /// Broadcast channel for progress/approval events.
    event_tx: broadcast::Sender<ServerEvent>,
    /// Pending approval requests: key = "task_id:step_id".
    pending_approvals: Arc<RwLock<HashMap<String, PendingApproval>>>,
    /// Pending user input requests: key = "task_id:step_id".
    pending_inputs: Arc<RwLock<HashMap<String, PendingInput>>>,
    /// Current task ID (set before execution).
    task_id: RwLock<String>,
}

impl ServerApprovalChannel {
    pub fn new() -> (Self, broadcast::Receiver<ServerEvent>) {
        let (event_tx, event_rx) = broadcast::channel(64);
        let channel = Self {
            event_tx,
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            pending_inputs: Arc::new(RwLock::new(HashMap::new())),
            task_id: RwLock::new(String::new()),
        };
        (channel, event_rx)
    }

    /// Set the current task ID (call before starting execution).
    pub async fn set_task_id(&self, task_id: &str) {
        *self.task_id.write().await = task_id.to_string();
    }

    /// Submit an approval decision for a pending request.
    /// Called by REST handler or WebSocket handler.
    pub async fn submit_approval(&self, key: &str, approved: bool) -> bool {
        if let Some(pending) = self.pending_approvals.write().await.remove(key) {
            let _ = pending.sender.send(approved);
            true
        } else {
            false
        }
    }

    /// Submit a user input response for a pending request.
    pub async fn submit_input(&self, key: &str, response: String) -> bool {
        if let Some(pending) = self.pending_inputs.write().await.remove(key) {
            let _ = pending.sender.send(response);
            true
        } else {
            false
        }
    }

    /// Subscribe to server events.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.event_tx.subscribe()
    }
}

#[async_trait]
impl ApprovalChannel for ServerApprovalChannel {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool> {
        let task_id = self.task_id.read().await.clone();
        let key = format!("{task_id}:{}", step.id);

        // Broadcast the approval request event.
        let _ = self.event_tx.send(ServerEvent::ApprovalReq {
            task_id: task_id.clone(),
            step_id: step.id,
            preview: preview.clone(),
        });

        // Create a oneshot channel and wait for the response.
        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .write()
            .await
            .insert(key, PendingApproval { sender: tx });

        // Wait for the approval response (from REST or WebSocket).
        rx.await.map_err(|_| Error::Cancelled)
    }

    async fn ask_user(&self, question: &str) -> Result<String> {
        let task_id = self.task_id.read().await.clone();
        // Use a synthetic step_id for user input requests.
        let key = format!("{task_id}:input");

        let _ = self.event_tx.send(ServerEvent::UserInput {
            task_id: task_id.clone(),
            step_id: 0,
            question: question.to_string(),
        });

        let (tx, rx) = oneshot::channel();
        self.pending_inputs
            .write()
            .await
            .insert(key, PendingInput { sender: tx });

        rx.await.map_err(|_| Error::Cancelled)
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        // Fire-and-forget: best effort to notify subscribers.
        let task_id = self.task_id.try_read().map(|t| t.clone()).unwrap_or_default();

        let status = match output {
            StepOutput::Text(_) | StepOutput::Json(_) => "done",
            StepOutput::Jump(t) => {
                let _ = self.event_tx.send(ServerEvent::StepDone {
                    task_id,
                    step: StepSummary {
                        id: step.id,
                        description: step.description.clone(),
                    },
                    status: format!("jump to {t}"),
                });
                return;
            }
            StepOutput::Skipped => "skipped",
        };

        let _ = self.event_tx.send(ServerEvent::StepDone {
            task_id,
            step: StepSummary {
                id: step.id,
                description: step.description.clone(),
            },
            status: status.to_string(),
        });
    }
}
