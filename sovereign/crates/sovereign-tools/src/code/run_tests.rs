//! `run_tests` — trigger the test suite to run immediately.
//!
//! Cancels any in-progress run and starts a fresh one. Does not wait for the
//! run to complete — returns immediately. Use `test_status` to poll progress.
//!
//! ## When to call
//!
//! - When `test_status` reports `stale` and you need a fresh result before
//!   proceeding (e.g. before a commit review).
//! - When the watcher hasn't caught up because a file was changed outside the
//!   watched root.
//! - Not needed when the watcher is active and files have been changed — it
//!   will already have triggered a run.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::Result;
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::update::test_watcher::TestWatcher;

pub struct RunTestsTool {
    watcher: Arc<TestWatcher>,
}

impl RunTestsTool {
    pub fn new(watcher: Arc<TestWatcher>) -> Self {
        Self { watcher }
    }
}

#[async_trait]
impl Tool for RunTestsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "run_tests".to_string(),
            name: "Run Tests".to_string(),
            description: "Trigger the test suite to run immediately, cancelling any in-progress run. \
                          Returns immediately — use test_status to poll for results. \
                          Prefer this over waiting for the file watcher when you need \
                          a guaranteed fresh run (e.g. before a commit review, or when \
                          tests failed and you've made a fix)."
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
        self.watcher.force_run().await;
        Ok(StepOutput::Json(json!({
            "status": "started",
            "message": "Test run started. Use test_status to poll for results."
        })))
    }
}
