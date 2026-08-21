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

use serde_json::json;

use sovereign_core::error::Result;
use sovereign_core::types::*;

use corpus_engine_watchers::TestWatcher;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct RunTestsTool {
    watcher: Arc<TestWatcher>,
}

impl RunTestsTool {
    pub fn new(watcher: Arc<TestWatcher>) -> Self {
        Self { watcher }
    }
}

impl RunTestsTool {
    /// Bind this tool's state to its `run_tests` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("run_tests", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `run_tests`.
    async fn run(
        &self,
        _params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        self.watcher.force_run().await;
        Ok(StepOutput::Json(json!({
            "status": "started",
            "message": "Test run started. Use test_status to poll for results."
        })))
    }
}
