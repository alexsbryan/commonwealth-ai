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
                          Prefer this over delete_note. The hide is PEER-CONVERGING: retire \
                          also tombstones the note, which is the event peers gossip, so a \
                          retired note disappears from other machines' reads too (UC-D1). \
                          Note: write_note with 'supersedes' already retires the superseded \
                          note automatically — call this only for a note that is stale with \
                          no replacement."
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
                    "retired":   { "type": "boolean" },
                    "tombstoned": {
                        "type": "boolean",
                        "description": "Always true on success — the peer-converging hide (UC-D1)."
                    },
                    "id":        { "type": "string" }
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
            .ok_or_else(|| {
                Error::InvalidInput("retire_note requires a non-empty 'reason'".to_string())
            })?;
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

        // UC-D1 (order seat-durable-rail): "B closes; A sees it gone".
        // `retire_by_id` alone sets only retired_at — a LOCAL hide with
        // no propagation, so a peer keeps showing the note. The
        // tombstone is the peer-converging hide: it emits the
        // propagation event peers ingest (tombstone-wins).
        // Order matters: tombstone FIRST, then retire. If the tombstone
        // succeeds and the retire fails, the note is hidden everywhere
        // (both local filters see the tombstone) and a retry of this
        // call completes the retire — tombstoning is idempotent. The
        // reverse order would strand the peer visible with no retry
        // path (the already-retired guard would reject it).
        self.store
            .set_note_tombstone(id, true)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "retire_note".to_string(),
                message: e.to_string(),
            })?;

        let retired = self
            .store
            .retire_by_id(id, reason)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "retire_note".to_string(),
                message: e.to_string(),
            })?;

        if retired {
            Ok(StepOutput::Json(json!({
                "retired": true,
                "tombstoned": true,
                "id": id,
            })))
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
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn retire_hides_the_note_but_keeps_the_row() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let id = store
            .write_note("invariant", "stale constraint", vec![], vec![], "s1")
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

    /// UC-D1 (order seat-durable-rail): closing an order must hide it
    /// on the PEER too. `retired_at` alone is local-only; the tombstone
    /// is the propagation event. Assert both are set.
    #[tokio::test]
    async fn retire_tombstones_so_the_hide_converges_to_peers() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(NoteStore::open(&tmp.path().join("notes.db")).unwrap());
        let id = store
            .write_note(
                "decision",
                "close order seat-durable-rail",
                vec![],
                vec![],
                "s1",
            )
            .await
            .unwrap();

        let tool = RetireNoteTool::new(Arc::clone(&store));
        let out = tool
            .execute(&json!({"id": id, "reason": "landed"}), &ctx())
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert_eq!(v["retired"].as_bool(), Some(true));
                assert_eq!(v["tombstoned"].as_bool(), Some(true));
            }
            other => panic!("expected Json, got {other:?}"),
        }

        assert!(
            store.is_note_tombstoned(&id).await.unwrap(),
            "retire must tombstone the note so the hide propagates to peers"
        );
        let row = store.read_note_by_id(&id).await.unwrap().unwrap();
        assert!(row.retired_at.is_some(), "retire must set retired_at");
        assert_eq!(row.retired_by.as_deref(), Some("landed"));
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
