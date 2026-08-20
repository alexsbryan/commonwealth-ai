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

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_notes::{NoteScope, NoteStore};

pub struct PromoteNoteTool {
    store: Arc<NoteStore>,
}

impl PromoteNoteTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for PromoteNoteTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("promote_note").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("promote_note")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
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

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
}
