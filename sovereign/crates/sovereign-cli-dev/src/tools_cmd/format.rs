//! Formatters for `sovereign tools` stdout.
//!
//! The CLI is agent-primary: output is plain text, shaped for an LLM
//! to read without a JSON-parse step. `--format json` is available for
//! pipeline use.

use serde_json::Value;
use sovereign_core::types::{Effect, Latency, Scope, StepOutput, ToolDescriptor};

#[derive(Copy, Clone)]
pub(super) enum OutputMode {
    Text,
    Json,
}

impl OutputMode {
    pub(super) fn parse(s: Option<&str>) -> Result<Self, String> {
        match s {
            None | Some("text") => Ok(Self::Text),
            Some("json") => Ok(Self::Json),
            Some(other) => Err(format!("unknown --format '{other}' (expected: text, json)")),
        }
    }
}

/// Render a tool's `StepOutput` for terminal consumption. Text outputs
/// pass through verbatim; JSON outputs pretty-print in text mode and
/// compact-print in json mode.
pub(super) fn render_step_output(output: &StepOutput, mode: OutputMode) -> String {
    match output {
        StepOutput::Text(s) => match mode {
            OutputMode::Text => s.clone(),
            OutputMode::Json => serde_json::to_string(&Value::String(s.clone()))
                .unwrap_or_else(|_| s.clone()),
        },
        StepOutput::Json(v) => match mode {
            OutputMode::Text => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
            OutputMode::Json => v.to_string(),
        },
        StepOutput::Skipped => "(skipped)".to_string(),
        StepOutput::Jump(_) | StepOutput::ReasonWithToolsResult { .. } => {
            "(executor-internal output variant; not renderable at the CLI)".to_string()
        }
    }
}

/// Compact `[Read · Persistent · Fast]` tag matching the planner +
/// ReasonWithTools prompt annotations.
pub(super) fn behaviour_tag(d: &ToolDescriptor) -> String {
    let effect = match d.effect {
        Effect::Read => "Read",
        Effect::Write => "Write",
        Effect::ReadWrite => "ReadWrite",
    };
    let scope = match d.scope {
        Scope::Session => "Session",
        Scope::Persistent => "Persistent",
        Scope::External => "External",
    };
    let latency = match d.latency {
        Latency::Instant => "Instant",
        Latency::Fast => "Fast",
        Latency::Slow => "Slow",
        Latency::Streaming => "Streaming",
    };
    format!("[{effect} · {scope} · {latency}]")
}
