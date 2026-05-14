//! `release_scope` — drop a Claim. Idempotent: releasing twice is
//! a no-op. Spec §3 forbids history — once released, the claim is
//! gone.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
    ToolExample,
};

use crate::model::Privacy;
use crate::store::WorkAtlasStore;
use crate::tools::broadcast::ClaimBroadcaster;

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

#[async_trait]
impl Tool for ReleaseScopeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "release_scope".to_string(),
            name: "Release Scope".to_string(),
            description: "Drop a previously-declared claim. Idempotent — releasing an \
                          already-released or expired claim returns `released: false` \
                          without erroring. Once released, the claim is gone; spec §3 \
                          forbids history."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "claim_id": {
                        "type": "string",
                        "description": "UUID returned by declare_scope."
                    }
                },
                "required": ["claim_id"]
            }),
            examples: vec![ToolExample {
                situation: "You finished the refactor you claimed earlier.".into(),
                call: json!({ "claim_id": "550e8400-e29b-41d4-a716-446655440000" }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "released": { "type": "boolean" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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

        let released = self.store.release_claim(claim_id).map_err(|e| Error::Tool {
            tool_id: "release_scope".into(),
            message: e.to_string(),
        })?;

        if released {
            tracing::info!(claim_id = %claim_id, "work_atlas:claim_released");
            if matches!(privacy, Some(Privacy::Public)) {
                let key = format!("claim:{claim_id}");
                self.broadcaster.broadcast(Privacy::Public.app_id(), &key).await;
            }
        }

        Ok(StepOutput::Json(json!({ "released": released })))
    }
}
