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

use corpus_engine::NoteStore;

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
        ToolDescriptor {
            id: "session_reflection".to_string(),
            name: "Session Reflection".to_string(),
            description: "Call this when a significant task is complete — a refactor lands, \
                          a bug is fixed, a feature is implemented. Record which tools helped, \
                          what you had to do manually that a tool should have handled, and what \
                          information would have saved steps if you had it earlier. \
                          This data surfaces to the developer as a product improvement backlog \
                          and is available to future agent sessions as tool calibration. \
                          Be specific — vague reflections are not useful. \
                          Future sessions can read these via: \
                          read_notes(kinds=[\"reflection\"], query=\"<tool_name>\")."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_summary": {
                        "type": "string",
                        "description": "One-sentence description of what was accomplished."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "The primary tool this reflection concerns (if any). \
                                        Used as the grouping key in sovereign reflect output."
                    },
                    "tools_that_helped": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tool IDs that made the task faster or easier."
                    },
                    "manual_work_that_should_be_a_tool": {
                        "type": "string",
                        "description": "Describe work you did manually (grep, file reads, etc.) \
                                        that a tool should have handled. Be specific about what \
                                        was missing."
                    },
                    "misleading_outputs": {
                        "type": "string",
                        "description": "Describe any tool output that was incorrect, incomplete, \
                                        or led you down the wrong path."
                    },
                    "wished_i_had_known": {
                        "type": "string",
                        "description": "Information that would have saved steps if available \
                                        at the start of the task."
                    }
                },
                "required": ["task_summary"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You just fixed a bug where a tool gave you incomplete or wrong information — record exactly what it missed so the developer can improve it and future sessions know to work around it.".into(),
                    call: serde_json::json!({
                        "task_summary": "Fixed GGML_ASSERT crash in embedding context on Metal",
                        "tool_name": "code_search",
                        "tools_that_helped": ["find_callers", "symbol_lookup"],
                        "manual_work_that_should_be_a_tool": "Had to read llama-context.cpp source directly to find op_offload parameter — no tool surfaces llama.cpp context param semantics",
                        "wished_i_had_known": "op_offload=true (the default) routes compute ops to Metal even when n_gpu_layers=0. All three flags must be set: n_gpu_layers=0, offload_kqv=false, op_offload=false."
                    }),
                },
                ToolExample {
                    situation: "A refactor touched many files and you want to record which tools were accurate, which were stale, and what you had to do by hand — so the next session starts calibrated.".into(),
                    call: serde_json::json!({
                        "task_summary": "Wired IndexHealthChecker into all 6 MCP tools",
                        "tools_that_helped": ["blast_radius", "find_callers", "symbol_lookup"],
                        "misleading_outputs": "rust-analyzer diagnostics showed stale errors after edits — confirmed by reading file that the fixes were already applied",
                        "manual_work_that_should_be_a_tool": "Had to grep for all NodeCapabilities struct literal sites across 17 files after adding a field — blast_radius only covers call graph, not struct initialization sites"
                    }),
                },
            ],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
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
