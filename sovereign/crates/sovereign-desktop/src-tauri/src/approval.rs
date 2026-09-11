// SPDX-License-Identifier: AGPL-3.0-or-later
use async_trait::async_trait;
use tokio::sync::RwLock;

use tauri::Emitter;

use sovereign_core::approval_desk::{ApprovalDesk, StepStatus};
use sovereign_core::error::Result;
use sovereign_core::traits::ApprovalChannel;
use sovereign_core::types::*;

// ─── Event Payloads ──────────────────────────────────────────

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
///
/// `kind` discriminates the two producers (post-answer refinement vs
/// planned task-blocking step); the UI renders distinct chrome per
/// kind. `task_title` is populated only for `step_block` cards.
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
    pub kind: sovereign_core::types::InformationRequestKind,
    pub task_title: String,
    /// Catalog-grounded acquisition routes for the gap (may be empty).
    /// Serialized with the contracts enum's snake_case tags — the TS
    /// mirror in types.ts matches.
    pub routes: Vec<sovereign_core::types::AcquisitionRoute>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ErrorPayload {
    pub message: String,
}

// ─── TauriApprovalChannel ────────────────────────────────────

/// The desktop's approval channel: raise the card, park the executor, resolve
/// when the user answers.
///
/// The park-and-resolve half is [`ApprovalDesk`] — shared with every other
/// host rather than three private maps here. What stays desktop-local is what
/// genuinely is the desktop's: the Tauri EVENT each question is shown as, and
/// the KEY POLICY (`{task_id}:{step_id}` / `:input` / `:info:{step_id}` over a
/// host-stamped slot), which is what the frontend echoes back to
/// `submit_approval` and its siblings. The keys are byte-identical to what
/// they were, so no TypeScript moved with this.
pub struct TauriApprovalChannel {
    app_handle: tauri::AppHandle,
    /// Every parked question — approvals, `ask_user`, and the structured
    /// information requests — on one desk instead of three maps.
    desk: ApprovalDesk<String>,
    task_id: RwLock<String>,
}

impl TauriApprovalChannel {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle,
            desk: ApprovalDesk::new(),
            task_id: RwLock::new(String::new()),
        }
    }

    pub async fn set_task_id(&self, task_id: &str) {
        *self.task_id.write().await = task_id.to_string();
    }

    /// Accessor for the underlying `AppHandle`. Used by
    /// `AppState::new_with_mode` to construct sibling event emitters
    /// (e.g. `TauriRoutingEventSink`) without plumbing the handle
    /// through every call site.
    pub fn app_handle(&self) -> tauri::AppHandle {
        self.app_handle.clone()
    }

    pub fn submit_approval(&self, key: &str, approved: bool) -> bool {
        self.desk
            .resolve_approval(&key.to_string(), approved)
            .reached_a_question()
    }

    pub fn submit_input(&self, key: &str, response: String) -> bool {
        self.desk
            .resolve_input(&key.to_string(), response)
            .reached_a_question()
    }

    /// Resolve a pending information-request. `content = None` means the
    /// user pressed skip; `Some(text)` means they pasted something.
    pub fn submit_information_response(&self, key: &str, content: Option<String>) -> bool {
        self.desk
            .resolve_information(&key.to_string(), content)
            .reached_a_question()
    }

    /// True iff a pending information-request exists for `key`. Used by
    /// the search-now affordance to fail fast before spending a search
    /// budget on a stale UI submission.
    pub fn has_pending_information(&self, key: &str) -> bool {
        self.desk.is_parked(&key.to_string())
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
        // Parked BEFORE the card goes up: a user who answers faster than this
        // task resumes must find the question already recorded.
        let parked = self.desk.park_approval(key.clone());

        self.emit(
            "approval-request",
            ApprovalRequestPayload {
                task_id,
                step_id: step.id,
                key,
                tool_id: preview.tool_id.clone(),
                description: preview.description.clone(),
                params: preview.params.clone(),
            },
        );

        parked.answered().await
    }

    async fn ask_user(&self, question: &str) -> Result<String> {
        let task_id = self.task_id.read().await.clone();
        let key = format!("{task_id}:input");
        let parked = self.desk.park_input(key.clone());

        self.emit(
            "user-input-request",
            UserInputRequestPayload {
                task_id,
                key,
                question: question.to_string(),
            },
        );

        parked.answered().await
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
        let parked = self.desk.park_information(key.clone());

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
                kind: request.kind,
                task_title: request.task_title.clone(),
                routes: request.routes.clone(),
            },
        );

        // A dropped channel (app shutdown) reads as the SKIP the user could
        // have pressed — the executor falls through to corpus-only synthesis
        // either way. That contract lives on `Parked` now.
        parked.answered_or_skipped().await
    }

    fn emit_progress(&self, step: &Step, output: &StepOutput) {
        let task_id = self
            .task_id
            .try_read()
            .map(|t| t.clone())
            .unwrap_or_default();

        self.emit(
            "step-done",
            StepDonePayload {
                task_id,
                step_id: step.id,
                description: step.description.clone(),
                status: StepStatus::of(output).to_string(),
            },
        );
    }

    fn emit_message_refined(&self, payload: MessageRefinedPayload) {
        // The frontend listens for "message-refined" in ChatView and
        // replaces the existing bubble's content with `new_content`.
        self.emit("message-refined", payload);
    }

    fn emit_lesson_proposed(&self, payload: LessonProposedPayload) {
        // TEACHABLE P0: ChatView listens for "lesson-proposed" and
        // renders the Learn-this card. Fire-and-forget — Save calls
        // the `save_lesson` command with this payload later; "Not
        // this" calls nothing. No pending map, nothing blocks.
        self.emit("lesson-proposed", payload);
    }
}
