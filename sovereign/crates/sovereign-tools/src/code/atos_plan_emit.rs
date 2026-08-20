// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atos_plan_emit` — structured plan emission for the ATOS Runner.
//!
//! ## Why this tool exists
//!
//! Earlier ATOS Runner versions asked the agent to emit a fenced
//! ```json``` block in PLAN / REASSESS phases. Free-form structured
//! output is brittle: small models truncate, omit required fields,
//! wrap the plan in a `{"plan": {...}}` envelope, or get confused
//! mid-string when nested escaping piles up. We layered six runner-
//! side robustness fixes (envelope unwrap, default-fill, brace-
//! balanced extractor, etc.) and still hit the failure mode every
//! few iterations.
//!
//! Structured tool calls bypass the failure entirely. The agent
//! emits the plan as a tool-call argument — JSON the model is
//! already trained to produce inside `<tool_call>` blocks, with the
//! daemon's parser handling the framing. The tool validates the
//! schema, builds a canonical `plan.json` (auto-incremented
//! revision, carried-over feature_id), and writes it to disk. The
//! runner reads the file after the agent exits.
//!
//! ## Output contract
//!
//! `<workdir>/.sovereign/plan.json` is the authoritative live plan
//! per the runner's resumption convention. The tool refuses to
//! write outside `<workdir>/.sovereign/` so a confused agent can't
//! escape its sandbox.
//!
//! Returns a small JSON document with the path and stats so the
//! agent can confirm in its session output.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

pub struct AtosPlanEmitTool {}

impl AtosPlanEmitTool {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for AtosPlanEmitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AtosPlanEmitTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("atos_plan_emit").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("atos_plan_emit")
            .permissions
            .clone()
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let workdir = params
            .get("workdir")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("atos_plan_emit needs string `workdir`".into()))?;
        if !workdir.starts_with('/') {
            return Err(Error::InvalidInput(format!(
                "atos_plan_emit `workdir` must be absolute path; got `{workdir}`"
            )));
        }
        let steps = params
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("atos_plan_emit needs array `steps`".into()))?;
        if steps.is_empty() {
            return Err(Error::InvalidInput(
                "atos_plan_emit `steps` is empty".into(),
            ));
        }
        if steps.len() > 32 {
            return Err(Error::InvalidInput(format!(
                "atos_plan_emit `steps` has {} entries; cap is 32",
                steps.len()
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for (i, step) in steps.iter().enumerate() {
            let obj = step
                .as_object()
                .ok_or_else(|| Error::InvalidInput(format!("step {i} is not an object")))?;
            for required in ["id", "goal", "verify_cmd"] {
                let v = obj
                    .get(required)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty());
                if v.is_none() {
                    return Err(Error::InvalidInput(format!(
                        "step {i} missing required non-empty `{required}`"
                    )));
                }
            }
            let id = obj.get("id").and_then(Value::as_str).unwrap();
            if !seen.insert(id.to_string()) {
                return Err(Error::InvalidInput(format!(
                    "step {i} has duplicate id `{id}`"
                )));
            }
        }
        Ok(())
    }

    async fn execute(&self, params: &Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let workdir = params
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| Error::InvalidInput("workdir missing".into()))?;
        let plan_path = workdir.join(".sovereign").join("plan.json");

        // Carry over feature_id and revision from any existing plan
        // so the runner's resumption logic stays consistent.
        let (prior_revision, prior_feature_id) = read_prior_plan_meta(&plan_path);

        let feature_id = params
            .get("feature_id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
            .or(prior_feature_id)
            .unwrap_or_else(|| {
                workdir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "atos-feature".into())
            });

        let revision = prior_revision.map(|r| r + 1).unwrap_or(1);

        // Build the canonical plan JSON. Steps land in `pending`
        // state by default; the runner's merge step copies execution
        // state from the prior plan when it reads the file.
        let now = chrono::Utc::now().to_rfc3339();
        let in_steps = params
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out_steps: Vec<Value> = Vec::with_capacity(in_steps.len());
        for s in in_steps {
            let id = s
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let goal = s
                .get("goal")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let verify_cmd = s
                .get("verify_cmd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let rationale = s
                .get("rationale")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let files_touched: Vec<String> = s
                .get("files_touched")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            out_steps.push(json!({
                "id": id,
                "goal": goal,
                "files_touched": files_touched,
                "verify_cmd": verify_cmd,
                "rationale": rationale,
                "state": "pending",
                "attempts": 0
            }));
        }

        let plan = json!({
            "schema_version": "1",
            "feature_id": feature_id,
            "design_sha": "",
            "created_at": now,
            "revision": revision,
            "steps": out_steps,
        });

        let body = serde_json::to_string_pretty(&plan).map_err(|e| Error::Tool {
            tool_id: "atos_plan_emit".into(),
            message: format!("serialize plan: {e}"),
        })?;
        if let Some(parent) = plan_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Tool {
                tool_id: "atos_plan_emit".into(),
                message: format!("mkdir {}: {e}", parent.display()),
            })?;
        }
        std::fs::write(&plan_path, body).map_err(|e| Error::Tool {
            tool_id: "atos_plan_emit".into(),
            message: format!("write {}: {e}", plan_path.display()),
        })?;

        Ok(StepOutput::Json(json!({
            "plan_path": plan_path.to_string_lossy().into_owned(),
            "steps": out_steps.len(),
            "revision": revision,
            "feature_id": feature_id,
        })))
    }
}

