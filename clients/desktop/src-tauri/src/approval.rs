// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Tauri event payloads the prompt cards are raised with. Until
//! 2026-09-11 this file also held `TauriApprovalChannel`, an in-process
//! `ApprovalDesk` implementing `ApprovalChannel` for a Runtime this app no
//! longer commissions: every prompt now arrives as a `TurnFrame::Prompt`,
//! parks in `state.pending_prompts`, and is answered over the wire
//! (`commands/conversation.rs::answer_wire_prompt`). The desk was the
//! fallback behind that path and could never hold an entry — deleted.

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
    pub kind: sovereign_contracts::types::InformationRequestKind,
    pub task_title: String,
    /// Catalog-grounded acquisition routes for the gap (may be empty).
    /// Serialized with the contracts enum's snake_case tags — the TS
    /// mirror in types.ts matches.
    pub routes: Vec<sovereign_contracts::types::AcquisitionRoute>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ErrorPayload {
    pub message: String,
}

// ─── TauriApprovalChannel ────────────────────────────────────
