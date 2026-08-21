// SPDX-License-Identifier: AGPL-3.0-or-later

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;
use std::sync::Arc;
use sovereign_core::tool_manifest::DeclaredTool;

/// Sandboxed Python code execution tool.
pub struct ComputeTool;

impl ComputeTool {
    /// Bind this tool's state to its `compute` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("compute", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `compute`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let code = params
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'code' parameter".to_string()))?;

        // Write code to a temporary file.
        let temp_dir = std::env::temp_dir().join("sovereign-compute");
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(|e| Error::Execution(format!("Failed to create temp dir: {e}")))?;

        let script_path = temp_dir.join(format!("{}.py", uuid::Uuid::new_v4()));
        tokio::fs::write(&script_path, code)
            .await
            .map_err(|e| Error::Execution(format!("Failed to write script: {e}")))?;

        // Execute with timeout.
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new("python3")
                .arg(&script_path)
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .output(),
        )
        .await
        .map_err(|_| Error::Execution("Python execution timed out (30s limit)".to_string()))?
        .map_err(|e| Error::Execution(format!("Failed to run Python: {e}")))?;

        // Clean up.
        let _ = tokio::fs::remove_file(&script_path).await;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            if stderr.is_empty() {
                Ok(StepOutput::Text(stdout))
            } else {
                Ok(StepOutput::Text(format!("{stdout}\n\n[stderr]\n{stderr}")))
            }
        } else {
            Ok(StepOutput::Text(format!(
                "[exit code: {}]\n{stderr}\n{stdout}",
                output.status.code().unwrap_or(-1)
            )))
        }
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {

        if params.get("code").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Compute requires a 'code' string parameter".to_string(),
            ));
        }
        Ok(())
    }
}
