//! `build` — single-call view of the workspace's compile/lint state.
//!
//! Demo shape (from the renamed-tool surface):
//!
//! ```text
//! build()                     // → status summary + top errors
//! build(full: true)           // → status + every error's full output
//! ```
//!
//! Why a new tool when [`crate::code::LintStatusTool`] already
//! exists? Two reasons:
//!
//! 1. **Demo ergonomics.** A panicked engineer asking the agent to
//!    "check the build" works better when the tool is named
//!    `build`. `lint_status` reads as observability plumbing
//!    rather than a daily affordance.
//! 2. **Single round-trip.** `lint_status` returns a status and a
//!    handful of error metadata; the agent typically follows up
//!    with `get_lint_output` for the actual error text. `build`
//!    folds those two calls into one — the default response
//!    already includes per-error output (truncated when long),
//!    and `full=true` returns untruncated output for each error.
//!
//! Internally `build` wraps the same [`LintResultStore`] as
//! `LintStatusTool` and reuses its query primitives — there is no
//! second copy of the watcher logic.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::lint_results::LintResultStore;

/// Maximum output bytes per error in the default (non-`full`)
/// response. Each error's `output` field is truncated to this with
/// an `output_truncated: true` marker so the agent can pass
/// `full=true` to get the rest. Picked to keep the default
/// response under ~4 KB even with 5 errors.
const DEFAULT_OUTPUT_BYTES_PER_ERROR: usize = 600;

/// Maximum number of errors and warnings included in the default
/// response. The store can hold many more (50 of each via
/// `latest_failures(50)`), but a chat-mode response only benefits
/// from the top few. `full=true` does not lift this — the agent
/// pages through `get_lint_output` for older errors when needed.
const DEFAULT_TOP_N: usize = 5;

pub struct BuildTool {
    store: Arc<LintResultStore>,
    /// Command the watcher runs (e.g. `cargo check --workspace`).
    /// Surfaced in the response so the agent can confirm scope.
    watched_scope: Option<String>,
    /// Shared with the watcher coordinator — true while the FS
    /// watcher is live. When `None` we report `watcher_active=true`
    /// (legacy behaviour: assume the watcher is running unless
    /// explicitly told otherwise).
    watcher_active: Option<Arc<AtomicBool>>,
}

impl BuildTool {
    pub fn new(store: Arc<LintResultStore>) -> Self {
        Self { store, watched_scope: None, watcher_active: None }
    }

    pub fn with_watched_scope(mut self, scope: String) -> Self {
        self.watched_scope = Some(scope);
        self
    }

    pub fn with_watcher_active(mut self, flag: Arc<AtomicBool>) -> Self {
        self.watcher_active = Some(flag);
        self
    }
}

