// SPDX-License-Identifier: AGPL-3.0-or-later
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

use corpus_engine_watchers::TestWatcher;

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
            description: "Force an immediate test run, cancelling any in-progress run. \
                          Returns immediately — do NOT block waiting for results. \
                          Call test_status after ~30-60s to check progress; call again \
                          until status is no longer 'running'. Only needed when test_status \
                          is 'stale' and you need a guaranteed fresh result (before a \
                          commit, after a targeted fix). The file watcher triggers runs \
                          automatically on changes — run_tests is only for when you want \
                          to force ahead of the watcher's debounce."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "test_status returned 'stale' and you need a guaranteed fresh result before proceeding. Trigger this, then wait ~30-60s and poll test_status. Do NOT run `cargo test` directly.".into(),
                    call: serde_json::json!({}),
                },
            ],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "triggered": { "type": "boolean" },
                    "run_id":    { "type": "integer" },
                    "message":   { "type": "string" }
                }
            })),
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
