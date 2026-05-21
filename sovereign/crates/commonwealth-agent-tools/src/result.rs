//! Canonical tool-result envelope and error taxonomy.
//!
//! Every executor returns `Result<ToolResult, ToolError>`. The
//! `ToolError` enum is closed by design — adapters that need to
//! report agent-specific failure modes do so by translating to one
//! of these variants, not by inventing new strings. This is what
//! makes cross-agent failure-class comparison well-defined.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Structured tool-call result. Shape varies by primitive (see
/// individual executor fns), but every result carries `ok` so the
/// model can branch without parsing the inner shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// True when the tool ran without an executor-level error. A
    /// `cargo_build` that reports compilation errors still has
    /// `ok = true` (the tool ran; the build failed — those are
    /// different events). `ok = false` is reserved for execution
    /// failures (workdir not found, subprocess spawn failure,
    /// timeout).
    pub ok: bool,
    /// Structured payload. Schema is primitive-specific; see each
    /// executor for the exact shape.
    pub payload: serde_json::Value,
}

impl ToolResult {
    /// Build a successful result from a structured payload.
    pub fn ok(payload: serde_json::Value) -> Self {
        Self {
            ok: true,
            payload,
        }
    }
}

#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum ToolError {
    /// Argument JSON didn't parse against the primitive's schema.
    /// The model should re-emit with a corrected arg shape.
    #[error("invalid arguments for {primitive}: {reason}")]
    InvalidArguments {
        primitive: &'static str,
        reason: String,
    },
    /// Path argument refers to something outside the workdir or
    /// the workdir itself was unavailable.
    #[error("workdir access violation: {0}")]
    WorkdirAccess(String),
    /// Filesystem operation failed (read/write/stat).
    #[error("filesystem error in {primitive}: {reason}")]
    Filesystem {
        primitive: &'static str,
        reason: String,
    },
    /// Subprocess (cargo, etc.) failed to spawn or exited non-zero
    /// for a reason other than the build/test reporting failure
    /// itself.
    #[error("subprocess error in {primitive}: {reason}")]
    Subprocess {
        primitive: &'static str,
        reason: String,
    },
    /// Subprocess exceeded its wall-clock budget.
    #[error("subprocess timed out in {primitive} after {secs}s")]
    Timeout {
        primitive: &'static str,
        secs: u64,
    },
}
