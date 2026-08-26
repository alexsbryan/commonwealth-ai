// SPDX-License-Identifier: AGPL-3.0-or-later
//! `get_run_output` — retrieve the full raw output of a test run.
//!
//! `test_status` truncates failure output at 4096 characters. Call this tool
//! when a failure's `output_truncated: true` and you need the full text. Pass
//! the `run_id` from the `test_status` summary.

use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_watchers::TestResultStore;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct GetRunOutputTool {
    store: Arc<TestResultStore>,
}

impl GetRunOutputTool {
    pub fn new(store: Arc<TestResultStore>) -> Self {
        Self { store }
    }
}

impl GetRunOutputTool {
    /// Bind this tool's state to its `get_run_output` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("get_run_output", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `get_run_output`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let run_id = params
            .get("run_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::InvalidInput("missing 'run_id'".to_string()))?;

        match self.store.raw_output(run_id).await {
            Ok(Some(output)) => Ok(StepOutput::Json(json!({
                "run_id": run_id,
                "output": output
            }))),
            Ok(None) => Ok(StepOutput::Json(json!({
                "run_id": run_id,
                "output": null,
                "error": "Run not found or no output stored."
            }))),
            Err(e) => Err(Error::Tool {
                tool_id: "get_run_output".to_string(),
                message: e.to_string(),
            }),
        }
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {

        params
            .get("run_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                Error::InvalidInput("get_run_output requires 'run_id' (integer)".to_string())
            })?;
        Ok(())
    }
}
