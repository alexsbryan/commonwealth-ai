// SPDX-License-Identifier: AGPL-3.0-or-later
//! `OpencodeRunner` — drives the `opencode` coding agent headless.
//!
//!   opencode run --pure --auto --format json --dir <workdir> \
//!       --model bench/<model> --title <problem> "<prompt>"
//!
//! Hermetic by construction. opencode reads a global config, the project's,
//! `~/.claude` rule files and skills, plugins, and fetches a model catalog
//! from models.dev at start; any of those would put the operator's own setup
//! (a hosted model, remote MCP servers) into a bench run, and the catalog
//! fetch alone stalled a fresh run for the full 600 s it was given
//! (2026-10-06, zero requests reached the server). So each run gets a
//! throwaway `HOME` and cache, the provider config arrives inline
//! (`OPENCODE_CONFIG_CONTENT`), and every loader and fetch is switched off.
//! `webfetch` is denied: a coding run answers from the workdir, not the web.
//!
//! `--title` is passed so opencode does not ask the model to title the
//! session: that request goes out alongside the first turn, and on a
//! one-slot server it either queues ahead of the turn or is refused, which
//! would put opencode bookkeeping into a server comparison.
//!
//! Events (opencode 1.18, `--format json`): `step_start`, then one
//! `tool_use` per tool (`part.tool`, `part.state.{input,status}`) and
//! `text` parts, then `step_finish` carrying `part.tokens.{input, output,
//! reasoning, cache.read}`. A step is one model call, read as one turn.
//! `tokens.input` is the uncached part only; the cached part is
//! `cache.read`. opencode's `reasoning` count read 0 on turns the server
//! streamed reasoning for, so it is not used; output tokens come from
//! `tokens.output`, which matched the server's `completion_tokens`.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Value};
use sovereign_agent_tools::adapter::{opencode as oc_adapter, AgentToolAdapter};
use tokio::process::Command;

use crate::runner::{AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner};
use crate::runners::jsonl_agent::{
    self, AgentEndpoint, AgentTurn, JsonlDialect, ToolRole, TurnTool,
};
use crate::sandbox::Sandbox;

/// The provider id opencode's config names the run's endpoint under.
const PROVIDER: &str = "bench";

/// Every opencode loader and fetch that would read the operator's setup or
/// the network, switched off.
const HERMETIC_ENV: &[(&str, &str)] = &[
    ("OPENCODE_DISABLE_MODELS_FETCH", "1"),
    ("OPENCODE_DISABLE_AUTOUPDATE", "1"),
    ("OPENCODE_DISABLE_SHARE", "1"),
    ("OPENCODE_DISABLE_CLAUDE_CODE", "1"),
    ("OPENCODE_DISABLE_DEFAULT_PLUGINS", "1"),
    ("OPENCODE_DISABLE_LSP_DOWNLOAD", "1"),
    ("OPENCODE_DISABLE_PROJECT_CONFIG", "1"),
    ("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1"),
];

pub struct OpencodeRunner {
    /// Path to the `opencode` binary. `None` means search PATH.
    binary: Option<String>,
    endpoint: AgentEndpoint,
}

impl OpencodeRunner {
    pub(crate) fn new() -> Self {
        Self {
            binary: None,
            endpoint: AgentEndpoint::local_daemon(),
        }
    }

    pub(crate) fn with_binary(mut self, path: impl Into<String>) -> Self {
        self.binary = Some(path.into());
        self
    }

    pub(crate) fn with_endpoint(mut self, endpoint: AgentEndpoint) -> Self {
        self.endpoint = endpoint;
        self
    }

    fn binary_path(&self) -> String {
        self.binary
            .clone()
            .unwrap_or_else(|| "opencode".to_string())
    }
}

impl Default for OpencodeRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for OpencodeRunner {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/coder")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        let start = Instant::now();

        // Throwaway HOME and cache, held until opencode exits.
        let home = tempfile::tempdir()
            .map_err(|e| AgentRunError::Internal(format!("opencode home: {e}")))?;
        let home_str = home.path().display().to_string();
        let cache_str = home.path().join(".cache").display().to_string();
        let config = opencode_config(&self.endpoint, &ctx.model_handle);
        let mut env = Sandbox::scrubbed_env(&[
            ("HOME", home_str.as_str()),
            ("XDG_CACHE_HOME", cache_str.as_str()),
            ("OPENCODE_CONFIG_CONTENT", config.as_str()),
        ]);
        for (k, v) in HERMETIC_ENV {
            env.insert((*k).to_string(), (*v).to_string());
        }
        if let Some(path) = jsonl_agent::path_with_binary_dir(
            &self.binary_path(),
            env.get("PATH").map(String::as_str),
        ) {
            env.insert("PATH".to_string(), path);
        }

