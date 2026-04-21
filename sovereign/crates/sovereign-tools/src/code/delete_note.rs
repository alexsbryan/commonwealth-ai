//! `delete_note` — remove a working note by ID.
//!
//! Notes are only removed explicitly — there is no automatic pruning or
//! expiry. Call this when a todo is resolved, a decision is superseded,
//! or an attempted approach is definitively abandoned.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::NoteStore;

pub struct DeleteNoteTool {
    store: Arc<NoteStore>,
}

impl DeleteNoteTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for DeleteNoteTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "delete_note".to_string(),
            name: "Delete Note".to_string(),
            description: "Delete a working note by its ID (returned by write_note \
                          or visible in read_notes results). Returns an error if \
                          the note does not exist."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The note ID to delete"
                    }
                },
                "required": ["id"]
            }),
            examples: vec![
                ToolExample {
                    situation: "You just completed the work described in a todo note, or a decision note has been superseded by a better approach. Clean it up so it doesn't mislead future sessions. The ID comes from the read_notes response.".into(),
                    call: serde_json::json!({ "id": "note_abc123" }),
                },
            ],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "deleted": { "type": "boolean" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("delete_note requires 'id'".to_string()))?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'id'".to_string()))?;

        let deleted = self
            .store
            .delete_note(id)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "delete_note".to_string(),
                message: e.to_string(),
            })?;

        if deleted {
            Ok(StepOutput::Json(json!({ "deleted": true })))
        } else {
            Err(Error::Tool {
                tool_id: "delete_note".to_string(),
                message: format!("note '{id}' not found"),
            })
        }
    }
}