fn read_prior_plan_meta(plan_path: &Path) -> (Option<u32>, Option<String>) {
    let Ok(body) = std::fs::read_to_string(plan_path) else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return (None, None);
    };
    let rev = v.get("revision").and_then(Value::as_u64).map(|n| n as u32);
    let fid = v
        .get("feature_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    (rev, fid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::types::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "atos-plan-emit-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn writes_plan_to_workdir_sovereign_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = AtosPlanEmitTool::new();
        let params = json!({
            "workdir": tmp.path().to_string_lossy(),
            "steps": [
                {"id": "step-01", "goal": "scaffold", "verify_cmd": "true", "files_touched": ["Cargo.toml"]}
            ]
        });
        tool.validate(&params).unwrap();
        let out = tool.execute(&params, &ctx()).await.unwrap();
        let written = tmp.path().join(".sovereign").join("plan.json");
        assert!(written.exists());
        let body = std::fs::read_to_string(&written).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["revision"], 1);
        assert_eq!(parsed["steps"].as_array().unwrap().len(), 1);
        // Output JSON includes path + stats.
        if let StepOutput::Json(v) = out {
            assert_eq!(v["steps"], 1);
            assert_eq!(v["revision"], 1);
        } else {
            panic!("expected JSON output");
        }
    }

    #[tokio::test]
    async fn auto_increments_revision_on_resubmit() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = AtosPlanEmitTool::new();
        let params = json!({
            "workdir": tmp.path().to_string_lossy(),
            "steps": [{"id": "s1", "goal": "g", "verify_cmd": "true"}]
        });
        tool.execute(&params, &ctx()).await.unwrap();
        tool.execute(&params, &ctx()).await.unwrap();
        let third = tool.execute(&params, &ctx()).await.unwrap();
        if let StepOutput::Json(v) = third {
            assert_eq!(v["revision"], 3);
        } else {
            panic!("expected JSON output");
        }
    }

    #[tokio::test]
    async fn carries_over_feature_id_from_prior_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = AtosPlanEmitTool::new();
        // Seed a prior plan with feature_id = "carry-me"
        let first = json!({
            "workdir": tmp.path().to_string_lossy(),
            "feature_id": "carry-me",
            "steps": [{"id": "s1", "goal": "g", "verify_cmd": "true"}]
        });
        tool.execute(&first, &ctx()).await.unwrap();
        // Second call without feature_id should carry it over.
        let second = json!({
            "workdir": tmp.path().to_string_lossy(),
            "steps": [{"id": "s1", "goal": "g", "verify_cmd": "true"}]
        });
        let out = tool.execute(&second, &ctx()).await.unwrap();
        if let StepOutput::Json(v) = out {
            assert_eq!(v["feature_id"], "carry-me");
        }
    }

    #[test]
    fn validate_rejects_relative_workdir() {
        let tool = AtosPlanEmitTool::new();
        let params = json!({
            "workdir": "relative/path",
            "steps": [{"id": "s1", "goal": "g", "verify_cmd": "true"}]
        });
        let err = tool.validate(&params).unwrap_err();
        assert!(format!("{err}").contains("absolute"));
    }

    #[test]
    fn validate_rejects_empty_steps() {
        let tool = AtosPlanEmitTool::new();
        let params = json!({"workdir": "/tmp/x", "steps": []});
        assert!(tool.validate(&params).is_err());
    }

    #[test]
    fn validate_rejects_duplicate_step_ids() {
        let tool = AtosPlanEmitTool::new();
        let params = json!({
            "workdir": "/tmp/x",
            "steps": [
                {"id": "s1", "goal": "a", "verify_cmd": "true"},
                {"id": "s1", "goal": "b", "verify_cmd": "true"}
            ]
        });
        let err = tool.validate(&params).unwrap_err();
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn validate_rejects_step_missing_goal() {
        let tool = AtosPlanEmitTool::new();
        let params = json!({
            "workdir": "/tmp/x",
            "steps": [{"id": "s1", "verify_cmd": "true"}]
        });
        let err = tool.validate(&params).unwrap_err();
        assert!(format!("{err}").contains("goal"));
    }
}
