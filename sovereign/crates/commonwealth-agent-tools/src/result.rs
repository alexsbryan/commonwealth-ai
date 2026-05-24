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
    /// `write_file` was rejected because the target file already
    /// exists with more lines than the structural threshold for
    /// full-file rewrites. The model is being directed to
    /// `patch_file` with small line ranges instead. Closes the
    /// "long-output token-level corruption" class observed on
    /// 4.2-mini-evaluator-python (2026-05-23): generating 5000+
    /// tokens of valid Python in one shot accumulates errors
    /// (drift into JS syntax, lost whitespace, escape confusion).
    /// `patch_file` with ≤30-line ranges sidesteps the regime
    /// where this corruption emerges.
    #[error("write_file rejected: {path} has {existing_lines} lines (> {threshold}); use patch_file")]
    WriteFileTooLarge {
        path: String,
        existing_lines: usize,
        threshold: usize,
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
    fn sticky_signature_collapses_varying_inputs_on_same_site() {
        // v-darwin 4.2 pattern (2026-05-24): the model emitted 6
        // consecutive replace_function attempts that each rendered
        // SLIGHTLY different `new_body` text but all failed with
        // `expected ':' at evaluator.py:94:25`. The old input-hash
        // detector saw 6 different hashes and never fired. The
        // signature must be IDENTICAL across these attempts so the
        // bench harness's consecutive-equal counter can trip.
        let a = ToolError::SyntaxRejected {
            primitive: "replace_function",
            language: "Python".to_string(),
            rendered_errors: "error: expected ':'\n  --> evaluator.py:94:25\n   |\n94 |     if foo bar\n   |                         ^".into(),
        };
        let b = ToolError::SyntaxRejected {
            primitive: "replace_function",
            language: "Python".to_string(),
            rendered_errors: "error: expected ':'\n  --> evaluator.py:94:25\n   |\n94 |     if quux baz\n   |                         ^\n   = note: while parsing if statement".into(),
        };
        assert_eq!(a.sticky_signature(), b.sticky_signature());
        // And the signature must INCLUDE the site so two different
        // sites produce different signatures (the model recovering at
        // line 94 and then failing at line 110 should reset the
        // counter).
        let c = ToolError::SyntaxRejected {
            primitive: "replace_function",
            language: "Python".to_string(),
            rendered_errors: "error: invalid syntax\n  --> evaluator.py:110:5".into(),
        };
        assert_ne!(a.sticky_signature(), c.sticky_signature());
    }

    #[test]
    fn sticky_signature_differs_across_variants() {
        // A SyntaxRejected and a Timeout should never coalesce — they
        // are different failure classes even if they happen back to
        // back. Pins that the variant tag is part of the signature.
        let a = ToolError::SyntaxRejected {
            primitive: "write_file",
            language: "Python".to_string(),
            rendered_errors: "error: invalid syntax\n  --> foo.py:1:1".into(),
        };
        let b = ToolError::Timeout {
            primitive: "write_file",
            secs: 120,
        };
        assert_ne!(a.sticky_signature(), b.sticky_signature());
    }

    #[test]
    fn sticky_signature_handles_missing_site_gracefully() {
        // If render_errors doesn't contain a parseable file:line, the
        // signature must still be stable (so two such rejections
        // collapse) and must NOT panic.
        let a = ToolError::SyntaxRejected {
            primitive: "patch_file",
            language: "Rust".to_string(),
            rendered_errors: "error: weird unparseable thing happened".into(),
        };
        let b = ToolError::SyntaxRejected {
            primitive: "patch_file",
            language: "Rust".to_string(),
            rendered_errors: "error: weird unparseable thing happened (slightly different prose)".into(),
        };
        assert_eq!(a.sticky_signature(), b.sticky_signature());
        assert!(a.sticky_signature().contains("<unknown>"));
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
            ToolError::WriteFileTooLarge { path, existing_lines, threshold } => format!(
                "error: write_file rejected for large existing file\n  \
                 = reason: `{path}` already has {existing_lines} lines, above the {threshold}-line \
                 threshold for full-file rewrites. Empirically the model accumulates \
                 token-level corruption (spacing drift, escape confusion, wrong-language \
                 syntax) when generating 5000+ tokens of valid source in one shot.\n  \
                 = help: use `patch_file` instead. Pick a tight line range (≤ 30 lines) \
                 around the buggy region, identify it from the line-numbered source \
                 anchor at the top of this message, and replace only that block. \
                 For the initial author of a NEW file, write_file is still the \
                 right tool — this rejection only fires for existing large files."
            ),
        }
    }

    /// Stable fingerprint used by the bench harness's sticky-retry
    /// detector. The signature must be CONTENT-INVARIANT — two
    /// different attempts that fail the SAME way should produce the
    /// SAME signature, even if the `new_body` or `new_content` text
    /// varies. The v-darwin run (4.2-mini-evaluator, 2026-05-24)
    /// showed 6 consecutive `expected ':' at evaluator.py:94:25`
    /// rejections with subtly different inputs each time — the old
    /// args-hash detector never fired because the inputs varied. The
    /// signature-based detector fires correctly because the REJECTION
    /// is identical.
    ///
    /// For `SyntaxRejected`, the signature extracts the first
    /// `file:line:col` triple from `rendered_errors` (the error
    /// SITE — where the broken character was found), discarding the
    /// rest of the prose. For other variants, the signature is the
    /// variant tag plus the primitive name (when present).
    pub fn sticky_signature(&self) -> String {
        match self {
            ToolError::SyntaxRejected { primitive, language, rendered_errors } => {
                let site = extract_file_line_col(rendered_errors)
                    .unwrap_or_else(|| "<unknown>".to_string());
                format!("SyntaxRejected:{primitive}:{language}:{site}")
            }
            ToolError::Timeout { primitive, .. } => format!("Timeout:{primitive}"),
            ToolError::InvalidArguments { primitive, reason } => {
                format!("InvalidArguments:{primitive}:{}", first_line(reason))
            }
            ToolError::WorkdirAccess(detail) => {
                format!("WorkdirAccess:{}", first_line(detail))
            }
            ToolError::Filesystem { primitive, reason } => {
                format!("Filesystem:{primitive}:{}", first_line(reason))
            }
            ToolError::Subprocess { primitive, reason } => {
                format!("Subprocess:{primitive}:{}", first_line(reason))
            }
            ToolError::WriteFileTooLarge { path, .. } => {
                format!("WriteFileTooLarge:{path}")
            }
        }
    }
}

