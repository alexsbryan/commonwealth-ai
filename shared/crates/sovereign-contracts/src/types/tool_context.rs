// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ToolContext`], split from `routing.rs` when it reached its size ceiling;
//! re-exported by types/mod.rs at its historical path.
use super::{ConversationId, TaskId};
use serde::{Deserialize, Serialize};

/// Ambient state handed to every `Tool::execute` call: who is asking (conversation/task) and per-call flags.
///
/// `Default` exists so a caller names only the fields it actually has
/// (`ToolContext { conversation_id: id, ..Default::default() }`) — adding a
/// turn fact here must not be a workspace-wide edit every time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolContext {
    /// Conversation the call belongs to.
    pub conversation_id: ConversationId,
    /// Owning task when called from a plan step; `None` for direct invocations.
    pub task_id: Option<TaskId>,
    /// Working directory for filesystem-affecting tools, when one applies.
    pub working_directory: Option<String>,
    /// True when this tool is being called inside a ReasonWithTools loop.
    /// Tools may format results differently for reasoning vs. synthesis.
    #[serde(default)]
    pub in_reasoning_loop: bool,
    /// Identifier for the calling agent's session, used by the work
    /// atlas to group successive tool calls into a single
    /// coordination session. Populated by `mcp_router` from the
    /// `X-Agent-Session` HTTP header; falls back to a synthetic
    /// `conn:<mcp_session>` per-connection token when no header is
    /// present, and is `None` for in-process callers (CLI, tests,
    /// runtime-internal tool execution) that don't go through the
    /// MCP transport. `#[serde(default)]` so older serialized
    /// contexts decode cleanly.
    #[serde(default)]
    pub agent_session_token: Option<String>,
    /// Zero-based count of prior user turns in this conversation
    /// (Tier 1 result memory). Tools that return citation-shaped
    /// evidence call `EvidenceId::from_index_with_turn(idx,
    /// turn_index)` so the resulting handles are unique across
    /// the conversation's history. `#[serde(default)]` means
    /// pre-Tier-1 serialized contexts decode as turn 0 — degraded
    /// but valid (handles render as `ev-T0-NNNN`).
    #[serde(default)]
    pub turn_index: usize,
    /// The user's question for this turn, verbatim, when the executor
    /// knows it (plan steps carry the task goal; direct//in-process
    /// invocations leave it `None`).
    ///
    /// Exists so a tool that claims AUTHORITY over a question can
    /// enforce, in code, that what it answers is what was asked —
    /// `Tool::claims` already receives the question at routing time, so
    /// this is the same fact at execute time and keeps ONE decider for
    /// question-derived constraints at both ends (ARCH §10.6).
    /// Motivating failure (FINANCIAL_CORPORA §7.6, reproduced
    /// 2026-08-16): asked for CALENDAR 2025, the planner called
    /// `sec_facts` with `period: "FY2025"` — while its own next step
    /// explained that Apple's fiscal year is not calendar 2025. A model
    /// instruction is not a guarantee; only code is.
    ///
    /// Never a permission or trust input — it is the asker's own text.
    #[serde(default)]
    pub question: Option<String>,
    /// The one code corpus this call is about, or `None` for every corpus.
    /// Each surface fills it at its edge: MCP from the `x-svrn-corpus`
    /// header (checked against the indexed corpora before a tool runs), the
    /// CLI from the corpus built from the cwd's git root. Not
    /// `working_directory`: that names a path filesystem tools act in.
    #[serde(default)]
    pub corpus_scope: Option<String>,
}
