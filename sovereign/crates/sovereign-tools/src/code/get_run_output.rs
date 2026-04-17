//! `get_run_output` — retrieve the full raw output of a test run.
//!
//! `test_status` truncates failure output at 4096 characters. Call this tool
//! when a failure's `output_truncated: true` and you need the full text. Pass
//! the `run_id` from the `test_status` summary.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::test_results::TestResultStore;

pub struct GetRunOutputTool {
    store: Arc<TestResultStore>,
}

impl GetRunOutputTool {
    pub fn new(store: Arc<TestResultStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for GetRunOutputTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "get_run_output".to_string(),
            name: "Get Run Output".to_string(),
            description: "Retrieve the full raw output of a test run by run_id. \
                          Call this when test_status shows output_truncated: true on a failure. \
                          The run_id is in the test_status summary."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "run_id": {
                        "type": "integer",
                        "description": "The run ID from test_status summary."
                    }
                },
                "required": ["run_id"]
            }),
            examples: vec![
                ToolExample {
                    situation: "test_status showed a failure but the output was truncated. Call this with the run_id from that response to get the full test output — panic message, assertion diff, and the exact line that failed.".into(),
                    call: serde_json::json!({ "run_id": 7 }),
                },
            ],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("run_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::InvalidInput("get_run_output requires 'run_id' (integer)".to_string()))?;
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
                tool_id: "get_run_output".to_string(),
                message: e.to_string(),
            }),
        }
    }
}
