// SPDX-License-Identifier: AGPL-3.0-or-later
//! `session_reflection` — structured post-task feedback from the agent.
//!
//! Agents call this at the end of a significant task (refactor, bug fix,
//! feature). The reflection is stored as `kind = "reflection"` in the notes
//! database and surfaces to the developer via `sovereign reflect`. Future
//! agent sessions read active reflections as tool calibration before running
//! into known limitations.
//!
//! ## When to call
//!
//! - A refactor lands (especially one involving many call-site changes).
//! - A bug is fixed where a tool gave misleading or incomplete information.
//! - A feature is implemented and you had to work around tool limitations.
//! - Any task where you wished you had information earlier or found a tool
//!   result surprising.
//!
//! ## What makes a useful reflection
//!
//! Be specific. "blast_radius was helpful" is not useful. "blast_radius
//! missed 3 macro-generated call sites in commonwealth-inference — had to
//! grep for `embed!` invocations manually" is actionable signal.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::NoteStore;

pub struct SessionReflectionTool {
    store: Arc<NoteStore>,
}

impl SessionReflectionTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SessionReflectionTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("session_reflection").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("session_reflection")
            .permissions
            .clone()
    }

    async fn execute(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let task_summary = params
            .get("task_summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Tool {
                tool_id: "session_reflection".to_string(),
                message: "missing required field 'task_summary'".to_string(),
            })?;

        let tool_name = params.get("tool_name").and_then(|v| v.as_str());

        // Build the structured content blob. All fields are stored as JSON so
        // FTS5 can search across them and sovereign reflect can parse them.
        let content = json!({
            "task_summary": task_summary,
            "tool_name": tool_name,
            "tools_that_helped": params.get("tools_that_helped")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            "manual_work_that_should_be_a_tool": params
                .get("manual_work_that_should_be_a_tool")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "misleading_outputs": params
                .get("misleading_outputs")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "wished_i_had_known": params
                .get("wished_i_had_known")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        });

        let content_str = content.to_string();
        let session_id = ctx.conversation_id.as_str();

        let id = self
            .store
            .write_reflection(&content_str, tool_name, session_id)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "session_reflection".to_string(),
                message: e.to_string(),
            })?;

        // Return the note ID and a timestamp so the caller can confirm storage.
        Ok(StepOutput::Json(json!({
            "id": id,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "message": "Reflection recorded. Thank you — this improves future sessions."
        })))
    }
}
