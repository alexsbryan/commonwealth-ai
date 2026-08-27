// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

/// Execute shell commands in a sandboxed subprocess.
/// Always requires Shell permission and per-action approval.
pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    /// Governed by the `shell` switch — the operator approves each command, so a surface that lets someone switch it off must be obeyed.
    fn family(&self) -> Option<sovereign_contracts::tool_bundle::ToolFamily> {
        Some(sovereign_contracts::tool_bundle::ToolFamily::Shell)
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "shell".to_string(),
            name: "Shell".to_string(),
            description: "Execute a shell command and return its output".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }),
            examples: vec![],
            effect: Effect::ReadWrite,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Slow,
            scope: Scope::Session,
            // Shell output is whatever the command produces; shape
            // depends entirely on the command. Downstream steps can
            // pipe the full text via `{N.output}` or route through
            // reasoning — no structured keys to reference.
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Shell]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if params.get("command").and_then(|v| v.as_str()).is_none() {
            return Err(Error::InvalidInput(
                "Shell tool requires a 'command' string parameter".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'command' parameter".to_string()))?;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        // Scope to working directory if set.
        if let Some(ref dir) = ctx.working_directory {
            cmd.current_dir(dir);
        }

        let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
            .await
            .map_err(|_| Error::Execution("Shell command timed out after 30 seconds".to_string()))?
            .map_err(|e| Error::Execution(format!("Failed to execute command: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let result = if output.status.success() {
            if stderr.is_empty() {
                stdout.to_string()
            } else {
                format!("{stdout}\n[stderr]: {stderr}")
            }
        } else {
            let code = output.status.code().unwrap_or(-1);
            format!("[exit code {code}]\n{stdout}\n[stderr]: {stderr}")
        };

        Ok(StepOutput::Text(result.trim().to_string()))
    }
}
