// SPDX-License-Identifier: AGPL-3.0-or-later
//! `record_atos_event` — append one tool-execution event to the ATOS
//! run ledger.
//!
//! Invoked by the opencode plugin (`.opencode/plugins/sovereign-atos.ts`)
//! on `tool.execute.before` and `tool.execute.after` hooks, and — for
//! parse-failure telemetry — by the mesh adapter when the Qwen output
//! parser rejects a tool-call block. Events are keyed by `run_id`
//! (which the CLI exports as `$ATOS_RUN_ID` to the driver subprocess).
//!
//! Orphan events (run_id not in `atos_runs`) are rejected with
//! `InvalidInput` rather than swallowed — a plugin that fires before
//! `open_run` is a bug we want to see.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_atos::FeatureStore;

pub struct RecordAtosEventTool {
    features: Arc<FeatureStore>,
}

impl RecordAtosEventTool {
    pub fn new(features: Arc<FeatureStore>) -> Self {
        Self { features }
    }
}

#[async_trait]
impl Tool for RecordAtosEventTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "record_atos_event".to_string(),
            name: "Record ATOS Event".to_string(),
            description:
                "Append a tool-execution event to the ATOS run ledger. Called by the opencode \
                 plugin on tool.execute.before/after hooks. The `phase` discriminator \
                 distinguishes pre-call (args captured) from post-call (outcome + duration). \
                 Orphan events whose run_id is not in atos_runs are rejected."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "run_id": {
                        "type": "string",
                        "description": "$ATOS_RUN_ID exported by `sovereign atos start-milestone`."
                    },
                    "call_id": {
                        "type": "string",
                        "description": "Driver-scoped tool-call id (persists across before/after)."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "Name of the MCP tool that was invoked."
                    },
                    "phase": {
                        "type": "string",
                        "enum": ["before", "after", "parse_error"],
                        "description": "before=args captured, after=outcome captured, \
                                        parse_error=tool_call JSON failed to parse."
                    },
                    "args_json": {
                        "type": "string",
                        "description": "Serialized arguments on phase=before. Omit on after."
                    },
                    "outcome": {
                        "type": "string",
                        "description": "'success'|'error'|'empty_result'|'parse_error' on phase=after."
                    },
                    "duration_ms": {
                        "type": "integer",
                        "description": "Wall-clock duration in ms on phase=after."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional driver session id; sets atos_runs.session_id on \
                                        first capture."
                    }
                },
                "required": ["run_id", "call_id", "tool_name", "phase"]
            }),
            examples: vec![ToolExample {
                situation: "opencode's tool.execute.after hook just fired after read_notes \
                            returned success in ~18ms. Append the event."
                    .into(),
                call: serde_json::json!({
                    "run_id": "run_abc123",
                    "call_id": "call_42",
                    "tool_name": "read_notes",
                    "phase": "after",
                    "outcome": "success",
                    "duration_ms": 18
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "recorded": { "type": "boolean" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        for key in ["run_id", "call_id", "tool_name", "phase"] {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::InvalidInput(format!("record_atos_event requires non-empty '{key}'"))
                })?;
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let run_id = params.get("run_id").and_then(|v| v.as_str()).unwrap_or("");
        let call_id = params.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let tool_name = params
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let phase = params.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        let args_json = params.get("args_json").and_then(|v| v.as_str());
        let outcome = params.get("outcome").and_then(|v| v.as_str());
        let duration_ms = params.get("duration_ms").and_then(|v| v.as_i64());
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        // Best-effort session capture. A race where two concurrent
        // events try to set it is fine — `set_run_session` is a
        // no-op after the first one lands.
        if let Some(sid) = session_id {
            let _ = self.features.set_run_session(run_id, sid).await;
        }

        let event_id = self
            .features
            .record_tool_event(
                run_id,
                call_id,
                tool_name,
                phase,
                args_json,
                outcome,
                duration_ms,
            )
            .await
            .map_err(|e| Error::Tool {
                tool_id: "record_atos_event".into(),
                message: e.to_string(),
            })?;

        Ok(StepOutput::Json(json!({
            "id": event_id,
            "run_id": run_id,
            "phase": phase,
        })))
    }
}
