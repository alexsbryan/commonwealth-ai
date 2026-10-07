// SPDX-License-Identifier: AGPL-3.0-or-later
//! `PiRunner` — drives the `pi` coding agent against the local daemon.
//!
//! Surface (see `pi --help`):
//!   pi --provider commonwealth --model commonwealth/coder \
//!      --no-session -p --mode json \
//!      --offline --no-context-files \
//!      --tools read,edit,write,bash,find,grep,ls \
//!      "<prompt>"
//!
//! Pi streams JSONL events on stdout. We parse a small subset:
//!   - `message_end` carrying `usage.input_tokens` / `usage.output_tokens`
//!   - `tool_execution_start` / `tool_execution_end`
//!   - `turn_end`
//!
//! Other event types are skipped at `tracing::debug` (lenient: pi's
//! schema may grow; we don't want a new event type to crash the harness).
//!
//! Budget enforcement: cumulative `usage.output_tokens` past
//! `token_budget` → SIGTERM. Wall-clock cap fires independently.
//!
//! Env scrub: only `PATH` (the pi binary's directory first), `HOME`,
//! `PI_CODING_AGENT_DIR`, `LANG`. No model credentials reach the child.
//!
//! Endpoint: pi reads its providers from `<agent dir>/models.json` and
//! nothing else — it has no URL env var (pi 0.78.0 never reads the
//! `PI_PROVIDER_URL` this runner used to set, so every run reached whatever
//! `~/.pi/agent/models.json` named). Each run writes its own agent dir with
//! one `commonwealth` provider at the run's [`AgentEndpoint`], so the URL a
//! report names is the one pi used, and `~/.pi` is never read or touched.

use std::time::Instant;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use crate::runner::{
    AgentRunArtifact, AgentRunContext, AgentRunError, AgentRunner, ToolCallRecord,
};
use crate::runners::jsonl_agent::{
    self, AgentEndpoint, AgentTurn, JsonlDialect, ToolRole, TurnTool,
};
use crate::sandbox::Sandbox;

/// Allowlisted pi tools. Structural invariant per ARCH §7.2 — not
/// configurable from TOML.
/// Pi tools exposed to the agent. `edit` is intentionally absent —
/// pi's edit requires `oldText` to match the file byte-for-byte
/// (whitespace + line endings included), which models struggle to
/// reproduce after a `read`. Forcing `write` (full-file replacement)
/// removes that brittleness. See `bench/agent-coding/problems/*/prompt.md`
/// — each prompt now nudges the model toward `write` + `bash` verify.
/// Tool names this runner passes to pi via `--tools`. Authoritative
/// source is `sovereign_agent_tools::adapter::pi::Adapter::
/// pi_tool_allowlist()`; the equivalence is pinned by
/// `tool_allowlist_matches_canonical_adapter` below so a future PR
/// that adds a primitive to the canonical layer can't forget to
/// expose it on the pi runner.
pub(crate) const PI_TOOL_ALLOWLIST: &[&str] = &["read", "write", "bash", "find", "grep", "ls"];

pub struct PiRunner {
    /// Path to the `pi` binary. `None` means search PATH.
    binary: Option<String>,
    endpoint: AgentEndpoint,
}

impl PiRunner {
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
        self.binary.clone().unwrap_or_else(|| "pi".to_string())
    }
}

impl Default for PiRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRunner for PiRunner {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn default_model_handle(&self) -> Option<&str> {
        Some("commonwealth/coder")
    }

    async fn run(&self, ctx: AgentRunContext) -> Result<AgentRunArtifact, AgentRunError> {
        let start = Instant::now();

        // The run's own agent dir. Held until pi exits.
        let agent_dir = tempfile::tempdir()
            .map_err(|e| AgentRunError::Internal(format!("pi agent dir: {e}")))?;
        std::fs::write(
            agent_dir.path().join("models.json"),
            pi_models_json(&self.endpoint, &ctx.model_handle),
        )
        .map_err(|e| AgentRunError::Internal(format!("pi models.json: {e}")))?;
        let agent_dir_str = agent_dir.path().display().to_string();
        let mut env = Sandbox::scrubbed_env(&[("PI_CODING_AGENT_DIR", agent_dir_str.as_str())]);
        if let Some(path) = jsonl_agent::path_with_binary_dir(
            &self.binary_path(),
            env.get("PATH").map(String::as_str),
        ) {
            env.insert("PATH".to_string(), path);
        }
        let tools_arg = ctx.tool_allowlist.join(",");

        // Prefix the prompt with the actual workdir state. Without
        // this, agents reach for `read` to inspect the empty dir and
        // loop until the no-progress detector fires. Closes the
        // observed "read-only loop on empty workdir" failure mode
        // surfaced by run `p`.
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
            "agent_bench: pi.run starting"
        );

