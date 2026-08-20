// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MockAgentRunner` — deterministic runner for tests.
//!
//! Drops a fixed file set into the workdir, records a synthetic
//! tool-call trace, returns a populated artifact. No subprocess.

use std::fs;

use async_trait::async_trait;

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ExitReason, TokenCounts,
    ToolCallRecord,
};

/// One scripted file write the mock will make.
#[derive(Debug, Clone)]
pub struct MockFileWrite {
    /// Path inside the workdir.
    pub path: String,
    pub contents: String,
}

/// What the mock will pretend happened during the agent's run.
#[derive(Debug, Clone, Default)]
pub struct MockScript {
    pub files: Vec<MockFileWrite>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub tokens: TokenCounts,
    pub wall_ms: u64,
    pub exit_reason: ExitReason,
    pub final_assistant_text: String,
}

pub struct MockAgentRunner {
    script: MockScript,
}

impl MockAgentRunner {
    pub fn new(script: MockScript) -> Self {
        Self { script }
    }

    /// Default canned script — a Light's Out-shaped no-op that returns
    /// `ExitReason::Completed`. Tests that need richer behaviour build
    /// their own `MockScript` and call `new`.
    pub fn canned() -> Self {
        Self {
            script: MockScript {
                files: vec![],
                tool_calls: vec![],
                tokens: TokenCounts {
                    input: 100,
                    output: 50,
                },
                wall_ms: 0,
                exit_reason: ExitReason::Completed,
                final_assistant_text: "(mock canned runner)".to_string(),
            },
        }
    }
}

#[async_trait]
impl AgentRunner for MockAgentRunner {
    fn id(&self) -> &'static str {
        "mock"
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        // Write the scripted files into the workdir.
        for fw in &self.script.files {
            let target = ctx.workdir().join(&fw.path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, &fw.contents)?;
        }
        Ok(AgentRunArtifact {
            workdir: ctx.workdir,
            tokens: self.script.tokens.clone(),
            wall_ms: self.script.wall_ms,
            exit_reason: self.script.exit_reason.clone(),
            tool_calls: self.script.tool_calls.clone(),
            stderr_tail: String::new(),
            final_assistant_text: self.script.final_assistant_text.clone(),
            raw_stdout_lines: vec![],
            request_records: Vec::new(),
            role_model_map_used: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Sandbox;

    #[tokio::test]
    async fn mock_writes_scripted_files_and_returns_artifact() {
        let fixtures = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(fixtures.path().to_path_buf()).unwrap();
        let (workdir, _fx) = sb.into_workdir();

        let script = MockScript {
            files: vec![MockFileWrite {
                path: "src/lib.rs".into(),
                contents: "pub fn solve() -> u32 { 7 }\n".into(),
            }],
            tool_calls: vec![ToolCallRecord {
                turn: 1,
                tool: "write".into(),
                args_preview: "{...}".into(),
                ok: true,
                canonical_kind: None,
            }],
            tokens: TokenCounts {
                input: 200,
                output: 80,
            },
            wall_ms: 1234,
            exit_reason: ExitReason::Completed,
            final_assistant_text: "Done.".into(),
        };
        let runner = MockAgentRunner::new(script);

        let ctx = AgentRunContext {
            problem_id: "x".into(),
            prompt: "do x".into(),
            workdir,
            tool_allowlist: &["read", "write"],
            workdir_scale: commonwealth_agent_tools::WorkdirScale::Scaffold,
            token_budget: 1_000,
            wall_seconds_cap: 60,
            model_handle: "commonwealth/coder".into(),
            build_cmd: "cargo build".into(),
            verify_cmd: "cargo test".into(),
            syntax_validator: None,
            role_model_map: commonwealth_agent_tools::RoleModelMap::default(),
        };
        let artifact = runner.run(ctx).await.unwrap();
        assert_eq!(artifact.tokens.output, 80);
        assert_eq!(artifact.wall_ms, 1234);
        assert!(artifact.exit_reason.is_completed());

        let path = artifact.workdir_path().join("src/lib.rs");
        let body = std::fs::read_to_string(path).unwrap();
        assert!(body.contains("solve"));
    }
}
