// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP tools for the SOLVE surface — `solve`, `solve_status`,
//! `solve_cancel` — registered on the daemon's `/mcp` registry and
//! dispatching into the same [`SolveJobs`] table the HTTP routes
//! use. An MCP agent and a curl session see the same jobs.
//!
//! The `solve` description is the discoverability mechanism (spec
//! done-means #3): it tells agents this is the STANDARD engine for
//! coding goals, so "add an is_palindrome function" routes here
//! unprompted rather than through hand-editing.

use std::sync::Arc;

use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::types::{StepOutput, ToolContext};

use super::solve_http::{SolveJobs, SubmitWire};
use sovereign_core::tool_manifest::DeclaredTool;

pub struct SolveTool(pub Arc<SolveJobs>);
pub struct SolveStatusTool(pub Arc<SolveJobs>);
pub struct SolveCancelTool(pub Arc<SolveJobs>);

impl SolveTool {
    /// Bind this tool's state to its `solve` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("solve", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `solve`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let wire: SubmitWire = serde_json::from_value(params.clone())
            .map_err(|e| Error::InvalidInput(format!("solve params: {e}")))?;
        match self.0.submit(wire) {
            Ok(job) => Ok(StepOutput::Json(json!({
                "job_id": job.id,
                "detected": job.detected,
                "next": "call solve_status with this job_id to watch rounds land; review with `git diff` when done",
            }))),
            Err(e) => {
                let (_, body) = e.payload();
                Ok(StepOutput::Json(body))
            }
        }
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
        for key in ["workdir", "goal"] {
            if params.get(key).and_then(|v| v.as_str()).is_none() {
                return Err(Error::InvalidInput(format!(
                    "solve requires a '{key}' string parameter"
                )));
            }
        }
        Ok(())
    }
}

impl SolveStatusTool {
    /// Bind this tool's state to its `solve_status` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("solve_status", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `solve_status`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = job_id(params)?;
        match self.0.get(id) {
            Some(job) => Ok(StepOutput::Json(job.status_json())),
            None => Ok(StepOutput::Json(no_such_job(id))),
        }
    }
}

impl SolveCancelTool {
    /// Bind this tool's state to its `solve_cancel` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("solve_cancel", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `solve_cancel`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = job_id(params)?;
        match self.0.cancel(id) {
            Some(true) => Ok(StepOutput::Json(
                json!({ "job_id": id, "state": "cancelled" }),
            )),
            Some(false) => Ok(StepOutput::Json(json!({
                "job_id": id,
                "error": "not_running",
                "message": "job already finished",
            }))),
            None => Ok(StepOutput::Json(no_such_job(id))),
        }
    }
}

fn job_id(params: &Value) -> Result<&str> {
    params
        .get("job_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::InvalidInput("missing 'job_id' string parameter".into()))
}

fn no_such_job(id: &str) -> Value {
    json!({ "error": "no_such_job", "job_id": id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::traits::Tool;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    #[test]
    fn solve_descriptor_names_it_the_standard_engine() {
        let tool = SolveTool(Arc::new(SolveJobs::new(1)));
        let d = tool.declared().descriptor();
        assert_eq!(d.id, "solve");
        // The discoverability sentence is load-bearing (spec
        // done-means #3) — a fresh agent must be able to tell this
        // is the default engine for coding goals, not a special
        // tool for special problems.
        assert!(
            d.description.contains("standard engine"),
            "description lost the discoverability sentence: {}",
            d.description
        );
    }

    #[tokio::test]
    async fn status_of_unknown_job_reports_no_such_job() {
        let tool = SolveStatusTool(Arc::new(SolveJobs::new(1)));
        let out = tool
            .declared()
            .execute(&json!({"job_id": "nope"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => assert_eq!(v["error"], "no_such_job"),
            other => panic!("expected Json output, got {other:?}"),
        }
    }
}
