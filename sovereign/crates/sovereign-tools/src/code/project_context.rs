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

use corpus_engine::ProjectDocsStore;

pub struct ProjectContextTool {
    store: Arc<ProjectDocsStore>,
}

impl ProjectContextTool {
    pub fn new(store: Arc<ProjectDocsStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ProjectContextTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "project_context".to_string(),
            name: "Project Context".to_string(),
            description: "Search indexed project documentation (*.md files, \
                          .sovereign/conventions/) for relevant context. \
                          Use to check architectural decisions, coding conventions, \
                          API contracts, or onboarding guides before making changes. \
                          Results are BM25 keyword-ranked — use specific terms from \
                          your change context for best results. \
                          If results seem incomplete, call \
                          read_notes(kinds=[\"reflection\"], query=\"project_context\") \
                          to check for known gaps recorded by previous sessions."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to look for in project docs"
                    }
                },
                "required": ["query"]
            }),
            examples: vec![],
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
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

        let results = self
            .store
            .search(query, 5)
            .await
            .map_err(|e| Error::Tool {
                tool_id: "project_context".to_string(),
                message: e.to_string(),
            })?;

        // Threshold: below this relevance, hint at adding conventions.
        const LOW_RELEVANCE_THRESHOLD: f32 = 0.05;

        let all_low = results.is_empty()
            || results.iter().all(|r| r.relevance < LOW_RELEVANCE_THRESHOLD);

        let hint: Option<&str> = if all_low {
            Some(
                "No relevant conventions found. \
                 Add project-specific guidance to .sovereign/conventions/ \
                 and restart the server to index it.",
            )
        } else {
            None
        };

        let result_values: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                json!({
                    "source": r.source,
                    "content": r.content,
                    "relevance": r.relevance
                })
            })
            .collect();

        Ok(StepOutput::Json(json!({
            "results": result_values,
            "hint": hint
        })))
    }
}
