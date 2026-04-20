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

use corpus_engine::FeatureStore;

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
        ToolDescriptor {
            id: "archive_feature".to_string(),
            name: "Archive Feature".to_string(),
            description:
                "Mark an ATOS feature as archived. Notes tagged to the feature remain \
                 queryable via read_notes with scope=['feature'] + feature_id but stop \
                 being injected into fresh sessions. Use after the compliance review \
                 passes and any promotable notes have been promoted to global scope."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Feature id (as returned by provision_feature)."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why the feature is archived (e.g. 'shipped v0.5', \
                                        'abandoned in favor of <other>')."
                    }
                },
                "required": ["id", "reason"]
            }),
            examples: vec![ToolExample {
                situation: "Milestone 1 of the atos-version-flag feature passed review and the \
                            operator asked you to wrap it up. Archive it and move on."
                    .into(),
                call: serde_json::json!({
                    "id": "atos-version-flag",
                    "reason": "shipped; --version flag lives in atos_cmd.rs"
                }),
            }],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
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

        let ok = self.store.archive(id, reason).await.map_err(|e| Error::Tool {
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
