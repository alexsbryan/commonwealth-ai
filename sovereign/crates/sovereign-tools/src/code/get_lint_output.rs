//! `get_lint_output` — retrieve the full raw output of a lint run.
//!
//! `lint_status` truncates error output at 500 characters. Call this when
//! `output_truncated: true` and you need the complete compiler message.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::lint_results::LintResultStore;

pub struct GetLintOutputTool {
    store: Arc<LintResultStore>,
}

impl GetLintOutputTool {
    pub fn new(store: Arc<LintResultStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for GetLintOutputTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "get_lint_output".to_string(),
            name: "Get Lint Output".to_string(),
            description: "Retrieve the full raw output of a lint run by run_id. \
                          Call when lint_status shows output_truncated: true on an error. \
                          The run_id is in the lint_status summary."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "run_id": {
                        "type": "integer",
                        "description": "The run ID from lint_status summary."
                    }
                },
                "required": ["run_id"]
            }),
            examples: vec![],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("run_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::InvalidInput("get_lint_output requires 'run_id' (integer)".to_string()))?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
}
