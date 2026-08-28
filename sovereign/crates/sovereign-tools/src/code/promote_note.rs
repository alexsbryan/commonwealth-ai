// SPDX-License-Identifier: AGPL-3.0-or-later
//! `promote_note` — rewrite a note from session/feature scope up to
//! feature/global scope.
//!
//! Promotions are the mechanism for turning a feature-specific decision
//! into a global invariant at teardown. The source row is left untouched
//! (audit trail); the new row carries `promoted_from = <source id>`.
//!
//! Operators typically drive this via `sovereign atos promote <id> --to global`,
//! but the tool is also available to agents so a wrap-up session can propose
//! promotions for the operator to review.

use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore};
use sovereign_core::tool_manifest::DeclaredTool;

pub struct PromoteNoteTool {
    store: Arc<NoteStore>,
}

impl PromoteNoteTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

impl PromoteNoteTool {
    /// Bind this tool's state to its `promote_note` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("promote_note", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `promote_note`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let to_scope_s = params
            .get("to_scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        let to_scope = NoteScope::parse(to_scope_s).unwrap_or(NoteScope::Global);
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let new_content = params
            .get("new_content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let new_id = self
            .store
            .promote_note(id, to_scope, feature_id, new_content)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "promote_note".to_string(),
                message: e.to_string(),
            })?;

        Ok(StepOutput::Json(json!({
            "source_id": id,
            "new_id": new_id,
            "to_scope": to_scope.as_str(),
            "feature_id": feature_id,
        })))
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("promote_note requires 'id'".to_string()))?;
        let scope = params
            .get("to_scope")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("promote_note requires 'to_scope'".to_string()))?;
        if !matches!(scope, "feature" | "global") {
            return Err(Error::InvalidInput(format!(
                "promote_note 'to_scope' must be 'feature' or 'global', got '{scope}'"
            )));
        }
        if scope == "feature"
            && params
                .get("feature_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .is_none()
        {
            return Err(Error::InvalidInput(
                "promote_note to_scope='feature' requires 'feature_id'".to_string(),
            ));
        }
        Ok(())
    }
}
