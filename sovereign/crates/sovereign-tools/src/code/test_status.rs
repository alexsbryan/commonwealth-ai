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
}

impl TestStatusTool {
    pub fn new(store: Arc<TestResultStore>) -> Self {
        Self {
            store,
            running_flag: None,
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
}

#[async_trait]
impl Tool for TestStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "test_status".to_string(),
            name: "Test Status".to_string(),
            description: "Return the current state of the background test runner. \
                          Cheap — reads from a local SQLite cache, never triggers a run. \
                          Call this before claiming a change is correct, before committing, \
                          or any time you need to know whether tests are passing. \
                          If status is 'stale', files have changed since the last run — \
                          call run_tests to force a fresh run."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            examples: vec![],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, _params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
                "stale_since": []
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
                "stale_since": []
            })));
        };

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
            "stale_since": stale_paths
        })))
    }
}
