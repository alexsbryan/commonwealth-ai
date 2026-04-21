//! `test_status` — return the current state of the background test runner.
//!
//! Reads from [`TestResultStore`] — never triggers a run itself. The watcher
//! runs automatically on file changes; this tool gives an agent an instant,
//! cheap snapshot without waiting for a run to complete.
//!
//! ## When to call
//!
//! - Before claiming a change is correct. If status is `fresh + passing`, the
//!   tests you care about have already run on the current file state.
//! - Before committing. `stale` status means files changed since the last run.
//! - Constantly during active editing — this call costs microseconds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::test_results::TestResultStore;

pub struct TestStatusTool {
    store: Arc<TestResultStore>,
    /// Optional handle to the watcher for live "Running" status.
    /// If None, status is derived entirely from the store.
    running_flag: Option<Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>>,
    /// The command the watcher runs, e.g. "cargo test --workspace". Passed
    /// through to the response so agents can confirm scope coverage.
    watched_scope: Option<String>,
    /// Shared with the watcher coordinator — true while the FS watcher is live.
    watcher_active: Option<Arc<AtomicBool>>,
}

impl TestStatusTool {
    pub fn new(store: Arc<TestResultStore>) -> Self {
        Self {
            store,
            running_flag: None,
            watched_scope: None,
            watcher_active: None,
        }
    }

    /// Attach the watcher's run_handle so we can report "Running" accurately.
    pub fn with_run_handle(
        mut self,
        handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ) -> Self {
        self.running_flag = Some(handle);
        self
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
impl Tool for TestStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "test_status".to_string(),
            name: "Test Status".to_string(),
            description: "Return test suite status from the background watcher. \
                          NEVER run `cargo test` via Bash — the watcher holds the Cargo \
                          file lock continuously; running cargo test alongside it causes \
                          BOTH processes to stall indefinitely waiting for the lock. \
                          This call reads cached results in microseconds with zero contention. \
                          Response fields to trust the result: \
                          `age_seconds` — how old the result is (if 0-60s, the watcher \
                          ran on your current changes); \
                          `watched_scope` — the exact command the watcher runs (e.g. \
                          'cargo test --workspace'), confirming which crates are covered; \
                          `watcher_active` — true = watcher is live and will rerun on your \
                          next save automatically; false = watcher not running, only then \
                          fall back to Bash. \
                          Status: 'fresh_passing' (all pass — safe to proceed), \
                          'fresh_failing' (failures in response), 'stale' (files changed \
                          since last run — call run_tests then poll), 'running' (in \
                          progress — check again in 30-60s), 'never_run' (watcher not \
                          configured — fall back to Bash)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You've made a change and want to know if tests pass. Do NOT run `cargo test` — it contends with the background watcher for the Cargo file lock. This reads the watcher's result instantly. If status is 'stale', call run_tests to force a fresh run, then poll back here in ~30s.".into(),
                    call: serde_json::json!({}),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "status":          { "type": "string", "enum": ["fresh_passing","fresh_failing","stale","running","never_run"] },
                    "age_seconds":     { "type": "integer" },
                    "pass_count":      { "type": "integer" },
                    "fail_count":      { "type": "integer" },
                    "watcher_active":  { "type": "boolean" },
                    "watched_scope":   { "type": "string" },
                    "failures":        { "type": "array" },
                    "run_id":          { "type": "integer" },
                    "output_truncated":{ "type": "boolean" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Signal: one-liner when the last test run failed. Silent on a
    /// clean run. SQLite point lookup — no extra work.
    async fn signal(&self) -> Option<String> {
        let summary = self.store.latest_run().await.ok().flatten()?;
        if summary.passed() {
            return None;
        }
        let age = summary
            .finished_at
            .elapsed()
            .ok()
            .map(|d| format!(" age {}s", d.as_secs()))
            .unwrap_or_default();
        Some(format!(
            "last test run: {} passed, {} failed{age}",
            summary.pass_count, summary.fail_count
        ))
    }

    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let watcher_active = self
            .watcher_active
            .as_ref()
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(false);

        // Check running flag.
        let is_running = if let Some(ref flag) = self.running_flag {
            flag.lock()
                .await
                .as_ref()
                .map(|h| !h.is_finished())
                .unwrap_or(false)
        } else {
            self.store.run_in_progress().await.unwrap_or(false)
        };

        if is_running {
            return Ok(StepOutput::Json(json!({
                "status": "running",
                "summary": null,
                "failures": [],
                "stale_since": [],
                "age_seconds": null,
                "watched_scope": self.watched_scope,
                "watcher_active": watcher_active,
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "test_status".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
            return Ok(StepOutput::Json(json!({
                "status": "never_run",
                "summary": null,
                "failures": [],
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

        let failures = self
            .store
            .latest_failures(10)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|f| {
                json!({
                    "name": f.name,
                    "output": f.output,
                    "output_truncated": f.output_truncated,
                    "run_id": f.run_id
                })
            })
            .collect::<Vec<_>>();

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
                "exit_code": run.exit_code,
            },
            "failures": failures,
            "stale_since": stale_paths,
            "age_seconds": age_seconds,
            "watched_scope": self.watched_scope,
            "watcher_active": watcher_active,
        })))
    }
}
