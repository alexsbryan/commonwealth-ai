// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atos_verify` — step verification with hollow/untouched gates.
//!
//! Runs a shell verify command in the workdir and checks that the
//! agent actually modified the files it claimed to touch. Without
//! this, an agent can exit silently without writing anything, and
//! `cargo check` / `cargo test` passes because the prior on-disk
//! state already compiles — the FSM credits forward motion that
//! didn't happen.
//!
//! The tool runs three independent gates:
//! 1. **Verify** — the shell command exits zero.
//! 2. **Hollow files** — every entry in `files_touched` is non-trivial
//!    (≥16 non-whitespace bytes). Skipped when `files_touched` is empty.
//! 3. **Untouched files** — at least one file in `files_touched` was
//!    modified after `since_unix_ts`. Skipped unless the caller
//!    supplies `since_unix_ts` (epoch seconds, snapshotted *before*
//!    the agent ran).
//!
//! `passed` is true iff every applicable gate passes.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use super::atos_utils::{detect_hollow_files, run_verify_cmd};
use std::sync::Arc;
use sovereign_core::tool_manifest::DeclaredTool;

const TOOL_ID: &str = "atos_verify";

pub struct AtosVerifyTool {}

impl AtosVerifyTool {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for AtosVerifyTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AtosVerifyTool {
    /// Bind this tool's state to its `atos_verify` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("atos_verify", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atos_verify`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let workdir = require_str(params, "workdir")?;
        let verify_cmd = require_str(params, "verify_cmd")?;
        let files_touched = parse_string_array(params, "files_touched");
        let since_unix_ts = params
            .get("since_unix_ts")
            .and_then(|v| v.as_f64())
            .map(|f| f as u64);

        let workdir_path = PathBuf::from(workdir);
        if !workdir_path.is_dir() {
            return Err(Error::Tool {
                tool_id: TOOL_ID.into(),
                message: format!("workdir is not a directory: {workdir}"),
            });
        }

        let (verify_exit_ok, stdout) = run_verify_cmd(&workdir_path, verify_cmd).await;

        let hollow_warning = if files_touched.is_empty() {
            None
        } else {
            detect_hollow_files(&workdir_path, &files_touched)
        };
        let hollow_files: Vec<String> = if hollow_warning.is_some() {
            files_touched.clone()
        } else {
            vec![]
        };

        let untouched = match since_unix_ts {
            Some(ts) if !files_touched.is_empty() => {
                detect_untouched_since(&workdir_path, &files_touched, ts)
            }
            _ => UntouchedCheck::Skipped,
        };

        let passed = verify_exit_ok && hollow_warning.is_none() && !untouched.failed();

        Ok(StepOutput::Json(json!({
            "passed": passed,
            "verify_exit_ok": verify_exit_ok,
            "hollow_files": hollow_files,
            "untouched": untouched.failed(),
            "untouched_checked": !matches!(untouched, UntouchedCheck::Skipped),
            "stdout": stdout,
        })))
    }
}

fn require_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params[key].as_str().ok_or_else(|| Error::Tool {
        tool_id: TOOL_ID.into(),
        message: format!("missing required parameter '{key}'"),
    })
}

fn parse_string_array(params: &Value, key: &str) -> Vec<String> {
    params[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, PartialEq, Eq)]
enum UntouchedCheck {
    Skipped,
    Passed,
    Failed,
}

impl UntouchedCheck {
    fn failed(&self) -> bool {
        matches!(self, UntouchedCheck::Failed)
    }
}

fn detect_untouched_since(
    workdir: &std::path::Path,
    files: &[String],
    since_unix_ts: u64,
) -> UntouchedCheck {
    let cutoff = UNIX_EPOCH + std::time::Duration::from_secs(since_unix_ts);
    let any_modified = files.iter().any(|f| {
        let p = workdir.join(f);
        match std::fs::metadata(&p).and_then(|m| m.modified()) {
            Ok(mtime) => mtime > cutoff,
            Err(_) => false,
        }
    });
    if any_modified {
        UntouchedCheck::Passed
    } else {
        UntouchedCheck::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "atos-verify-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
            ..Default::default()
        }
    }

    fn json_output(out: StepOutput) -> Value {
        match out {
            StepOutput::Json(v) => v,
            _ => panic!("expected JSON output"),
        }
    }

    #[tokio::test]
    async fn rejects_missing_workdir() {
        let tool = AtosVerifyTool::new();
        let result = tool.run(&json!({"verify_cmd": "true"}), &ctx()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_missing_verify_cmd() {
        let tool = AtosVerifyTool::new();
        let result = tool.run(&json!({"workdir": "/tmp"}), &ctx()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn passes_on_trivial_true_command() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = AtosVerifyTool::new();
        let v = json_output(
            tool.run(
                &json!({
                    "workdir": tmp.path().to_string_lossy(),
                    "verify_cmd": "true",
                    "files_touched": []
                }),
                &ctx(),
            )
            .await
            .unwrap(),
        );
        assert!(v["passed"].as_bool().unwrap());
        assert!(!v["untouched_checked"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn fails_on_hollow_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("lib.rs"), "  \n  ").unwrap();
        let tool = AtosVerifyTool::new();
        let v = json_output(
            tool.run(
                &json!({
                    "workdir": tmp.path().to_string_lossy(),
                    "verify_cmd": "true",
                    "files_touched": ["lib.rs"]
                }),
                &ctx(),
            )
            .await
            .unwrap(),
        );
        assert!(!v["passed"].as_bool().unwrap());
        assert!(!v["hollow_files"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn untouched_gate_fires_when_since_ts_is_in_the_future() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a file with substantive content so the hollow gate is
        // not what fails the assertion.
        fs::write(tmp.path().join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // since_ts ten minutes in the future → no file can have been
        // modified after it.
        let future_ts = now + 600;
        let tool = AtosVerifyTool::new();
        let v = json_output(
            tool.run(
                &json!({
                    "workdir": tmp.path().to_string_lossy(),
                    "verify_cmd": "true",
                    "files_touched": ["lib.rs"],
                    "since_unix_ts": future_ts
                }),
                &ctx(),
            )
            .await
            .unwrap(),
        );
        assert!(v["untouched_checked"].as_bool().unwrap());
        assert!(v["untouched"].as_bool().unwrap());
        assert!(!v["passed"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn untouched_gate_passes_when_file_is_modified_after_since_ts() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        fs::write(&path, "pub fn answer() -> u32 { 42 }\n").unwrap();
        // Cutoff well in the past.
        let past_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .saturating_sub(Duration::from_secs(3600))
            .as_secs();
        let tool = AtosVerifyTool::new();
        let v = json_output(
            tool.run(
                &json!({
                    "workdir": tmp.path().to_string_lossy(),
                    "verify_cmd": "true",
                    "files_touched": ["lib.rs"],
                    "since_unix_ts": past_ts
                }),
                &ctx(),
            )
            .await
            .unwrap(),
        );
        assert!(v["untouched_checked"].as_bool().unwrap());
        assert!(!v["untouched"].as_bool().unwrap());
        assert!(v["passed"].as_bool().unwrap());
    }
}
