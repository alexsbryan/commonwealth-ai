// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desk-domain outcome types that cross the wire.
//!
//! These lived in `sovereign_core::approval_desk` until sv-surface rung
//! R1 (2026-09-09) gave the turn protocol its NOTICE half: `TurnNotice::
//! StepDone` carries a [`StepStatus`] and `TurnNotice::ResolveAck`
//! carries a [`ResolveOutcome`], and a wire type may not mirror a core
//! type — a mirror is the second decider for the same fact (ARCH §10.6;
//! the rung-4 reading-DTO deletion is the standing precedent). They
//! moved here the same way `ToolDecisionOutcome` did in rung 6 D:
//! sovereign-contracts owns the definition, `sovereign_core::
//! approval_desk` re-exports it at the historical path, and every
//! existing importer is unaffected.

use serde::{Deserialize, Serialize};

use crate::types::StepOutput;

/// What a resolve attempt did. Named rather than collapsed into a bool: an
/// answerer that heard nothing back cannot tell "accepted" from "never
/// arrived" (ARCH §18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveOutcome {
    /// The question was found and the executor is running again.
    Resolved,
    /// Nothing is parked under that key — already answered, the turn ended,
    /// or the key names something this answerer does not own.
    NoSuchPending,
    /// A question IS parked there, of the other kind. The entry is left
    /// alone: a wrong-kind answer must not consume the question the right one
    /// is still coming for.
    WrongKind,
}

impl ResolveOutcome {
    /// The `bool` the pre-desk `submit_*` methods returned — "did this reach
    /// something". Kept so a host's own wire contract need not change to sit
    /// on the desk.
    pub fn reached_a_question(self) -> bool {
        self == ResolveOutcome::Resolved
    }
}

/// What became of a step, for progress emission.
///
/// One decider for a match that existed four times — core's
/// `AutoApprovalChannel`, the server's channel, the daemon socket's and the
/// desktop's — each re-deriving the same three outcomes, and the desktop's
/// spelling the jump differently from the rest.
///
/// On the wire (`TurnNotice::StepDone`) it is TYPED, not pre-rendered:
/// the hosts' `status: String` spellings ran `StepStatus::to_string()`
/// first, which turns `Jump(4)` into the prose "jump to 4" — lossy, and
/// a display concern baked into a protocol. Here `jump` carries its
/// target as a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step produced output.
    Done,
    /// The step never ran (an untaken branch arm).
    Skipped,
    /// A `Branch` resolved to a jump; the payload is the step jumped to.
    Jump(usize),
}

impl StepStatus {
    pub fn of(output: &StepOutput) -> Self {
        match output {
            StepOutput::Text(_)
            | StepOutput::Json(_)
            | StepOutput::ReasonWithToolsResult { .. } => StepStatus::Done,
            StepOutput::Jump(target) => StepStatus::Jump(*target),
            StepOutput::Skipped => StepStatus::Skipped,
        }
    }
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepStatus::Done => f.write_str("done"),
            StepStatus::Skipped => f.write_str("skipped"),
            StepStatus::Jump(target) => write!(f, "jump to {target}"),
        }
    }
}
