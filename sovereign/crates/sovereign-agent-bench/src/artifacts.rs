//! Per-run artifact sink — agent workdir copy + per-judge prompts +
//! pi stderr/JSONL. The operator's iteration surface: every run drops
//! enough on disk that the next iteration can read "what did pi
//! actually write?" and "what did the judge actually see?" without
//! rerunning anything.
//!
//! Layout under `<artifacts_dir>/<problem_id>/`:
//!
//! ```text
//! problem_id/
//!   agent.json        agent run summary (tokens, exit, tool_calls, stderr_tail, final_text)
//!   workdir/          deep copy of the agent's workdir contents (filtered)
//!   judge/
//!     <dim>-trial-<N>.json   prompt + parsed outcome (or error)
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use commonwealth_agent_tools::RoleModelMap;
use serde::Serialize;
use serde_json::Value;

use crate::judge::{JudgeError, JudgeRequest, JudgeTrialOutcome};
use crate::runner::AgentRunArtifact;

const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", "__pycache__"];

#[derive(Debug, Clone)]
pub struct ArtifactSink {
    root: PathBuf,
}

impl ArtifactSink {
    /// `root` is the per-problem directory; caller creates it.
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write the agent's per-run summary + raw stdout JSONL + full
    /// stderr + a deep copy of the workdir.
    pub fn persist_agent_run(&self, artifact: &AgentRunArtifact) -> std::io::Result<()> {
        let summary = AgentSummary {
            tokens_input: artifact.tokens.input,
            tokens_output: artifact.tokens.output,
            wall_ms: artifact.wall_ms,
            exit_reason: serde_json::to_value(&artifact.exit_reason)
                .unwrap_or(Value::Null),
            tool_calls: artifact.tool_calls.clone(),
            stderr_tail: artifact.stderr_tail.clone(),
            final_assistant_text: artifact.final_assistant_text.clone(),
            raw_line_count: artifact.raw_stdout_lines.len(),
            role_model_map_used: artifact.role_model_map_used.clone(),
        };
        let body = serde_json::to_vec_pretty(&summary).unwrap_or_default();
        fs::write(self.root.join("agent.json"), body)?;

        // Raw JSONL — one line per agent stdout line. When our parser
        // missed tool_calls or assistant_text, this is the forensic
        // surface for figuring out what the agent ACTUALLY emitted.
        let mut jsonl = String::new();
        for line in &artifact.raw_stdout_lines {
            jsonl.push_str(line);
            jsonl.push('\n');
        }
        fs::write(self.root.join("agent.jsonl"), jsonl)?;

        // Full stderr (uncapped). `stderr_tail` in agent.json is the
        // capped form; this is the raw stream so the operator can
        // dig past 32 KiB if needed.
        fs::write(self.root.join("agent.stderr.txt"), &artifact.stderr_tail)?;

        // Per-turn chat-completion requests + responses. Empty for
        // runners that don't drive the daemon directly (pi). One
        // JSON object per line so the `replay` subcommand can pick
        // and re-send any individual turn with overrides.
        if !artifact.request_records.is_empty() {
            let mut requests_jsonl = String::new();
            for r in &artifact.request_records {
                requests_jsonl.push_str(&serde_json::to_string(r).unwrap_or_default());
                requests_jsonl.push('\n');
            }
            fs::write(self.root.join("requests.jsonl"), requests_jsonl)?;
        }

        // Deep-copy workdir (filtered).
        let workdir_dst = self.root.join("workdir");
        fs::create_dir_all(&workdir_dst)?;
        copy_filtered(artifact.workdir.path(), &workdir_dst)?;
        Ok(())
    }

