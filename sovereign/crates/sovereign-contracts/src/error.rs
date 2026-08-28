// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crate-wide error and `Result` vocabulary shared by the daemon and packages.
//!
//! Variant display strings (the `#[error(...)]` attributes) are the user-facing
//! message — surfaces render them verbatim, so keep them self-explanatory.
use crate::oicp::InferenceError;
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

    /// The host refused a request rather than making it wait an unreasonable
    /// time for a model permit (MESH_N4_TOPOLOGY M5).
    ///
    /// Structured rather than a formatted string because callers must branch
    /// on it: the HTTP boundary renders `503 + Retry-After`, and a peer load
    /// balancer reads it as "try another holder". An `Err` collapsed into
    /// prose is the §18.3 smell this exists to avoid.
    ///
    /// Distinct from `Routing`: the request was well-formed and the model IS
    /// present. This says "not now, and here is when".
    #[error(
        "host busy: ~{predicted_wait_ms} ms predicted wait at queue position \
         {position}; retry after {retry_after_secs}s"
    )]
    QueueShed {
        /// 1-based place this caller would have taken in line.
        position: u32,
        /// Predicted wait, from observed turn durations on this slot.
        predicted_wait_ms: u64,
        /// Hint for `Retry-After`; always ≥ 1.
        retry_after_secs: u64,
    },

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

impl Error {
    /// Build a [`Error::QueueShed`] with the retry hint DERIVED from the
    /// predicted wait.
    ///
    /// Exists so `retry_after_secs` has one implementation and one name
    /// (§10.6). Two independent shed sites now report it — the model
    /// slot's predicted-wait gate and the FastShort coalescer's queue
    /// bound — and a second copy of `div_ceil(1_000).max(1)` is exactly
    /// the drift this constructor prevents. Always ≥ 1: a
    /// `Retry-After: 0` is an invitation to hot-loop the host that just
    /// refused you.
    pub fn queue_shed(position: u32, predicted_wait_ms: u64) -> Self {
        Error::QueueShed {
            position,
            predicted_wait_ms,
            retry_after_secs: predicted_wait_ms.div_ceil(1_000).max(1),
        }
    }
}

/// Crate-standard result alias; every trait method in [`traits`](crate::traits) returns this.
pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serialization(e.to_string())
    }
}

/// Lift a protocol failure into the runtime's error taxonomy.
///
/// Total and lossless: every [`InferenceError`] variant has an exact twin
/// here, so nothing is invented and nothing is dropped.
impl From<InferenceError> for Error {
    fn from(e: InferenceError) -> Self {
        match e {
            InferenceError::Inference { message } => Error::Inference(message),
            InferenceError::ComputeUnavailable { slot, reason } => {
                Error::ComputeUnavailable { slot, reason }
            }
            InferenceError::ModelNotLoaded { model } => Error::ModelNotLoaded(model),
            InferenceError::Routing { message } => Error::Routing(message),
            InferenceError::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            } => Error::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            },
            InferenceError::InvalidInput { message } => Error::InvalidInput(message),
            InferenceError::NotImplemented { message } => Error::NotImplemented(message),
            InferenceError::Cancelled => Error::Cancelled,
        }
    }
}

/// Narrow a runtime failure to what the inference wire can actually carry.
///
/// The other direction, and deliberately NOT symmetric — read this before
/// using it. Lifting is exact; narrowing cannot be, because `Planning`,
/// `Execution`, `Storage`, `Tool` and the rest have no protocol meaning. The
/// eight with twins map variant-for-variant; everything else widens to
/// [`InferenceError::Inference`] carrying the full `Display` text, which keeps
/// the original variant's prefix ("Storage error: ...") — so the detail
/// survives even though the ability to `match` on it across the seam does not.
///
/// This is not new policy. `sovereign-compute`'s wire encoder has always done
/// exactly this with its `_ => "inference"` arm. What changes is that the
/// decision now has ONE site and says so out loud, rather than being an
/// unnamed default at the bottom of a `match` (ARCH §10.6; §18.3's rule that a
/// substitution is named, never silent).
///
/// Takes a reference because the wire encoder only ever borrows the error it
/// is reporting on; the payloads are `String`s and are cloned.
impl From<&Error> for InferenceError {
    fn from(e: &Error) -> Self {
        match e {
            Error::Inference(message) => InferenceError::Inference {
                message: message.clone(),
            },
            Error::ComputeUnavailable { slot, reason } => InferenceError::ComputeUnavailable {
                slot: slot.clone(),
                reason: reason.clone(),
            },
            Error::ModelNotLoaded(model) => InferenceError::ModelNotLoaded {
                model: model.clone(),
            },
            Error::Routing(message) => InferenceError::Routing {
                message: message.clone(),
            },
            Error::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            } => InferenceError::QueueShed {
                position: *position,
                predicted_wait_ms: *predicted_wait_ms,
                retry_after_secs: *retry_after_secs,
            },
            Error::InvalidInput(message) => InferenceError::InvalidInput {
                message: message.clone(),
            },
            Error::NotImplemented(message) => InferenceError::NotImplemented {
                message: message.clone(),
            },
            Error::Cancelled => InferenceError::Cancelled,
            other => InferenceError::Inference {
                message: other.to_string(),
            },
        }
    }
}
