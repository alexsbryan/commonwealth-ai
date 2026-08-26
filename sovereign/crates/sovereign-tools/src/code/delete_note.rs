// SPDX-License-Identifier: AGPL-3.0-or-later
//! `delete_note` — remove a working note by ID.
//!
//! Notes are only removed explicitly — there is no automatic pruning or
//! expiry. Call this when a todo is resolved, a decision is superseded,
//! or an attempted approach is definitively abandoned.

use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_notes::NoteStore;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct DeleteNoteTool {
    store: Arc<NoteStore>,
}

impl DeleteNoteTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

impl DeleteNoteTool {
    /// Bind this tool's state to its `delete_note` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("delete_note", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `delete_note`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
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

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {

        params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("delete_note requires 'id'".to_string()))?;
        Ok(())
    }
}