    /// Persist one judge trial's prompt + outcome.
    pub fn persist_judge_trial(
        &self,
        dim_id: &str,
        trial: u8,
        req: &JudgeRequest,
        result: Result<&JudgeTrialOutcome, &JudgeError>,
    ) -> std::io::Result<()> {
        let dir = self.root.join("judge");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{dim_id}-trial-{trial}.json"));
        let blob = match result {
            Ok(outcome) => serde_json::json!({
                "dim": dim_id,
                "trial": trial,
                "prompt": req,
                "outcome": {
                    "anchor": outcome.anchor,
                    "rationale": outcome.rationale,
                },
                "ok": true,
            }),
            Err(err) => serde_json::json!({
                "dim": dim_id,
                "trial": trial,
                "prompt": req,
                "error": format!("{err}"),
                "ok": false,
            }),
        };
        let body = serde_json::to_vec_pretty(&blob).unwrap_or_default();
        fs::write(path, body)?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct AgentSummary {
    tokens_input: u64,
    tokens_output: u64,
    wall_ms: u64,
    exit_reason: Value,
    tool_calls: Vec<crate::runner::ToolCallRecord>,
    stderr_tail: String,
    final_assistant_text: String,
    /// Number of raw stdout lines captured to `agent.jsonl`.
    /// Useful sanity-check: empty `tool_calls` + non-zero
    /// `raw_line_count` says "agent emitted output we didn't parse."
    raw_line_count: usize,
    /// Per-role model overrides used during the run. `None` for
    /// single-model runs (PR-2 behavior); skipped on serialize so
    /// agent.json from default runs is byte-stable across PR-2 →
    /// PR-3.
    #[serde(skip_serializing_if = "Option::is_none")]
    role_model_map_used: Option<RoleModelMap>,
}

fn copy_filtered(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ft.is_dir() && SKIP_DIRS.iter().any(|s| *s == name_str) {
            continue;
        }
        let target = dst.join(&name);
        if ft.is_dir() {
            fs::create_dir_all(&target)?;
            copy_filtered(&entry.path(), &target)?;
        } else if ft.is_file() {
            // Skip obvious binaries by NUL-byte sniff (keep markdown +
            // source even when large; the operator's iteration loop
            // wants to read whatever pi wrote).
            let bytes = fs::read(entry.path()).unwrap_or_default();
            if bytes.iter().take(4096).any(|b| *b == 0) {
                continue;
            }
            fs::write(&target, &bytes)?;
        }
    }
    Ok(())
}

impl serde::Serialize for JudgeRequest {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = ser.serialize_struct("JudgeRequest", 6)?;
        s.serialize_field("problem_id", &self.problem_id)?;
        s.serialize_field("problem_prompt", &self.problem_prompt)?;
        s.serialize_field("dimension_name", &self.dimension_name)?;
        s.serialize_field("rubric_anchors", &self.rubric_anchors)?;
        s.serialize_field("workspace_view", &self.workspace_view)?;
        s.serialize_field("final_assistant_text", &self.final_assistant_text)?;
        s.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{ExitReason, TokenCounts, ToolCallRecord};

    fn fake_artifact() -> AgentRunArtifact {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("src.rs"), "pub fn x() {}\n").unwrap();
        AgentRunArtifact {
            workdir: tmp,
            tokens: TokenCounts {
                input: 100,
                output: 50,
            },
            wall_ms: 1234,
            exit_reason: ExitReason::Completed,
            tool_calls: vec![ToolCallRecord {
                turn: 1,
                tool: "write".into(),
                args_preview: "{...}".into(),
                ok: true,
                canonical_kind: None,
            }],
            stderr_tail: "no errors".into(),
            final_assistant_text: "done".into(),
            raw_stdout_lines: vec![
                r#"{"type":"message_end","message":{"usage":{"input_tokens":100,"output_tokens":50}}}"#.to_string(),
                r#"{"type":"some_event","payload":{}}"#.to_string(),
            ],
            request_records: Vec::new(),
            role_model_map_used: None,
        }
    }

    #[test]
    fn persist_agent_run_writes_summary_and_workdir_copy() {
        let dst = tempfile::tempdir().unwrap();
        let sink = ArtifactSink::new(dst.path().join("3.2-lights-out")).unwrap();
        let artifact = fake_artifact();
        sink.persist_agent_run(&artifact).unwrap();

        let summary_path = sink.root().join("agent.json");
        assert!(summary_path.is_file());
        let summary = std::fs::read_to_string(&summary_path).unwrap();
        assert!(summary.contains("\"tokens_output\""));
        assert!(summary.contains("\"final_assistant_text\""));
        assert!(summary.contains("\"completed\""));
        assert!(summary.contains("\"raw_line_count\": 2"));

        let workdir_copy = sink.root().join("workdir/src.rs");
        assert!(workdir_copy.is_file());

        let jsonl = std::fs::read_to_string(sink.root().join("agent.jsonl")).unwrap();
        assert_eq!(jsonl.lines().count(), 2);
        assert!(jsonl.contains("message_end"));
        assert!(jsonl.contains("some_event"));

        let stderr = std::fs::read_to_string(sink.root().join("agent.stderr.txt")).unwrap();
        assert_eq!(stderr, "no errors");
    }