#[async_trait]
impl Tool for BuildTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "build".to_string(),
            name: "Build".to_string(),
            description: "Return the workspace's compile/lint status from the background \
                          watcher: pass/fail summary, top 5 errors with their output, and \
                          freshness markers. NEVER run `cargo check` or `cargo build` via \
                          Bash — the watcher holds the Cargo file lock continuously; \
                          running cargo alongside it stalls both processes indefinitely. \
                          This tool reads the cached run in microseconds with zero \
                          contention. \
                          Pass `full: true` to receive untruncated output for each error \
                          (use sparingly — long failures balloon the response). \
                          Status: 'fresh_passing' (clean), 'fresh_failing' (errors in \
                          response), 'stale' (files changed since last run — watcher will \
                          rerun on next save), 'running' (in progress — check again in \
                          ~15s), 'never_run' (watcher not configured — only then fall \
                          back to Bash)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "full": {
                        "type": "boolean",
                        "default": false,
                        "description": "Include untruncated output for every error in the response."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You've edited one or more files and want to know if the code still compiles.".into(),
                    call: serde_json::json!({}),
                },
                ToolExample {
                    situation: "The default response truncated an error you need to read in full.".into(),
                    call: serde_json::json!({ "full": true }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "status":         { "type": "string", "enum": ["fresh_passing","fresh_failing","stale","running","never_run"] },
                    "age_seconds":    { "type": "integer" },
                    "summary":        { "type": "object" },
                    "errors":         { "type": "array" },
                    "warnings":       { "type": "array" },
                    "stale_since":    { "type": "array" },
                    "watcher_active": { "type": "boolean" },
                    "watched_scope":  { "type": "string" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Signal mirrors `LintStatusTool::signal` — a one-liner
    /// surfaced when the watcher shows failures or stale state.
    /// Silent when the last run was clean.
    async fn signal(&self) -> Option<String> {
        let status = self.store.latest_run().await.ok().flatten()?;
        if status.passed() && !self.store.has_stale_files().await.unwrap_or(false) {
            return None;
        }
        if !status.passed() {
            Some(format!(
                "build failing: {} errors, {} warnings (run #{})",
                status.fail_count, status.warn_count, status.run_id,
            ))
        } else {
            Some(format!(
                "build stale (files changed since run #{}; rerun pending)",
                status.run_id,
            ))
        }
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let full = params
            .get("full")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let watcher_active = self
            .watcher_active
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(true);

        // In-progress short-circuit. Same shape as LintStatusTool.
        if self.store.run_in_progress().await.unwrap_or(false) {
            return Ok(StepOutput::Json(json!({
                "status": "running",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": watcher_active,
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "build".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
            return Ok(StepOutput::Json(json!({
                "status": "never_run",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": watcher_active,
            })));
        };

        let age_seconds = SystemTime::now()
            .duration_since(run.finished_at)
            .unwrap_or_default()
            .as_secs();

        let stale = self.store.stale_files_since_last_run().await.unwrap_or_default();

        let status = if !stale.is_empty() {
            "stale"
        } else if run.passed() {
            "fresh_passing"
        } else {
            "fresh_failing"
        };

        // Top-N errors. The store sorts by recency / severity; we
        // take the prefix and truncate per-error output for the
        // default response. `LintResult.output` is
        // `Option<String>` — None means the row recorded a
        // diagnostic location but no captured stdout/stderr.
        let raw_errors = self
            .store
            .latest_failures(DEFAULT_TOP_N)
            .await
            .unwrap_or_default();

        let errors: Vec<_> = raw_errors
            .into_iter()
            .map(|f| {
                let raw_output = f.output.unwrap_or_default();
                let needs_truncation = !full
                    && raw_output.len() > DEFAULT_OUTPUT_BYTES_PER_ERROR;
                let output = if needs_truncation {
                    let cut = byte_truncate(&raw_output, DEFAULT_OUTPUT_BYTES_PER_ERROR);
                    format!("{cut}…")
                } else {
                    raw_output
                };
                json!({
                    "file": f.file,
                    "output": output,
                    "output_truncated": needs_truncation || f.output_truncated,
                    "line": f.line,
                    "col": f.col,
                    "run_id": f.run_id,
                })
            })
            .collect();

        let warnings: Vec<_> = self
            .store
            .latest_warnings(DEFAULT_TOP_N)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|w| {
                json!({
                    "file": w.file,
                    "output": w.output.unwrap_or_default(),
                    "line": w.line,
                    "col": w.col,
                })
            })
            .collect();

        let stale_paths: Vec<String> = stale
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        Ok(StepOutput::Json(json!({
            "status": status,
            "summary": {
                "run_id":      run.run_id,
                "pass_count":  run.pass_count,
                "fail_count":  run.fail_count,
                "warn_count":  run.warn_count,
                "elapsed_ms":  run.elapsed_ms,
            },
            "errors":   errors,
            "warnings": warnings,
            "stale_since":   stale_paths,
            "age_seconds":   age_seconds,
            "watched_scope": self.watched_scope,
            "watcher_active": watcher_active,
        })))
    }
}

/// Truncate a string to at most `max_bytes` while landing on a
/// UTF-8 codepoint boundary. Returns the truncated `&str` view —
/// the caller appends a continuation marker.
fn byte_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `byte_truncate` lands on a codepoint boundary even when the
    /// limit cuts mid-multi-byte-character. Otherwise we'd panic
    /// on the `&s[..cut]` slice.
    #[test]
    fn byte_truncate_lands_on_char_boundary() {
        // A string with a 4-byte codepoint right at the cut point.
        let s = "abc\u{1F600}xyz"; // 'abc' + 😀 (4 bytes) + 'xyz'
        assert_eq!(s.len(), 10);
        // Limit 4 → would land mid-😀; should back off to 3.
        let out = byte_truncate(s, 4);
        assert_eq!(out, "abc");
        // Limit 7 → 'abc' + 😀 = 7 bytes exactly, fits.
        let out = byte_truncate(s, 7);
        assert_eq!(out, "abc\u{1F600}");
        // Limit larger than input — return verbatim.
        let out = byte_truncate(s, 100);
        assert_eq!(out, s);
    }

    /// Empty input is preserved verbatim regardless of limit.
    #[test]
    fn byte_truncate_empty_input() {
        assert_eq!(byte_truncate("", 0), "");
        assert_eq!(byte_truncate("", 100), "");
    }
}
