// SPDX-License-Identifier: AGPL-3.0-or-later
//! `read_note_by_id` — fetch one note row by its id.
//!
//! Complements `read_notes` (bulk, filtered) and `read_note_digest` (M2)
//! with a point-lookup. The intended use is: after a context compaction,
//! the agent reads the injected digest (which references notes by id),
//! then calls `read_note_by_id` on exactly the handful it needs to expand.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::NoteStore;

pub struct ReadNoteByIdTool {
    store: Arc<NoteStore>,
}

impl ReadNoteByIdTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ReadNoteByIdTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("read_note_by_id").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("read_note_by_id")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("read_note_by_id requires 'id'".to_string()))?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'id'".to_string()))?;

        let row = self
            .store
            .read_note_by_id(id)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "read_note_by_id".to_string(),
                message: e.to_string(),
            })?;

        match row {
            Some(n) => Ok(StepOutput::Json(json!({
                "id": n.id,
                "kind": n.kind,
                "content": n.content,
                "symbols": n.symbols,
                "files": n.files,
                "session_id": n.session_id,
                "created_at": n.created_at,
                "scope": n.scope,
                "feature_id": n.feature_id,
                "promoted_from": n.promoted_from,
                "author": self.store.attribution(n.origin_node_id.as_deref()).label(),
                "author_relation": self.store.attribution(n.origin_node_id.as_deref()).as_str(),
            }))),
            None => Ok(StepOutput::Json(json!({ "found": false, "id": id }))),
        }
    }
}