        // Same workdir prefix the pi runner gives, so the two agents read
        // the same task statement.
        let workdir_state = jsonl_agent::describe_workdir(ctx.workdir.path());
        let final_prompt = format!(
            "## Workdir state (factual, current state of `.`)\n{workdir_state}\n\n---\n\n{}",
            ctx.prompt,
        );

        tracing::info!(
            problem = %ctx.problem_id,
            model = %ctx.model_handle,
            endpoint = %self.endpoint.base_url,
            budget = ctx.token_budget,
            wall_cap = ctx.wall_seconds_cap,
            "agent_bench: opencode.run starting"
        );

        let mut cmd = Command::new(self.binary_path());
        cmd.arg("run")
            .arg("--pure")
            .arg("--auto")
            .arg("--format")
            .arg("json")
            .arg("--dir")
            .arg(ctx.workdir())
            .arg("--model")
            .arg(format!("{PROVIDER}/{}", ctx.model_handle))
            .arg("--title")
            .arg(&ctx.problem_id)
            .arg(&final_prompt)
            .current_dir(ctx.workdir())
            .env_clear();
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let dialect = OpencodeDialect::new(&ctx.build_cmd, &ctx.verify_cmd);
        let run = jsonl_agent::supervise("opencode", cmd, &ctx, Box::new(dialect)).await?;
        drop(home);

        let wall_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            problem = %ctx.problem_id,
            tokens_in = run.tokens.input,
            tokens_out = run.tokens.output,
            wall_ms,
            exit = run.exit_reason.id(),
            "agent_bench: opencode.run complete"
        );

        Ok(AgentRunArtifact {
            workdir: ctx.workdir,
            tokens: run.tokens,
            wall_ms,
            exit_reason: run.exit_reason,
            tool_calls: run.tool_calls,
            stderr_tail: run.stderr_tail,
            final_assistant_text: run.final_text,
            raw_stdout_lines: run.raw_lines,
            // opencode is subprocess-driven like pi; its HTTP traffic is
            // seen by the arm tap, not here.
            request_records: Vec::new(),
            role_model_map_used: None,
        })
    }
}

/// opencode's config for one run: the run's endpoint as an
/// OpenAI-compatible provider serving `model`, also as the small model (so
/// nothing reaches for a model the run did not name), with web fetch off.
fn opencode_config(endpoint: &AgentEndpoint, model: &str) -> String {
    let id = format!("{PROVIDER}/{model}");
    json!({
        "$schema": "https://opencode.ai/config.json",
        "model": id,
        "small_model": id,
        "share": "disabled",
        "autoupdate": false,
        "permission": { "webfetch": "deny" },
        "provider": {
            PROVIDER: {
                "npm": "@ai-sdk/openai-compatible",
                "name": PROVIDER,
                "options": { "baseURL": endpoint.base_url, "apiKey": "dummy" },
                "models": {
                    model: {
                        "name": model,
                        "tool_call": true,
                        "reasoning": true,
                        "limit": {
                            "context": endpoint.context_window,
                            "output": endpoint.max_output_tokens,
                        },
                    }
                },
            }
        },
    })
    .to_string()
}

/// opencode's events read as turns: one `step_start` … `step_finish` per
/// model call, with its `tool_use` and `text` parts in between.
struct OpencodeDialect {
    adapter: oc_adapter::Adapter,
    pending: AgentTurn,
}

impl OpencodeDialect {
    fn new(build_cmd: &str, verify_cmd: &str) -> Self {
        Self {
            adapter: oc_adapter::Adapter::default().with_problem_commands(build_cmd, verify_cmd),
            pending: AgentTurn::default(),
        }
    }
}