    #[test]
    fn persist_judge_trial_writes_ok_and_err_shapes() {
        let dst = tempfile::tempdir().unwrap();
        let sink = ArtifactSink::new(dst.path().join("3.2")).unwrap();
        let req = JudgeRequest {
            problem_id: "3.2".into(),
            problem_prompt: "P".into(),
            dimension_name: "Approach".into(),
            rubric_anchors: ["a".into(), "b".into(), "c".into(), "d".into()],
            workspace_view: "WS".into(),
            final_assistant_text: "FT".into(),
        };
        let ok = JudgeTrialOutcome {
            anchor: 2,
            rationale: "good".into(),
        };
        sink.persist_judge_trial("dim_b", 0, &req, Ok(&ok)).unwrap();
        sink.persist_judge_trial(
            "dim_c",
            1,
            &req,
            Err(&JudgeError::Http("connection refused".into())),
        )
        .unwrap();

        let ok_path = sink.root().join("judge/dim_b-trial-0.json");
        let err_path = sink.root().join("judge/dim_c-trial-1.json");
        assert!(ok_path.is_file());
        assert!(err_path.is_file());
        let ok_body = std::fs::read_to_string(ok_path).unwrap();
        assert!(ok_body.contains("\"anchor\": 2"));
        let err_body = std::fs::read_to_string(err_path).unwrap();
        assert!(err_body.contains("\"ok\": false"));
        assert!(err_body.contains("connection refused"));
    }

    #[test]
    fn persist_agent_run_omits_role_model_map_when_none() {
        // Default single-model run (PR-2 behavior) — agent.json
        // should NOT carry the role_model_map_used field. Pins
        // byte-stable artifact for default runs.
        let dst = tempfile::tempdir().unwrap();
        let sink = ArtifactSink::new(dst.path().join("p")).unwrap();
        let artifact = fake_artifact();
        assert!(artifact.role_model_map_used.is_none());
        sink.persist_agent_run(&artifact).unwrap();
        let body = std::fs::read_to_string(sink.root().join("agent.json")).unwrap();
        assert!(!body.contains("role_model_map_used"));
    }

    #[test]
    fn persist_agent_run_writes_role_model_map_when_set() {
        use commonwealth_agent_tools::{Role, RoleModelMap};
        let dst = tempfile::tempdir().unwrap();
        let sink = ArtifactSink::new(dst.path().join("p")).unwrap();
        let mut map = RoleModelMap::new();
        map.set(Role::Planner, Some("commonwealth/coder".into()));
        map.set(Role::Implementer, Some("commonwealth/primary".into()));
        let mut artifact = fake_artifact();
        artifact.role_model_map_used = Some(map);
        sink.persist_agent_run(&artifact).unwrap();
        let body = std::fs::read_to_string(sink.root().join("agent.json")).unwrap();
        assert!(body.contains("role_model_map_used"));
        assert!(body.contains("commonwealth/coder"));
        assert!(body.contains("commonwealth/primary"));
        // Evaluator unset → field absent (RoleModelMap field-level
        // skip_serializing_if).
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let m = &parsed["role_model_map_used"];
        assert!(m.get("evaluator").is_none());
        assert_eq!(m["planner"], "commonwealth/coder");
        assert_eq!(m["implementer"], "commonwealth/primary");
    }

    #[test]
    fn copy_filtered_skips_target_dir() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("target/release")).unwrap();
        std::fs::write(src.path().join("target/release/blob.bin"), [0u8; 100]).unwrap();
        std::fs::write(src.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        copy_filtered(src.path(), dst.path()).unwrap();
        assert!(dst.path().join("Cargo.toml").is_file());
        assert!(!dst.path().join("target").exists());
    }
}
