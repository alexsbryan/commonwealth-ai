// SPDX-License-Identifier: AGPL-3.0-or-later
//! `project_context` — search indexed project documentation.
//!
//! Answers questions like "what are the conventions for error handling?"
//! or "how does the architecture handle auth?" by searching `*.md` files
//! indexed at server startup and kept up to date by `ProjectIndexWatcher`.
//!
//! Results are BM25-ranked; when no relevant docs are found, a hint
//! suggests adding conventions to `.sovereign/conventions/`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_atos::FeatureStore;
use corpus_engine_notes::ProjectDocsStore;

pub struct ProjectContextTool {
    store: Arc<ProjectDocsStore>,
    /// When set and the caller passes `feature_id`, the feature's charter
    /// and SOVEREIGN.md are prepended to the results as a synthetic
    /// top-relevance entry. Absent → feature_id is simply ignored.
    features: Option<Arc<FeatureStore>>,
}

impl ProjectContextTool {
    pub fn new(store: Arc<ProjectDocsStore>) -> Self {
        Self {
            store,
            features: None,
        }
    }

    pub fn with_features(mut self, features: Arc<FeatureStore>) -> Self {
        self.features = Some(features);
        self
    }
}

#[async_trait]
impl Tool for ProjectContextTool {
    fn descriptor(&self) -> ToolDescriptor {
        sovereign_core::tool_manifest::require("project_context").to_descriptor()
    }

    fn required_permissions(&self) -> Vec<Permission> {
        sovereign_core::tool_manifest::require("project_context")
            .permissions
            .clone()
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        params
            .get("query")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::InvalidInput("project_context requires 'query'".to_string()))?;
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing 'query'".to_string()))?;

        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let results = self.store.search(query, 5).await.map_err(|e| Error::Tool {
            tool_id: "project_context".to_string(),
            message: e.to_string(),
        })?;

        // Threshold: below this relevance, hint at adding conventions.
        const LOW_RELEVANCE_THRESHOLD: f32 = 0.05;

        let all_low = results.is_empty()
            || results
                .iter()
                .all(|r| r.relevance < LOW_RELEVANCE_THRESHOLD);

        // Resolve the feature charter if requested. A missing feature is
        // communicated via a hint rather than an error so a typo doesn't
        // derail the whole query.
        let mut result_values: Vec<serde_json::Value> = Vec::new();
        let mut feature_hint: Option<String> = None;

        if let (Some(fid), Some(fs)) = (feature_id, self.features.as_ref()) {
            match fs.get(fid).await {
                Ok(Some(feat)) => {
                    let content = if feat.sovereign_md.trim().is_empty() {
                        feat.charter_md.clone()
                    } else {
                        format!("{}\n\n---\n\n{}", feat.charter_md, feat.sovereign_md)
                    };
                    result_values.push(json!({
                        "source": format!("features/{}/SOVEREIGN.md", feat.id),
                        "content": content,
                        "relevance": 1.0,
                    }));
                }
                Ok(None) => {
                    feature_hint = Some(format!(
                        "feature_id='{fid}' is not provisioned — continuing with normal BM25 search"
                    ));
                }
                Err(e) => {
                    feature_hint = Some(format!(
                        "feature lookup for '{fid}' failed: {e} — continuing with normal BM25 search"
                    ));
                }
            }
        }

        result_values.extend(results.into_iter().map(|r| {
            json!({
                "source": r.source,
                "content": r.content,
                "relevance": r.relevance
            })
        }));

        let hint: Option<String> = match (all_low, feature_hint) {
            (true, Some(fh)) => Some(format!(
                "{fh}. No relevant conventions found either — add guidance to .sovereign/conventions/ and restart."
            )),
            (true, None) => Some(
                "No relevant conventions found. \
                 Add project-specific guidance to .sovereign/conventions/ \
                 and restart the server to index it."
                    .to_string(),
            ),
            (false, Some(fh)) => Some(fh),
            (false, None) => None,
        };

        // Include a basic index health block so agents can distinguish
        // "no relevant docs" from "no index at all".
        let doc_count = self.store.file_count().await.unwrap_or(0);
        let index_health = if doc_count == 0 {
            json!({
                "present": false,
                "staleness": "absent",
                "hint": "Project index not built or empty. \
                         Run `sovereign index project` to enable project_context search."
            })
        } else {
            json!({
                "present": true,
                "staleness": "unknown",
                "document_count": doc_count,
                "hint": null
            })
        };

        Ok(StepOutput::Json(json!({
            "results": result_values,
            "hint": hint,
            "index_health": index_health
        })))
    }
}
