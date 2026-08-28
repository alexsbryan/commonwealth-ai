// SPDX-License-Identifier: AGPL-3.0-or-later
//! `release_scope` — drop a Claim. Idempotent: releasing twice is
//! a no-op. Spec §3 forbids history — once released, the claim is
//! gone.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::{StepOutput, ToolContext};

use crate::model::Privacy;
use crate::store::WorkAtlasStore;
use crate::tools::broadcast::ClaimBroadcaster;
use sovereign_core::tool_manifest::DeclaredTool;

#[derive(Debug)]
pub struct ReleaseScopeTool {
    store: Arc<WorkAtlasStore>,
    broadcaster: Arc<dyn ClaimBroadcaster>,
}

impl ReleaseScopeTool {
    pub fn new(store: Arc<WorkAtlasStore>, broadcaster: Arc<dyn ClaimBroadcaster>) -> Self {
        Self { store, broadcaster }
    }
}

impl ReleaseScopeTool {
    /// Bind this tool's state to its `release_scope` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("release_scope", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `release_scope`.
    async fn run(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let claim_id_str = params
            .get("claim_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("release_scope requires 'claim_id'".into()))?;
        let claim_id = Uuid::parse_str(claim_id_str)
            .map_err(|_| Error::InvalidInput(format!("invalid claim_id uuid: {claim_id_str}")))?;

        // Find which namespace the claim lives in before deletion so
        // we know whether to broadcast.
        let privacy = self
            .store
            .get_claim(claim_id)
            .map_err(|e| Error::Tool {
                tool_id: "release_scope".into(),
                message: e.to_string(),
            })?
            .map(|(p, _)| p);

        let released = self
            .store
            .release_claim(claim_id)
            .map_err(|e| Error::Tool {
                tool_id: "release_scope".into(),
                message: e.to_string(),
            })?;

        if released {
            tracing::info!(claim_id = %claim_id, "work_atlas:claim_released");
            if matches!(privacy, Some(Privacy::Public)) {
                let key = format!("claim:{claim_id}");
                self.broadcaster
                    .broadcast(Privacy::Public.app_id(), &key)
                    .await;
            }
        }

        Ok(StepOutput::Json(json!({ "released": released })))
    }
}
