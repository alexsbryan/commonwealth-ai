// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bare-metal agent runner — single chat-completion call, no
//! orchestration, no retry, no diversity. The minimum-ceremony
//! baseline: emit one edit, apply, run tests, report.
//!
//! Diagnostic value: if bare-metal solves a problem, the
//! orchestrated runners (search, native) add zero value for it. Lets
//! the operator pick the cheapest tool that does the job and lets
//! us measure how much of any orchestrated runner's lift comes from
//! the orchestration vs. just having a competent model emit at the
//! right shape.
//!
//! Uses the same EditAction schema + executor + test-runner as
//! `search`, just without the loop. Output is one tool_call record
//! and one chat request record so downstream judges still see a
//! recognizable trace.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::{info, warn};

use commonwealth_agent_tools::executor::ExecCtx;

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ChatRequestRecord, ExitReason,
    TokenCounts,
};
use crate::runners::shared::{
    apply_edit, chat_body, discover_source_file, parse_response, post_chat_completion,
    render_with_line_numbers, run_tests, EditAction,
};

const DEFAULT_EMIT_MAX_TOKENS: u32 = 2500;
const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct BareMetalRunner {
    http: reqwest::Client,
    provider_url: String,
    emit_max_tokens: u32,
    test_timeout: Duration,
}

impl BareMetalRunner {
    pub(crate) fn new() -> Self {
        Self::with_provider_url("http://localhost:9741/v1".into())
    }

    pub(crate) fn with_provider_url(provider_url: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            provider_url,
            emit_max_tokens: DEFAULT_EMIT_MAX_TOKENS,
            test_timeout: DEFAULT_TEST_TIMEOUT,
        }
    }
}

impl Default for BareMetalRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn language_for_source(source_file: &str) -> crate::problem::WitnessLanguage {
    use crate::problem::WitnessLanguage;
    if source_file.ends_with(".py") {
        WitnessLanguage::Python
    } else if source_file.ends_with(".rs") {
        WitnessLanguage::Rust
    } else if source_file.ends_with(".go") {
        WitnessLanguage::Go
    } else {
        WitnessLanguage::Python
    }
}

fn system_prompt() -> Value {
    json!({
        "role": "system",
        "content": "You are a careful engineer. Respond with one fenced JSON action header followed by one fenced source-code block; no commentary outside the two fenced blocks.",
    })
}

fn build_user_prompt(source_file: &str, file_listing: &str, problem_prompt: &str) -> Value {
    let content = format!(
        r#"Fix the program below so all tests pass.

## Problem

{problem_prompt}

## Current file (`{source_file}`, line-numbered)

```
{file_listing}
```

## Your output

Emit ONE fenced JSON action describing your edit, then ONE fenced
source code block with the new content.

```json
{{"action": "rewrite_function", "name": "<name>"}}
```
```json
{{"action": "patch_lines", "start": <int>, "end": <int>}}
```
```json
{{"action": "insert_before", "line": <int>}}
```

Pick whichever edit shape fits. Indent the source block to match the
file's existing indent at the edit site. No commentary outside the
two fenced blocks.
"#
    );
    json!({"role": "user", "content": content})
}

fn shape_summary(action: &EditAction) -> String {
    match action {
        EditAction::RewriteFunction { name } => format!("rewrite {name}"),
        EditAction::PatchLines { start, end } => format!("patch {start}-{end}"),
        EditAction::InsertBefore { line } => format!("insert@{line}"),
        EditAction::WriteFile { .. } => "write_file".to_string(),
    }
}

#[async_trait]
impl AgentRunner for BareMetalRunner {
    fn id(&self) -> &'static str {
        "bare-metal"
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/primary")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        let started = Instant::now();
        let workdir = ctx.workdir;
        let base_workdir = workdir.path().to_path_buf();

        let source_file = match discover_source_file(workdir.path()) {
            Some(f) => f,
            None => {
                warn!(problem = %ctx.problem_id, "bare-metal: no source file in workdir");
                return Ok(AgentRunArtifact {
                    workdir,
                    tokens: TokenCounts::default(),
                    wall_ms: started.elapsed().as_millis() as u64,
                    exit_reason: ExitReason::Crashed {
                        stderr_tail: "no source file present in workdir".into(),
                    },
                    tool_calls: vec![],
                    stderr_tail: String::new(),
                    final_assistant_text: String::new(),
                    raw_stdout_lines: vec![],
                    request_records: vec![],
                    role_model_map_used: None,
                });
            }
        };
        let language = language_for_source(&source_file);

