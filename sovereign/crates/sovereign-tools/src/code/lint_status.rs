//! `lint_status` — return the current state of the background lint runner.
//!
//! Groups errors by file and separates warnings from failures. Cheap —
//! reads from a local SQLite cache, never triggers a run.
//!
//! ## When to call
//!
//! - Before every commit.
//! - Constantly during active editing — lint is fast (seconds, not minutes)
//!   and this tool costs microseconds.
//! - After any structural change (new imports, type changes) to catch type
//!   errors before wasting time on test runs.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::lint_results::LintResultStore;

pub struct LintStatusTool {
    store: Arc<LintResultStore>,
}

impl LintStatusTool {
    pub fn new(store: Arc<LintResultStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for LintStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "lint_status".to_string(),
            name: "Lint Status".to_string(),
            description: "PREFERRED OVER `cargo check`. Return the current lint/type-check \
                          status. Reads from a local SQLite cache — instant, zero contention. \
                          The background watcher runs cargo check automatically on every file \
                          change; by the time you finish an edit the result is often already \
                          here. Do NOT run cargo check via Bash — it will contend with the \
                          watcher for the Cargo lock and block both. Errors are pre-grouped by \
                          file and included in this response; call get_lint_output only when \
                          output_truncated is true. Status values: 'fresh_passing' (clean), \
                          'fresh_failing' (errors in response), 'stale' (watcher queued — \
                          call again in ~15s), 'running' (in progress — call again shortly), \
                          'never_run' (watcher not configured — fall back to Bash)."
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
        let is_running = self.store.run_in_progress().await.unwrap_or(false);

        if is_running {
            return Ok(StepOutput::Json(json!({
                "status": "running",
                "summary": null,
                "errors": [],
                "warnings": [],
                "stale_since": []
            })));
        }

        let latest = self.store.latest_run().await.map_err(|e| Error::Tool {
            tool_id: "lint_status".to_string(),
            message: e.to_string(),
        })?;

        let Some(run) = latest else {
            return Ok(StepOutput::Json(json!({
                "status": "never_run",
                "summary": null,
                "errors": [],
                "warnings": [],
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
            "fresh"
        } else {
            "fresh_failing"
        };

        let errors: Vec<_> = self
            .store
            .latest_failures(50)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|f| {
                json!({
                    "file": f.file,
                    "output": f.output,
                    "output_truncated": f.output_truncated,
                    "line": f.line,
                    "col": f.col,
                    "run_id": f.run_id
                })
            })
            .collect();

        let warnings: Vec<_> = self
            .store
            .latest_warnings(50)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|w| {
                json!({
                    "file": w.file,
                    "output": w.output,
                    "line": w.line,
                    "col": w.col
                })
            })
            .collect();

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
                "warn_count": run.warn_count,
                "elapsed_ms": run.elapsed_ms,
            },
            "errors": errors,
            "warnings": warnings,
            "stale_since": stale_paths
        })))
    }
}