        let mut cmd = Command::new(self.binary_path());
        cmd.arg("--provider")
            .arg("commonwealth")
            .arg("--model")
            .arg(&ctx.model_handle)
            .arg("--no-session")
            .arg("--no-context-files")
            .arg("--offline")
            .arg("--print")
            .arg("--mode")
            .arg("json")
            .arg("--tools")
            .arg(&tools_arg)
            .arg(&final_prompt)
            .current_dir(ctx.workdir())
            .env_clear();
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let dialect = PiDialect {
            build_cmd: ctx.build_cmd.clone(),
            verify_cmd: ctx.verify_cmd.clone(),
        };
        let run = jsonl_agent::supervise("pi", cmd, &ctx, Box::new(dialect)).await?;
        drop(agent_dir);

        let wall_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            problem = %ctx.problem_id,
            tokens_in = run.tokens.input,
            tokens_out = run.tokens.output,
            wall_ms,
            exit = run.exit_reason.id(),
            "agent_bench: pi.run complete"
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
            // Pi runner is subprocess-driven — request capture would
            // require parsing pi's internal HTTP traffic. Out of
            // scope; replay supports the native runner only.
            request_records: Vec::new(),
            // Pi has no role concept; role_model_map is ignored on
            // this path.
            role_model_map_used: None,
        })
    }
}

/// Parsed-and-relevant subset of pi's JSONL events.
///
/// `AssistantTurn` bundles a single `message_end` event's usage +
/// content[]. Tools and text are extracted by `harvest_assistant_blocks`
/// at drain time.
#[derive(Debug)]
enum ParsedEvent {
    AssistantTurn {
        input_tokens: u64,
        output_tokens: u64,
        content: Value,
    },
    Unknown,
}

/// Pi (`@earendil-works/pi-coding-agent`, observed 2026-05-21)
/// emits a single event per JSONL line with `type` ∈
/// {session, agent_start, turn_start, message_start, message_end,
///  turn_end, agent_end, auto_retry_start, auto_retry_end, ...}.
///
/// Assistant tool calls live INSIDE `message_end.message.content[]`
/// as `{type:"tool_use", name, input}` blocks (alongside text blocks).
/// `message_end.message.usage` carries token accounting with
/// `{input, output, totalTokens, cacheRead, cacheWrite}` fields.
fn parse_pi_line(line: &str) -> ParsedEvent {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ParsedEvent::Unknown;
    }
    let v: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return ParsedEvent::Unknown,
    };
    let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match kind {
        "message_end" => {
            let msg = v.get("message").cloned().unwrap_or(Value::Null);
            let role = msg.get("role").and_then(|x| x.as_str()).unwrap_or("");
            let usage = msg.get("usage").cloned().unwrap_or(Value::Null);
            let input_tokens =
                extract_token_count(&usage, &["input_tokens", "inputTokens", "input"]);
            let output_tokens =
                extract_token_count(&usage, &["output_tokens", "outputTokens", "output"]);
            // Only credit tokens to assistant messages — user-message
            // echoes also carry message_end events but with zero usage.
            // Defensive: zero-usage events fall through harmlessly anyway.
            ParsedEvent::AssistantTurn {
                input_tokens,
                output_tokens,
                content: if role == "assistant" {
                    msg.get("content").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
            }
        }
        _ => ParsedEvent::Unknown,
    }
}

fn extract_token_count(usage: &Value, keys: &[&str]) -> u64 {
    for k in keys {
        if let Some(n) = usage.get(*k).and_then(|x| x.as_u64()) {
            return n;
        }
    }
    0
}

