// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crate-wide error and `Result` vocabulary shared by the daemon and packages.
//!
//! Variant display strings (the `#[error(...)]` attributes) are the user-facing
//! message — surfaces render them verbatim, so keep them self-explanatory.
use thiserror::Error;

/// Unified error for every fallible contract operation.
///
/// Coarse by design: variants classify *which subsystem* failed so callers can
/// route or retry; the payload string carries the human-readable detail.
#[derive(Debug, Error)]
pub enum Error {
    /// The inference backend (local llama.cpp slot or peer) failed mid-request.
    #[error("Inference error: {0}")]
    Inference(String),

    /// A compute-slot child process (DISTRIBUTED_PILOT_READINESS.md P1) was
    /// unreachable — not yet warm, mid-restart after a crash, or gone — and
    /// the request had no in-process fallback. Fail-fast: the caller retries
    /// after the supervisor respawns rather than hanging on a dead socket.
    #[error("Compute slot unavailable: {slot}: {reason}")]
    ComputeUnavailable {
        /// Name of the compute slot/pool that was unreachable.
        slot: String,
        /// Why it was unavailable (warming / restarting / exited), surfaced verbatim.
        reason: String,
    },

    /// The named model/slot was requested but is not resident in memory.
    #[error("Model not loaded: {0}")]
    ModelNotLoaded(String),

    /// No eligible slot or peer could be selected for the request (scheduler-level, before inference starts).
    #[error("Routing error: {0}")]
    Routing(String),

    /// The LLM-produced plan failed validation: malformed JSON, missing/empty `steps`, or a dependency cycle.
    #[error("Planning error: {0}")]
    Planning(String),

    /// A plan step failed while being executed (as opposed to `Planning`, which is pre-execution).
    #[error("Execution error: {0}")]
    Execution(String),

    /// A specific tool invocation failed inside the tool's own logic.
    #[error("Tool error: {tool_id}: {message}")]
    Tool {
        /// Registry id of the tool that failed (the `ToolRegistry` key).
        tool_id: String,
        /// Failure detail as reported by the tool, surfaced verbatim.
        message: String,
    },

    /// The requested tool id is not registered in the [`ToolRegistry`](crate::ToolRegistry).
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// The persistence layer (SQLite state store, LanceDB index, filesystem) failed.
    #[error("Storage error: {0}")]
    Storage(String),

    /// A requested entity (conversation, recipe, corpus, ...) does not exist. Payload names it.
    #[error("Not found: {0}")]
    NotFound(String),

    /// The caller is not allowed to perform the operation; distinct from `NotFound` so surfaces can show a consent prompt instead of a 404.
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// The operation is defined by the contract but this implementation does not support it yet.
    #[error("Not implemented: {0}")]
    NotImplemented(String),

    /// Serde encode/decode failed; `serde_json::Error` converts into this via `From`.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Caller-supplied input was rejected by validation before any work started.
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// The task was interrupted: user cancel, or an approval/response channel was dropped.
    #[error("Task cancelled")]
    Cancelled,

    /// A health checker's `repair()` was asked to fix an issue kind it has no remediation for (validators return this as their catch-all arm).
    #[error("Repair not supported for this issue type")]
    RepairNotSupported,

    /// A corpus write was refused because the owning recipe sets `auto_update = false`.
    #[error("Corpus update not authorised (auto_update = false in recipe)")]
    UpdateNotAuthorised,

    /// Transparent wrapper for foreign errors that don't fit the taxonomy; displays as the inner error.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Crate-standard result alias; every trait method in [`traits`](crate::traits) returns this.
pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serialization(e.to_string())
    }
}
