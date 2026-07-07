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

use async_trait::async_trait;
use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
};

use super::solve_http::{SolveJobs, SubmitWire};

pub struct SolveTool(pub Arc<SolveJobs>);
pub struct SolveStatusTool(pub Arc<SolveJobs>);
pub struct SolveCancelTool(pub Arc<SolveJobs>);

#[async_trait]
impl Tool for SolveTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "solve".to_string(),
            name: "Solve".to_string(),
            description: "The standard engine for executing a coding goal. Give it a \
                workdir (a git repo) and a plain-language goal; the daemon makes the goal \
                test-shaped — using your failing tests if you have them, writing the one \
                failing test that pins the goal if you don't — then iterates until the \
                tests pass. Prefer this over hand-editing for any goal a test can measure: \
                bug fixes, new functions, behavior changes, splitting oversized files. \
                Returns a job_id immediately; poll solve_status for live rounds and the \
                result, then review the workdir with `git diff`."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workdir": {
                        "type": "string",
                        "description": "Absolute path to the git repo to solve in. Must be committed/clean unless force=true."
                    },
                    "goal": {
                        "type": "string",
                        "description": "Plain-language coding goal, e.g. \"add an is_palindrome function to utils.py\"."
                    },
                    "verb": {
                        "type": "string",
                        "enum": ["fix", "pin", "split"],
                        "description": "Optional path override when the default inference isn't what you meant: fix = drive existing failing tests green; pin = only write the failing test; split = shrink oversized files (requires max_lines)."
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "With verb=split: the per-file line budget."
                    },
                    "test_command": {
                        "type": "string",
                        "description": "Override the auto-detected test command."
                    },
                    "model": {
                        "type": "string",
                        "description": "Override the model (default: the daemon's primary)."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Acknowledge solving on a dirty tree."
                    }
                },
                "required": ["workdir", "goal"]
            }),
            examples: vec![],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Fast, // submit returns immediately; the job itself is Slow
            scope: Scope::Session,
            output_schema: Some(json!({
                "job_id": "string — pass to solve_status / solve_cancel",
                "detected": {
                    "framework": "string",
                    "test_command": "string",
                    "model": "string"
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        // The solver edits the workdir and runs its test command.
        vec![Permission::Shell, Permission::FileWrite]
    }

    fn validate(&self, params: &Value) -> Result<()> {
        for key in ["workdir", "goal"] {
            if params.get(key).and_then(|v| v.as_str()).is_none() {
                return Err(Error::InvalidInput(format!(
                    "solve requires a '{key}' string parameter"
                )));
            }
        }
        Ok(())
    }

    async fn execute(&self, params: &Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
}

#[async_trait]
impl Tool for SolveStatusTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "solve_status".to_string(),
            name: "Solve status".to_string(),
            description: "State of a solve job: running/done/cancelled, the rounds so far \
                (what won, what each candidate tried), and — once done — the full result \
                with test counts before/after and the winning diff."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "From solve's response." }
                },
                "required": ["job_id"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: Some(json!({
                "state": "running | done | cancelled",
                "rounds": "array of round events",
                "result": "present when done: {path, result: TrialResult, ...}"
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = job_id(params)?;
        match self.0.get(id) {
            Some(job) => Ok(StepOutput::Json(job.status_json())),
            None => Ok(StepOutput::Json(no_such_job(id))),
        }
    }
}

#[async_trait]
impl Tool for SolveCancelTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "solve_cancel".to_string(),
            name: "Solve cancel".to_string(),
            description: "Cancel a running solve job. The workdir keeps whatever the last \
                promoted round wrote — `git diff` shows it, `git checkout .` discards it."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "From solve's response." }
                },
                "required": ["job_id"]
            }),
            examples: vec![],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Session,
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = job_id(params)?;
        match self.0.cancel(id) {
            Some(true) => Ok(StepOutput::Json(json!({ "job_id": id, "state": "cancelled" }))),
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

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[test]
    fn solve_descriptor_names_it_the_standard_engine() {
        let tool = SolveTool(Arc::new(SolveJobs::new(1)));
        let d = tool.descriptor();
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
            .execute(&json!({"job_id": "nope"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => assert_eq!(v["error"], "no_such_job"),
            other => panic!("expected Json output, got {other:?}"),
        }
    }
}
