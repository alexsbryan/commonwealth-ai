// SPDX-License-Identifier: AGPL-3.0-or-later
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

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_watchers::LintResultStore;
use corpus_engine_watchers::WatcherHeartbeat;

use super::watcher_health::{
    apply_liveness, assess, read_legacy, watcher_json, WatcherHealthInputs,
};
use sovereign_core::tool_manifest::DeclaredTool;

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
    /// Legacy one-shot liveness bool. Superseded by `heartbeat`.
    watcher_active: Option<Arc<AtomicBool>>,
    /// Shared coordinator heartbeat — authoritative liveness signal. See
    /// [`super::watcher_health`].
    heartbeat: Option<Arc<WatcherHeartbeat>>,
}

impl BuildTool {
    pub fn new(store: Arc<LintResultStore>) -> Self {
        Self {
            store,
            watched_scope: None,
            watcher_active: None,
            heartbeat: None,
        }
    }

    pub fn with_watched_scope(mut self, scope: String) -> Self {
        self.watched_scope = Some(scope);
        self
    }

    pub fn with_watcher_active(mut self, flag: Arc<AtomicBool>) -> Self {
        self.watcher_active = Some(flag);
        self
    }

    /// Attach the coordinator heartbeat. Preferred over
    /// [`with_watcher_active`](Self::with_watcher_active).
    pub fn with_heartbeat(mut self, heartbeat: Arc<WatcherHeartbeat>) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }
}

impl BuildTool {
    /// Bind this tool's state to its `build` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("build", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_signal({
            let state = Arc::clone(&state);
            Arc::new(move || {
                let state = Arc::clone(&state);
                Box::pin(async move { state.signal_now().await })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>
            })
        })
    }

    /// The executable half of `build`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let full = params
            .get("full")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let legacy_active = read_legacy(&self.watcher_active);
        let configured = self.watched_scope.is_some();

        // In-progress short-circuit. Same shape as LintStatusTool.
        if self.store.run_in_progress().await.unwrap_or(false) {
            let reason = assess(&WatcherHealthInputs {
                heartbeat: self.heartbeat.as_ref(),
                legacy_active,
                configured,
                run_in_progress: true,
                last_run_age_secs: None,
            });
            return Ok(StepOutput::Json(json!({
                "status": "running",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": reason.is_live(),
                "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "build".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
            let reason = assess(&WatcherHealthInputs {
                heartbeat: self.heartbeat.as_ref(),
                legacy_active,
                configured,
                run_in_progress: false,
                last_run_age_secs: None,
            });
            return Ok(StepOutput::Json(json!({
                "status": "never_run",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": reason.is_live(),
                "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
            })));
        };

        let age_seconds = SystemTime::now()
            .duration_since(run.finished_at)
            .unwrap_or_default()
            .as_secs();

        let stale = self
            .store
            .stale_files_since_last_run()
            .await
            .unwrap_or_default();

        let raw_status = if !stale.is_empty() {
            "stale"
        } else if run.passed() {
            "fresh_passing"
        } else {
            "fresh_failing"
        };

        let reason = assess(&WatcherHealthInputs {
            heartbeat: self.heartbeat.as_ref(),
            legacy_active,
            configured,
            run_in_progress: false,
            last_run_age_secs: Some(age_seconds),
        });
        let status = apply_liveness(raw_status, reason);

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
                let needs_truncation = !full && raw_output.len() > DEFAULT_OUTPUT_BYTES_PER_ERROR;
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
            "watcher_active": reason.is_live(),
            "watcher": watcher_json(reason, self.heartbeat.as_ref(), configured),
        })))
    }

    async fn signal_now(&self) -> Option<String> {
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