/// Walk an assistant-message `content` array and emit per-block
/// observations (tool_use → ToolCallRecord, text → string).
fn harvest_assistant_blocks(
    content: &Value,
    pi_build_cmd: &str,
    pi_verify_cmd: &str,
) -> (Vec<ToolCallRecord>, String) {
    let mut tools: Vec<ToolCallRecord> = Vec::new();
    let mut text = String::new();
    if let Some(arr) = content.as_array() {
        for block in arr {
            let block_type = block.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match block_type {
                "tool_use" | "toolCall" => {
                    let name = block
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let input = block
                        .get("input")
                        .or_else(|| block.get("arguments"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args_preview = serde_json::to_string(&input)
                        .unwrap_or_default()
                        .chars()
                        .take(256)
                        .collect::<String>();
                    // Normalize through the pi adapter into a
                    // canonical primitive kind. Observer-only —
                    // pi already executed the tool; this just
                    // labels the telemetry for cross-agent
                    // comparison. `Unrecognized` / `Unknown`
                    // outcomes leave `canonical_kind = None`,
                    // which the failure-class aggregator surfaces.
                    let canonical_kind = {
                        use sovereign_agent_tools::adapter::{pi as pi_adapter, AgentToolAdapter};
                        let adapter = pi_adapter::Adapter::default()
                            .with_problem_commands(pi_build_cmd, pi_verify_cmd);
                        adapter.translate(&name, &input).canonical_kind()
                    };
                    tools.push(ToolCallRecord {
                        turn: 0,
                        tool: name,
                        args_preview,
                        ok: true,
                        canonical_kind,
                    });
                }
                "text" => {
                    if let Some(s) = block.get("text").and_then(|x| x.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(s);
                    }
                }
                _ => {}
            }
        }
    }
    (tools, text)
}

/// pi's provider config for one run: a single `commonwealth` provider (the
/// name the command line selects) serving `model` at `endpoint`. The shape
/// is the one `scripts/setup-pi-provider.sh` writes.
fn pi_models_json(endpoint: &AgentEndpoint, model: &str) -> String {
    serde_json::json!({
        "providers": {
            "commonwealth": {
                "baseUrl": endpoint.base_url,
                "api": "openai-completions",
                "apiKey": "dummy",
                "compat": {
                    "supportsDeveloperRole": false,
                    "supportsReasoningEffort": false,
                },
                "models": [{
                    "id": model,
                    "name": model,
                    "contextWindow": endpoint.context_window,
                    "maxTokens": endpoint.max_output_tokens,
                }],
            }
        }
    })
    .to_string()
}

/// pi's events read as turns: every `message_end` is one (a user echo is an
/// empty one, which keeps turn numbering what it was before the supervisor
/// moved out).
struct PiDialect {
    build_cmd: String,
    verify_cmd: String,
}

impl JsonlDialect for PiDialect {
    fn feed(&mut self, line: &str) -> Option<AgentTurn> {
        let ParsedEvent::AssistantTurn {
            input_tokens,
            output_tokens,
            content,
        } = parse_pi_line(line)
        else {
            return None;
        };
        let (records, text) = harvest_assistant_blocks(&content, &self.build_cmd, &self.verify_cmd);
        Some(AgentTurn {
            input_tokens,
            output_tokens,
            tools: pi_turn_tools(&content, records),
            text,
        })
    }
}

/// Pair each tool block with the record `harvest_assistant_blocks` made for
/// it (one per block, same order) and add what the detectors key on: the
/// role its name plays and the `path` its arguments name.
fn pi_turn_tools(content: &Value, records: Vec<ToolCallRecord>) -> Vec<TurnTool> {
    let blocks = content.as_array().map(Vec::as_slice).unwrap_or(&[]);
    blocks
        .iter()
        .filter(|b| {
            matches!(
                b.get("type").and_then(|x| x.as_str()),
                Some("tool_use") | Some("toolCall")
            )
        })
        .zip(records)
        .map(|(block, record)| TurnTool {
            role: match record.tool.as_str() {
                "write" => ToolRole::Write,
                "bash" => ToolRole::Verify,
                "done" => ToolRole::Done,
                _ => ToolRole::Other,
            },
            path: block
                .get("arguments")
                .or_else(|| block.get("input"))
                .and_then(|args| args.get("path"))
                .and_then(|p| p.as_str())
                .map(str::to_string),
            record,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_message_end_extracts_pi_real_shape() {
        // Pi's real usage shape: `input` / `output` (no _tokens suffix).
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":120,"output":35,"totalTokens":155,"cacheRead":0,"cacheWrite":0}}}"#;
        match parse_pi_line(line) {
            ParsedEvent::AssistantTurn {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 120);
                assert_eq!(output_tokens, 35);
            }
            _ => panic!("expected AssistantTurn"),
        }
    }

    #[test]
    fn parse_message_end_supports_legacy_alias() {
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":2}}}"#;
        match parse_pi_line(line) {
            ParsedEvent::AssistantTurn {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 7);
                assert_eq!(output_tokens, 2);
            }
            _ => panic!("expected AssistantTurn"),
        }
    }

    #[test]
    fn parse_user_message_yields_null_content() {
        // User-message echoes carry message_end too but role=user;
        // content must NOT leak to the assistant-side harvest.
        let line = r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#;
        match parse_pi_line(line) {
            ParsedEvent::AssistantTurn { content, .. } => {
                assert!(content.is_null());
            }
            _ => panic!("expected AssistantTurn"),
        }
    }

    #[test]
    fn harvest_extracts_tool_use_and_text_blocks() {
        let content = serde_json::json!([
            {"type": "text", "text": "I'll write src/lib.rs."},
            {"type": "tool_use", "id": "abc", "name": "write", "input": {"path": "src/lib.rs", "content": "pub fn solve() {}"}},
            {"type": "text", "text": "Done."},
        ]);
        let (tools, text) = harvest_assistant_blocks(
            &content,
            "cargo build",
            "cargo test --quiet --test integration",
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool, "write");
        assert!(tools[0].args_preview.contains("src/lib.rs"));
        assert!(text.contains("I'll write"));
        assert!(text.contains("Done."));
    }

    #[test]
    fn harvest_handles_empty_content() {
        let (tools, text) =
            harvest_assistant_blocks(&serde_json::json!([]), "cargo build", "cargo test");
        assert!(tools.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn harvest_handles_null_content() {
        let (tools, text) =
            harvest_assistant_blocks(&serde_json::Value::Null, "cargo build", "cargo test");
        assert!(tools.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn parse_unknown_kind_is_lenient() {
        let line = r#"{"type":"some_new_event","payload":{}}"#;
        assert!(matches!(parse_pi_line(line), ParsedEvent::Unknown));
    }

    #[test]
    fn parse_blank_line_is_unknown() {
        assert!(matches!(parse_pi_line(""), ParsedEvent::Unknown));
        assert!(matches!(parse_pi_line("   "), ParsedEvent::Unknown));
    }

    #[test]
    fn parse_garbage_does_not_panic() {
        assert!(matches!(parse_pi_line("not json"), ParsedEvent::Unknown));
    }

    #[test]
    fn tool_allowlist_is_canonical() {
        assert_eq!(
            PI_TOOL_ALLOWLIST,
            &["read", "write", "bash", "find", "grep", "ls"]
        );
    }

    #[test]
    fn tool_allowlist_matches_canonical_adapter() {
        // The canonical pi adapter is the source of truth for which
        // pi tools the bench exposes. If a future PR adds a tool to
        // the adapter (e.g. opens up `mv` for some new primitive)
        // without updating PI_TOOL_ALLOWLIST, this fails.
        let canonical = sovereign_agent_tools::adapter::pi::Adapter::pi_tool_allowlist();
        assert_eq!(PI_TOOL_ALLOWLIST, canonical);
    }

    #[test]
    fn models_json_names_the_run_endpoint_and_model() {
        let ep = AgentEndpoint {
            base_url: "http://127.0.0.1:18180/v1".into(),
            context_window: 65_536,
            max_output_tokens: 16_384,
        };
        let v: Value =
            serde_json::from_str(&pi_models_json(&ep, "Qwen3.8-27B-UD-Q6_K_XL")).unwrap();
        let p = &v["providers"]["commonwealth"];
        assert_eq!(p["baseUrl"], "http://127.0.0.1:18180/v1");
        assert_eq!(p["models"][0]["id"], "Qwen3.8-27B-UD-Q6_K_XL");
        assert_eq!(p["models"][0]["contextWindow"], 65_536);
    }

    #[test]
    fn turn_tools_carry_role_and_write_path() {
        let content = serde_json::json!([
            {"type": "toolCall", "name": "read", "arguments": {"path": "src/lib.rs"}},
            {"type": "toolCall", "name": "write", "arguments": {"path": "src/lib.rs", "content": "..."}},
            {"type": "tool_use", "name": "bash", "input": {"command": "cargo test"}},
        ]);
        let (records, _) = harvest_assistant_blocks(&content, "cargo build", "cargo test");
        let tools = pi_turn_tools(&content, records);
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].role, ToolRole::Other);
        assert_eq!(tools[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(tools[1].role, ToolRole::Write);
        assert_eq!(tools[1].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(tools[2].role, ToolRole::Verify);
        assert_eq!(tools[2].path, None);
    }
}