/// Extract the first `path:line:col` triple from a rendered error
/// block. Matches the `path.ext:NN:MM` shape produced by pyflakes,
/// rustc, and our own `SyntaxValidator::render_errors`.
fn extract_file_line_col(s: &str) -> Option<String> {
    // Look for "<word>.<ext>:N:M" or "<word>.<ext>:N" anywhere in s.
    // Walk byte-by-byte rather than pulling in a regex dep.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find a likely path char run (alnum, _, -, ., /).
        let start = i;
        while i < bytes.len() && is_path_char(bytes[i]) {
            i += 1;
        }
        let path = &s[start..i];
        // Must contain a '.' (extension) and not be all dots.
        if path.contains('.') && path.bytes().any(|b| b != b'.') && i < bytes.len() && bytes[i] == b':' {
            // Optional :line[:col] suffix.
            let mut j = i + 1;
            let line_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > line_start {
                let mut suffix = format!("{path}:{}", &s[line_start..j]);
                if j < bytes.len() && bytes[j] == b':' {
                    let col_start = j + 1;
                    let mut k = col_start;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k > col_start {
                        suffix.push(':');
                        suffix.push_str(&s[col_start..k]);
                    }
                }
                return Some(suffix);
            }
        }
        if i == start {
            i += 1;
        }
    }
    None
}

fn is_path_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'/'
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}
