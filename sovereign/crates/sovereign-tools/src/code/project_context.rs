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
            examples: vec![
                ToolExample {
                    situation: "You're about to implement something and want to check whether the project has established conventions for it before you guess or invent your own. Do this before writing any code.".into(),
                    call: serde_json::json!({ "query": "error handling conventions" }),
                },
                ToolExample {
                    situation: "You're unsure about the architectural boundary between two subsystems. Pull the documented decisions rather than inferring from code.".into(),
                    call: serde_json::json!({ "query": "corpus engine vs sovereign tools boundary" }),
                },
                ToolExample {
                    situation: "You got empty or low-relevance results from a code search. Check here — the answer may be in conventions docs rather than source code.".into(),
                    call: serde_json::json!({ "query": "testing strategy integration vs unit" }),
                },
            ],
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
