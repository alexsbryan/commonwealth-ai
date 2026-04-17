//! `lint_status` — return the current state of the background lint runner.
//!
//! Groups errors by file and separates warnings from failures. Cheap —
//! reads from a local SQLite cache, never triggers a run.
//!
//! ## When to call
//!
//! - Before every commit.
//! - Constantly during active editing — lint is fast (seconds, not minutes)
//!   and this tool costs microseconds.
//! - After any structural change (new imports, type changes) to catch type
//!   errors before wasting time on test runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::lint_results::LintResultStore;

pub struct LintStatusTool {
    store: Arc<LintResultStore>,
    /// The command the watcher runs, e.g. "cargo check --workspace". Passed
    /// through to the response so agents can confirm scope coverage.
    watched_scope: Option<String>,
    /// Shared with the watcher coordinator — true while the FS watcher is live.
    watcher_active: Option<Arc<AtomicBool>>,
}

impl LintStatusTool {
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
impl Tool for LintStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "lint_status".to_string(),
            name: "Lint Status".to_string(),
            description: "Return lint/type-check status from the background watcher. \
                          NEVER run `cargo check` or `cargo build` via Bash — the watcher \
                          holds the Cargo file lock continuously; running cargo check \
                          alongside it causes BOTH processes to stall indefinitely waiting \
                          for the lock. This call reads cached results in microseconds with \
                          zero contention. \
                          Response fields to trust the result: \
                          `age_seconds` — how old the result is (typically < 30s after an \
                          edit; if large, the watcher may have been idle); \
                          `watched_scope` — the exact command the watcher runs (e.g. \
                          'cargo check --workspace'), confirming which crates are covered; \
                          `watcher_active` — true = watcher is live and will pick up your \
                          next save automatically; false = watcher not running, only then \
                          fall back to Bash. \
                          Status: 'fresh_passing' (clean, age_seconds shows recency), \
                          'fresh_failing' (errors in response), 'stale' (files changed \
                          since last run — watcher will rerun automatically on next save), \
                          'running' (in progress — check again in ~15s), \
                          'never_run' (watcher not configured — fall back to Bash)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You've edited one or more files and want to know if the code compiles. Do NOT run `cargo check` or `cargo build` — that fights the background watcher for the Cargo file lock and blocks both processes. This reads the watcher's cached result instantly with no contention.".into(),
                    call: serde_json::json!({}),
                },
            ],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let watcher_active = self
            .watcher_active
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(false);

        let is_running = self.store.run_in_progress().await.unwrap_or(false);

        if is_running {
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
            tool_id: "lint_status".to_string(),
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

        let stale = self
            .store
            .stale_files_since_last_run()
            .await
            .unwrap_or_default();

        let status = if !stale.is_empty() {
            "stale"
        } else if run.passed() {
            "fresh_passing"
        } else {
            "fresh_failing"
        };

        let errors: Vec<_> = self
            .store
            .latest_failures(50)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|f| {
                json!({
                    "file": f.file,
                    "output": f.output,
                    "output_truncated": f.output_truncated,
                    "line": f.line,
                    "col": f.col,
                    "run_id": f.run_id
                })
            })
            .collect();

        let warnings: Vec<_> = self
            .store
            .latest_warnings(50)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|w| {
                json!({
                    "file": w.file,
                    "output": w.output,
                    "line": w.line,
                    "col": w.col
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
                "run_id": run.run_id,
                "pass_count": run.pass_count,
                "fail_count": run.fail_count,
                "warn_count": run.warn_count,
                "elapsed_ms": run.elapsed_ms,
            },
            "errors": errors,
            "warnings": warnings,
            "stale_since": stale_paths,
            "age_seconds": age_seconds,
            "watched_scope": self.watched_scope,
            "watcher_active": watcher_active,
        })))
    }
}
