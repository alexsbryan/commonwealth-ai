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

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_atos::FeatureStore;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct RecordAtosEventTool {
    features: Arc<FeatureStore>,
}

impl RecordAtosEventTool {
    pub fn new(features: Arc<FeatureStore>) -> Self {
        Self { features }
    }
}

impl RecordAtosEventTool {
    /// Bind this tool's state to its `record_atos_event` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("record_atos_event", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `record_atos_event`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
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
}
