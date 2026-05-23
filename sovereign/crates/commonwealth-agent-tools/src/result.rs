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
    /// Pre-write syntax check rejected a write_file call before
    /// touching disk. Closes the "model emits English prose / typos
    /// inside `content`" class observed on 3.2-lights-out-python
    /// (2026-05-23): syntax defects no longer land on disk where
    /// the next build cycle would discover them via cargo/pytest —
    /// they're rejected at the write boundary so the Implementer
    /// can re-emit immediately without burning an Evaluator
    /// round-trip. `rendered_errors` is the compiler-shape error
    /// block from `SyntaxValidator::render_errors`.
    #[error("pre-write syntax check rejected {primitive}: {rendered_errors}")]
    SyntaxRejected {
        primitive: &'static str,
        /// Language id (e.g. `"Rust"`, `"Python"`). Carried as `String`
        /// because the source list lives on a trait object whose
        /// lifetime is the executor frame, not `'static`.
        language: String,
        rendered_errors: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workdir_access_renders_help_for_absolute_path() {
        let e = ToolError::WorkdirAccess(
            "absolute path not allowed: /home/user/lights_out.py".into(),
        );
        let s = e.render_for_agent();
        assert!(s.contains("error: workdir access violation"));
        assert!(s.contains("reason:"));
        assert!(s.contains("help:"));
        // Help mentions the basename as the suggested fix.
        assert!(s.contains("lights_out.py"), "render: {s}");
    }

    #[test]
    fn workdir_access_renders_help_for_parent_traversal() {
        let e = ToolError::WorkdirAccess(
            "parent-dir traversal not allowed: ../etc/passwd".into(),
        );
        let s = e.render_for_agent();
        assert!(s.contains("workdir access violation"));
        assert!(s.contains("help:"));
    }

    #[test]
    fn timeout_renders_with_secs() {
        let e = ToolError::Timeout {
            primitive: "build",
            secs: 120,
        };
        let s = e.render_for_agent();
        assert!(s.contains("`build`"));
        assert!(s.contains("120s"));
        assert!(s.contains("help:"));
    }

    #[test]
    fn invalid_args_renders_help_about_schema() {
        let e = ToolError::InvalidArguments {
            primitive: "write_file",
            reason: "missing required field `content`".into(),
        };
        let s = e.render_for_agent();
        assert!(s.contains("`write_file`"));
        assert!(s.contains("missing required field"));
        assert!(s.contains("parameter schema"));
    }

    #[test]
    fn syntax_rejected_renders_with_language_and_help() {
        let e = ToolError::SyntaxRejected {
            primitive: "write_file",
            language: "Python".to_string(),
            rendered_errors: "error: invalid syntax\n  --> lights_out.py:83:5\n   |\n83 |     let me redo Gaussian elimination more carefully.\n   |     ^".into(),
        };
        let s = e.render_for_agent();
        assert!(s.contains("pre-write syntax check rejected"));
        assert!(s.contains("(Python)"));
        assert!(s.contains("--> lights_out.py:83"));
        assert!(s.contains("let me redo Gaussian elimination"));
        // Help must say the disk wasn't touched and tell the model
        // to re-emit cleanly. If a future PR softens this language,
        // the model may interpret the failure as a hard write error
        // and try a different filename instead of fixing content.
        assert!(s.contains("NOT written to disk"));
        assert!(s.contains("do not include reasoning, narration"));
    }
}

impl ToolError {
    /// Render in cargo-output texture for the model's chat history.
    /// The point: the model is the consumer of this string, so the
    /// format should mimic the compiler-error shape it's already
    /// trained to read — what was tried, why it failed, what to do
    /// instead. The bare `Display` impl above produces a single
    /// terse line; this method produces a multi-line block with a
    /// help: suggestion when possible.
    ///
    /// Per ARCH §0.1 (glassbox): model-to-model communication is
    /// itself part of the system surface that must be legible.
    /// Opaque enum names produce confused retries; cargo-shape
    /// errors produce targeted fixes.
    pub fn render_for_agent(&self) -> String {
        match self {
            ToolError::InvalidArguments { primitive, reason } => format!(
                "error: invalid arguments for `{primitive}`\n  \
                 = reason: {reason}\n  \
                 = help: re-emit the call with arguments matching the \
                 tool's parameter schema. Check that all required \
                 fields are present and types match."
            ),
            ToolError::WorkdirAccess(detail) => {
                // Heuristic suggestion: pick the bit of `detail` that
                // names a path and propose a workdir-relative form.
                let mut help = String::from(
                    "use a workdir-relative path (e.g. `lights_out.py` \
                     for a file at the workdir root, or `src/lib.rs` \
                     for a nested file). Absolute paths and `..` \
                     traversals are not permitted in the workdir \
                     sandbox.",
                );
                // Try to extract the offending path from messages like
                // "absolute path not allowed: /home/user/lights_out.py"
                // or "parent-dir traversal not allowed: ../foo".
                if let Some(colon_idx) = detail.rfind(": ") {
                    let path = detail[colon_idx + 2..].trim();
                    if !path.is_empty() && path != "(none)" {
                        // Suggest the basename as the trivial fix when
                        // an absolute path was emitted with a filename.
                        let basename = std::path::Path::new(path)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned());
                        if let Some(name) = basename {
                            if path != name {
                                help = format!(
                                    "use a workdir-relative path. The \
                                     workdir is sandboxed — absolute \
                                     paths and `..` traversals are \
                                     rejected. For the path you \
                                     emitted (`{path}`), try \
                                     `{name}` if you want it at the \
                                     workdir root."
                                );
                            }
                        }
                    }
                }
                format!(
                    "error: workdir access violation\n  \
                     = reason: {detail}\n  \
                     = help: {help}"
                )
            }
            ToolError::Filesystem { primitive, reason } => format!(
                "error: filesystem operation failed in `{primitive}`\n  \
                 = reason: {reason}\n  \
                 = help: verify the path exists and is readable/\
                 writable. If you're writing a nested file, the \
                 parent directory will be created automatically."
            ),
            ToolError::Subprocess { primitive, reason } => format!(
                "error: subprocess failure in `{primitive}`\n  \
                 = reason: {reason}\n  \
                 = help: this is a host-side issue (process spawn or \
                 wait failed). If repeated, the daemon environment may \
                 be missing a required binary."
            ),
            ToolError::Timeout { primitive, secs } => format!(
                "error: subprocess timed out in `{primitive}` after {secs}s\n  \
                 = reason: the build or test command did not complete \
                 within the per-call wall budget.\n  \
                 = help: if the work is genuinely long-running, consider \
                 breaking it into smaller steps; otherwise this likely \
                 indicates a hang in the spawned process."
            ),
            ToolError::SyntaxRejected { primitive, language, rendered_errors } => format!(
                "error: pre-write syntax check rejected `{primitive}` ({language})\n\
                 {rendered_errors}\n  \
                 = help: re-emit `write_file` with a corrected `content` field. \
                 The file was NOT written to disk — your next write_file call \
                 starts from the same state as before. The `content` field must \
                 be valid {language} source code; do not include reasoning, \
                 narration, or English sentences outside of comments/docstrings."
            ),
        }
    }
}
