// SPDX-License-Identifier: AGPL-3.0-or-later
//! `archive_feature` — mark an ATOS feature as archived.
//!
//! Archived features stay in the database (their notes remain queryable by
//! explicit scope) but are excluded from default listings and injection.
//! Primarily driven by `sovereign atos archive` but exposed to agents so a
//! wrap-up session can close out a feature without shelling out.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_atos::FeatureStore;

pub struct ArchiveFeatureTool {
    store: Arc<FeatureStore>,
}

impl ArchiveFeatureTool {
    pub fn new(store: Arc<FeatureStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ArchiveFeatureTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("archive_feature").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("archive_feature")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
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

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
}
