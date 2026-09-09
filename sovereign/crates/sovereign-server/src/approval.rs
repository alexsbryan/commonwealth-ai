// SPDX-License-Identifier: AGPL-3.0-or-later
use async_trait::async_trait;
use tokio::sync::{broadcast, RwLock};

use sovereign_contracts::types::{TurnAnswer, TurnPrompt};
use sovereign_core::approval_desk::{ApprovalDesk, ResolveOutcome, StepStatus};
use sovereign_core::error::Result;
use sovereign_core::traits::ApprovalChannel;
use sovereign_core::types::*;

/// Executor progress + approval events, fanned out to every connected
/// consumer by [`ServerApprovalChannel`].
///
/// Deliberately NOT the turn protocol. `sovereign_contracts::types::
/// TurnFrame` carries one tenant's in-flight turn down the one socket
/// that asked for it; these are genuinely broadcast. Until
/// 2026-08-25 both sets were one `ServerEvent` enum kept apart by a doc
/// comment, which meant `event_tx.send(ServerEvent::Token { .. })`
/// compiled — a tenant's answer delivered to every other connected
/// client. Two types is what makes that a type error instead of a thing
/// to remember (TOPOLOGY.md §10 phase 5b, ARCH §7).
///
/// The two ASK variants converged on the turn protocol's shape in
/// sv-surface R1 (2026-09-09): `Prompt { id, prompt }` carries the same
/// closed [`TurnPrompt`] enum and the same host-minted id
/// [`TurnRequest::Answer`] echoes, so a client renders one consent
/// question and answers it the same way whichever host it reached —
/// this host's `{task}:{step}` slot format is simply what it mints the
/// id FROM. `StepDone` stays this host's spelling until its wire
/// producer lands (sv-surface G6).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum ExecutorEvent {
    StepDone {
        task_id: String,
        step: StepSummary,
        status: String,
    },
    Prompt {
        /// The desk key this question is parked under — echo it in
        /// [`sovereign_contracts::types::TurnRequest::Answer`].
        id: String,
        /// The question, in the protocol's closed shape.
        prompt: TurnPrompt,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StepSummary {
    pub id: usize,
    pub description: String,
}

/// Server-side approval channel: broadcast the question, park the executor,
/// resolve on the answer.
///
/// The park-and-resolve half is [`ApprovalDesk`] — shared with every other
/// host rather than a private pair of maps here. What stays server-local is
/// the two things that are genuinely the server's: the FAN-OUT (an
/// `ExecutorEvent` to every subscriber) and the KEY POLICY
/// (`"{task_id}:{step_id}"` over a host-stamped task slot, which is what
/// `POST /v1/tasks/{id}/approve` and the WebSocket `approve` event address).
pub struct ServerApprovalChannel {
    /// Broadcast channel for progress/approval events.
    event_tx: broadcast::Sender<ExecutorEvent>,
    /// The parked questions, keyed by this host's slot format.
    desk: ApprovalDesk<String>,
    /// Current task ID (set before execution).
    task_id: RwLock<String>,
}

impl ServerApprovalChannel {
    pub fn new() -> (Self, broadcast::Receiver<ExecutorEvent>) {
        let (event_tx, event_rx) = broadcast::channel(64);
        let channel = Self {
            event_tx,
            desk: ApprovalDesk::new(),
            task_id: RwLock::new(String::new()),
        };
        (channel, event_rx)
    }

    /// Set the current task ID (call before starting execution).
    pub async fn set_task_id(&self, task_id: &str) {
        *self.task_id.write().await = task_id.to_string();
    }

    /// Submit an answer for a pending question — REST handler or WebSocket
    /// handler. The `id` is the one the Prompt carried; the ONE mapping from
    /// the wire's answer shape to the parked kind lives on the desk.
    pub fn submit(&self, id: &str, answer: &TurnAnswer) -> ResolveOutcome {
        self.desk.resolve_answer(&id.to_string(), answer)
    }

    /// Subscribe to server events.
    pub fn subscribe(&self) -> broadcast::Receiver<ExecutorEvent> {
        self.event_tx.subscribe()
    }
}

#[async_trait]
impl ApprovalChannel for ServerApprovalChannel {
    async fn request_approval(&self, step: &Step, preview: &ActionPreview) -> Result<bool> {
        let task_id = self.task_id.read().await.clone();
        let id = format!("{task_id}:{}", step.id);
        // Parked BEFORE the broadcast: a subscriber that answers faster than
        // this task resumes must find the question already recorded.
        let parked = self.desk.park_approval(id.clone());

        let _ = self.event_tx.send(ExecutorEvent::Prompt {
            id,
            prompt: TurnPrompt::Approval {
                preview: preview.clone(),
            },
        });

        parked.answered().await
    }

    async fn ask_user(&self, question: &str) -> Result<String> {
        let task_id = self.task_id.read().await.clone();
        // A synthetic slot for user input requests — one question per task.
        let id = format!("{task_id}:input");
        let parked = self.desk.park_input(id.clone());

        let _ = self.event_tx.send(ExecutorEvent::Prompt {
            id,
            prompt: TurnPrompt::UserInput {
                question: question.to_string(),
            },
        });

        parked.answered().await
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        // Fire-and-forget: best effort to notify subscribers.
        let task_id = self
            .task_id
            .try_read()
            .map(|t| t.clone())
            .unwrap_or_default();

        let _ = self.event_tx.send(ExecutorEvent::StepDone {
            task_id,
            step: StepSummary {
                id: step.id,
                description: step.description.clone(),
            },
            status: StepStatus::of(output).to_string(),
        });
    }
}
