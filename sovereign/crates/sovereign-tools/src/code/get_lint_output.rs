// SPDX-License-Identifier: AGPL-3.0-or-later
//! `get_lint_output` — retrieve the full raw output of a lint run.
//!
//! `lint_status` truncates error output at 500 characters. Call this when
//! `output_truncated: true` and you need the complete compiler message.

use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_watchers::LintResultStore;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct GetLintOutputTool {
    store: Arc<LintResultStore>,
}

impl GetLintOutputTool {
    pub fn new(store: Arc<LintResultStore>) -> Self {
        Self { store }
    }
}

impl GetLintOutputTool {
    /// Bind this tool's state to its `get_lint_output` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("get_lint_output", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `get_lint_output`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
                tool_id: "get_lint_output".to_string(),
                message: e.to_string(),
            }),
        }
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("run_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                Error::InvalidInput("get_lint_output requires 'run_id' (integer)".to_string())
            })?;
        Ok(())
    }
}
