// SPDX-License-Identifier: AGPL-3.0-or-later
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

use corpus_engine_notes::NoteStore;

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
        sovereign_core::tool_manifest::require("delete_note").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("delete_note")
            .permissions
            .clone()
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

        let deleted = self.store.delete_note(id).await.map_err(|e| Error::Tool {
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