        let file_listing = render_with_line_numbers(&base_workdir.join(&source_file));
        let messages = vec![
            system_prompt(),
            build_user_prompt(&source_file, &file_listing, &ctx.prompt),
        ];

        // One emit at daemon default temperature.
        let body = chat_body(
            &ctx.model_handle,
            messages.clone(),
            None,
            self.emit_max_tokens,
        );
        let request_started = Instant::now();
        let response_json = match post_chat_completion(&self.http, &self.provider_url, &body).await
        {
            Ok(v) => v,
            Err(e) => {
                warn!(problem = %ctx.problem_id, error = %e, "bare-metal: daemon call failed");
                return Ok(AgentRunArtifact {
                    workdir,
                    tokens: TokenCounts::default(),
                    wall_ms: started.elapsed().as_millis() as u64,
                    exit_reason: ExitReason::Crashed {
                        stderr_tail: format!("daemon: {e}"),
                    },
                    tool_calls: vec![],
                    stderr_tail: e,
                    final_assistant_text: String::new(),
                    raw_stdout_lines: vec![],
                    request_records: vec![],
                    role_model_map_used: None,
                });
            }
        };
        let elapsed_ms = request_started.elapsed().as_millis() as u64;
        let content = response_json
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tokens_in = response_json
            .pointer("/usage/prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tokens_out = response_json
            .pointer("/usage/completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tokens = TokenCounts {
            input: tokens_in,
            output: tokens_out,
        };

        let request_record = ChatRequestRecord {
            turn: 0,
            role: None,
            request: json!({"shape": "bare-metal one-shot"}),
            response: json!({"content": content}),
            elapsed_ms,
        };

        // Parse + apply.
        let Some(parsed) = parse_response(&content) else {
            info!(problem = %ctx.problem_id, "bare-metal: response had no parseable action+block");
            let final_text = format!(
                "(bare-metal: no parseable action; raw response truncated)\n{}",
                content.chars().take(500).collect::<String>()
            );
            return Ok(AgentRunArtifact {
                workdir,
                tokens,
                wall_ms: started.elapsed().as_millis() as u64,
                exit_reason: ExitReason::Completed,
                tool_calls: vec![],
                stderr_tail: String::new(),
                final_assistant_text: final_text,
                raw_stdout_lines: vec![],
                request_records: vec![request_record],
                role_model_map_used: None,
            });
        };
        let shape = shape_summary(&parsed.action);

        let mut exec_ctx = ExecCtx::new(base_workdir.clone());
        if let Some(v) = ctx.syntax_validator.clone() {
            exec_ctx = exec_ctx.with_syntax_validator(v);
        }
        let apply_msg = match apply_edit(&exec_ctx, &source_file, &parsed).await {
            Ok(()) => format!("applied {shape}"),
            Err(e) => {
                let detail = e
                    .render_for_agent()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                info!(problem = %ctx.problem_id, shape = %shape, error = %detail, "bare-metal: apply rejected");
                format!("apply rejected ({shape}): {detail}")
            }
        };

        // Final test run — even if apply was rejected, the workdir
        // is in its pre-edit state so the witness can still score.
        let test_result =
            run_tests(workdir.path(), &ctx.verify_cmd, language, self.test_timeout).await;

        let final_text = format!(
            "Bare-metal result: {} → tests {}/{} ({} failed)",
            apply_msg,
            test_result.parsed.passed,
            test_result.parsed.passed + test_result.parsed.failed,
            test_result.parsed.failed
        );

        Ok(AgentRunArtifact {
            workdir,
            tokens,
            wall_ms: started.elapsed().as_millis() as u64,
            exit_reason: ExitReason::Completed,
            tool_calls: vec![],
            stderr_tail: String::new(),
            final_assistant_text: final_text,
            raw_stdout_lines: vec![],
            request_records: vec![request_record],
            role_model_map_used: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_metal_runner_has_stable_id() {
        let r = BareMetalRunner::new();
        assert_eq!(r.id(), "bare-metal");
    }

    #[test]
    fn bare_metal_runner_default_model_is_primary() {
        let r = BareMetalRunner::new();
        assert_eq!(r.default_model_handle(), Some("commonwealth/primary"));
    }

    #[test]
    fn bare_metal_runner_defaults_match_search() {
        // Same emit-token budget + timeout as search so per-runner
        // comparisons are apples-to-apples on a single call.
        let r = BareMetalRunner::new();
        assert_eq!(r.emit_max_tokens, 2500);
        assert_eq!(r.test_timeout, Duration::from_secs(60));
    }
}
