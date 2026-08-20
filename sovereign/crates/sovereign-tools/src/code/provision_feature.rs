// SPDX-License-Identifier: AGPL-3.0-or-later
//! `provision_feature` — create an ATOS feature charter.
//!
//! This tool is primarily invoked by the `sovereign atos provision` CLI over
//! localhost MCP, but it is also exposed to agents so a meta-agent can
//! scaffold a new feature during a planning session.
//!
//! Idempotency: provisioning an existing feature id fails with InvalidInput.
//! The CLI is expected to either pass a fresh id or call `archive_feature`
//! on the stale row first.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_atos::FeatureStore;

pub struct ProvisionFeatureTool {
    store: Arc<FeatureStore>,
}

impl ProvisionFeatureTool {
    pub fn new(store: Arc<FeatureStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ProvisionFeatureTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("provision_feature").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("provision_feature")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        for key in ["id", "title", "charter_md"] {
            params
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::InvalidInput(format!("provision_feature requires non-empty '{key}'"))
                })?;
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let charter_md = params
            .get("charter_md")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sovereign_md = params
            .get("sovereign_md")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let stop_condition = params
            .get("stop_condition")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let feature = self
            .store
            .provision(id, title, charter_md, sovereign_md, stop_condition)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "provision_feature".to_string(),
                message: e.to_string(),
            })?;

        Ok(StepOutput::Json(json!({
            "id": feature.id,
            "state": feature.state,
            "created_at": feature.created_at,
        })))
    }
}