impl JsonlDialect for OpencodeDialect {
    fn feed(&mut self, line: &str) -> Option<AgentTurn> {
        let v: Value = serde_json::from_str(line.trim()).ok()?;
        let part = v.get("part").cloned().unwrap_or(Value::Null);
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "step_start" => {
                self.pending = AgentTurn::default();
                None
            }
            "tool_use" => {
                let name = part
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown");
                let state = part.get("state").cloned().unwrap_or(Value::Null);
                let input = state.get("input").cloned().unwrap_or(Value::Null);
                let mut record = jsonl_agent::tool_record(
                    name,
                    &input,
                    self.adapter.translate(name, &input).canonical_kind(),
                );
                record.ok = state.get("status").and_then(|s| s.as_str()) == Some("completed");
                self.pending.tools.push(TurnTool {
                    role: opencode_role(name),
                    path: input
                        .get("filePath")
                        .and_then(|p| p.as_str())
                        .map(str::to_string),
                    record,
                });
                None
            }
            "text" => {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    if !self.pending.text.is_empty() {
                        self.pending.text.push('\n');
                    }
                    self.pending.text.push_str(t);
                }
                None
            }
            "step_finish" => {
                let tokens = part.get("tokens").cloned().unwrap_or(Value::Null);
                let n = |k: &str| tokens.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                let mut turn = std::mem::take(&mut self.pending);
                turn.input_tokens = n("input");
                turn.output_tokens = n("output");
                Some(turn)
            }
            _ => None,
        }
    }
}

/// What opencode's tool names mean to the detectors.
fn opencode_role(tool: &str) -> ToolRole {
    match tool {
        "write" | "edit" | "multiedit" | "patch" => ToolRole::Write,
        "bash" => ToolRole::Verify,
        _ => ToolRole::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four events of the 2026-10-06 probe's first step, verbatim in
    /// shape (ids trimmed).
    const STEP: &[&str] = &[
        r#"{"type":"step_start","part":{"type":"step-start"}}"#,
        r#"{"type":"tool_use","part":{"type":"tool","tool":"write","state":{"status":"completed","input":{"filePath":"/w/fib.py","content":"def fib(n): ..."}}}}"#,
        r#"{"type":"text","part":{"type":"text","text":"Wrote fib.py."}}"#,
        r#"{"type":"step_finish","part":{"type":"step-finish","reason":"tool-calls","tokens":{"total":7768,"input":561,"output":234,"reasoning":0,"cache":{"write":0,"read":6973}}}}"#,
    ];

    #[test]
    fn a_step_is_one_turn_with_its_tools_and_tokens() {
        let mut d = OpencodeDialect::new("python3 -m py_compile", "pytest -q");
        let out: Vec<_> = STEP.iter().filter_map(|l| d.feed(l)).collect();
        assert_eq!(out.len(), 1, "only step_finish closes a turn");
        let t = &out[0];
        assert_eq!((t.input_tokens, t.output_tokens), (561, 234));
        assert_eq!(t.tools.len(), 1);
        assert_eq!(t.tools[0].role, ToolRole::Write);
        assert_eq!(t.tools[0].path.as_deref(), Some("/w/fib.py"));
        assert!(t.tools[0].record.ok);
        assert_eq!(
            t.tools[0].record.canonical_kind,
            Some(sovereign_agent_tools::PrimitiveKind::WriteFile)
        );
        assert_eq!(t.text, "Wrote fib.py.");
    }

    #[test]
    fn a_failed_tool_is_recorded_not_ok() {
        let mut d = OpencodeDialect::new("", "");
        d.feed(STEP[0]);
        d.feed(r#"{"type":"tool_use","part":{"tool":"bash","state":{"status":"error","input":{"command":"pytest"}}}}"#);
        let t = d.feed(STEP[3]).unwrap();
        assert_eq!(t.tools[0].role, ToolRole::Verify);
        assert!(!t.tools[0].record.ok);
    }

    #[test]
    fn non_events_are_ignored() {
        let mut d = OpencodeDialect::new("", "");
        assert!(d.feed("").is_none());
        assert!(d.feed("not json").is_none());
        assert!(d.feed(r#"{"type":"something_new"}"#).is_none());
    }

    #[test]
    fn config_names_the_run_endpoint_and_denies_webfetch() {
        let ep = AgentEndpoint {
            base_url: "http://127.0.0.1:18183/v1".into(),
            context_window: 65_536,
            max_output_tokens: 16_384,
        };
        let v: Value =
            serde_json::from_str(&opencode_config(&ep, "Qwen3.8-27B-UD-Q6_K_XL")).unwrap();
        assert_eq!(v["model"], "bench/Qwen3.8-27B-UD-Q6_K_XL");
        assert_eq!(v["small_model"], v["model"]);
        assert_eq!(
            v["provider"]["bench"]["options"]["baseURL"],
            "http://127.0.0.1:18183/v1"
        );
        assert_eq!(
            v["provider"]["bench"]["models"]["Qwen3.8-27B-UD-Q6_K_XL"]["limit"]["context"],
            65_536
        );
        assert_eq!(v["permission"]["webfetch"], "deny");
    }
}
