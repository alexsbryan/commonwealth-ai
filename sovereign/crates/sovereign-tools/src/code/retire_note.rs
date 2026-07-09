// SPDX-License-Identifier: AGPL-3.0-or-later
//! `retire_note` — mark a working note stale without deleting it.
//!
//! Retirement is the non-destructive counterpart to `delete_note`: it sets
//! `retired_at`/`retired_by` so the note is hidden from `read_notes`, but the
//! row is KEPT — its history, its supersedes chain, and its content_hash stay
//! intact for gossip and audit. Prefer this over `delete_note` when a note is
//! no longer true but you want a record of why (the `reason`). `write_note`
//! with `supersedes` retires the superseded note automatically; call this
//! directly when a note is stale on its own, with no replacement.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::NoteStore;

pub struct RetireNoteTool {
    store: Arc<NoteStore>,
}

impl RetireNoteTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for RetireNoteTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "retire_note".to_string(),
            name: "Retire Note".to_string(),
            description: "Retire a working note by its ID: hide it from read_notes without \
                          deleting the row (its history and supersedes chain are kept). Use \
                          when a note is no longer true but you want a durable record of why. \
                          Prefer this over delete_note. Note: write_note with 'supersedes' \
                          already retires the superseded note automatically — call this only \
                          for a note that is stale with no replacement."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The note ID to retire (from write_note or read_notes)."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why the note is retired (e.g. 'fixed in PR #88', \
                                        'the gate was removed'). Stored as retired_by and shown \
                                        in history views."
                    }
                },
                "required": ["id", "reason"]
            }),
            examples: vec![ToolExample {
                situation: "A prior invariant note no longer holds because the constraint was \
                            removed, and there's no single replacement note to supersede it with. \
                            Retire it with the reason so future sessions see why it's gone."
                    .into(),
                call: serde_json::json!({
                    "id": "note_abc123",
                    "reason": "the native-grammar path was deleted; this no longer applies"
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "retired": { "type": "boolean" },
                    "id":      { "type": "string" }
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
            .ok_or_else(|| Error::InvalidInput("retire_note requires 'id'".to_string()))?;
        params
            .get("reason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("retire_note requires a non-empty 'reason'".to_string()))?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'id'".to_string()))?;
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'reason'".to_string()))?;

        let retired = self
            .store
            .retire_by_id(id, reason)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "retire_note".to_string(),
                message: e.to_string(),
            })?;

        if retired {
            Ok(StepOutput::Json(json!({ "retired": true, "id": id })))
        } else {
            Err(Error::Tool {
                tool_id: "retire_note".to_string(),
                message: format!("note '{id}' not found or already retired"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sovereign_core::types::ToolContext;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "retire-note-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[tokio::test]
    async fn retire_hides_the_note_but_keeps_the_row() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let id = store
            .write_note(
                "invariant",
                "stale constraint",
                vec![],
                vec![],
                "s1",
            )
            .await
            .unwrap();

        let tool = RetireNoteTool::new(Arc::clone(&store));
        let out = tool
            .execute(&json!({"id": id, "reason": "the gate was removed"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => assert_eq!(v["retired"].as_bool(), Some(true)),
            other => panic!("expected Json, got {other:?}"),
        }

        // Row is kept (non-destructive) but marked retired with the reason.
        let row = store.read_note_by_id(&id).await.unwrap().unwrap();
        assert!(row.retired_at.is_some(), "retire must set retired_at");
        assert_eq!(row.retired_by.as_deref(), Some("the gate was removed"));

        // Second retire is a no-op error (already retired).
        let again = tool
            .execute(&json!({"id": id, "reason": "again"}), &ctx())
            .await;
        assert!(again.is_err(), "re-retiring an already-retired note errors");
    }

    #[tokio::test]
    async fn retire_requires_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let tool = RetireNoteTool::new(store);
        assert!(tool.validate(&json!({"id": "x"})).is_err());
        assert!(tool.validate(&json!({"id": "x", "reason": ""})).is_err());
        assert!(tool.validate(&json!({"id": "x", "reason": "ok"})).is_ok());
    }
}
