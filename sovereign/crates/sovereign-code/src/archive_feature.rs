// SPDX-License-Identifier: AGPL-3.0-or-later
//! `archive_feature` — mark an ATOS feature as archived.
//!
//! Archived features stay in the database (their notes remain queryable by
//! explicit scope) but are excluded from default listings and injection.
//! Primarily driven by `sovereign atos archive` but exposed to agents so a
//! wrap-up session can close out a feature without shelling out.

use std::sync::Arc;

use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;

use corpus_engine_atos::FeatureStore;
use sovereign_core::tool_manifest::DeclaredTool;

pub struct ArchiveFeatureTool {
    store: Arc<FeatureStore>,
}

impl ArchiveFeatureTool {
    pub fn new(store: Arc<FeatureStore>) -> Self {
        Self { store }
    }
}

impl ArchiveFeatureTool {
    /// Bind this tool's state to its `archive_feature` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("archive_feature", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `archive_feature`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let reason = params.get("reason").and_then(|v| v.as_str()).unwrap_or("");

        let ok = self
            .store
            .archive(id, reason)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "archive_feature".to_string(),
                message: e.to_string(),
            })?;

        if !ok {
            return Err(Error::InvalidInput(format!(
                "archive_feature: no feature with id='{id}'"
            )));
        }

        Ok(StepOutput::Json(json!({
            "id": id,
            "state": "archived",
            "reason": reason,
        })))
    }

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
        for key in ["id", "reason"] {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::InvalidInput(format!("archive_feature requires non-empty '{key}'"))
                })?;
        }
        Ok(())
    }
}
