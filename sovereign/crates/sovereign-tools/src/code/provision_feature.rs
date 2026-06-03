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
        ToolDescriptor {
            id: "provision_feature".to_string(),
            name: "Provision Feature".to_string(),
            description: "Create an ATOS feature charter. The feature holds the human-approved \
                 spec (charter_md), per-feature invariants (sovereign_md), and a \
                 machine-checkable stop condition. Returns the feature id to pass \
                 back to `start-milestone`. Fails if the id already exists."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Slug-style identifier, e.g. 'atos-version-flag'. Must be unique."
                    },
                    "title": {
                        "type": "string",
                        "description": "Human-readable title shown in status views."
                    },
                    "charter_md": {
                        "type": "string",
                        "description": "Approved specification in markdown. Includes the sections \
                                        required by the ATOS spec gate: integration points, \
                                        schema additions, files, invariants, test plan, milestones."
                    },
                    "sovereign_md": {
                        "type": "string",
                        "description": "Optional per-feature invariants and conventions surfaced \
                                        via project_context when feature_id is set."
                    },
                    "stop_condition": {
                        "type": "string",
                        "description": "Shell command whose zero-exit status signals the feature \
                                        is complete. Run by `sovereign atos end-milestone`."
                    }
                },
                "required": ["id", "title", "charter_md"]
            }),
            examples: vec![ToolExample {
                situation:
                    "The operator has approved a spec and wants to kick off ATOS milestone 1. \
                            Call this before any implementation tool to register the feature."
                        .into(),
                call: serde_json::json!({
                    "id": "atos-version-flag",
                    "title": "Add --version flag to `sovereign atos`",
                    "charter_md": "# atos-version-flag\n\n## Invariants\n- Output must match regex ^atos [0-9]+\\.",
                    "stop_condition": "cargo run -p sovereign-cli -- atos --version | grep -E '^atos [0-9]+\\.'"
                }),
            }],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "id":    { "type": "string" },
                    "title": { "type": "string" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
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
