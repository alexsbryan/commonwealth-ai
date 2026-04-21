//! `read_notes` — retrieve agent working notes.
//!
//! Supports full-text search (BM25), symbol/file filtering, and kind
//! filtering. Without a `query`, results are ordered by recency (newest
//! first). With a `query`, results are ordered by BM25 relevance.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine::{NoteScope, NoteStore, ScopeFilter};

pub struct ReadNotesTool {
    store: Arc<NoteStore>,
}

impl ReadNotesTool {
    pub fn new(store: Arc<NoteStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ReadNotesTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "read_notes".to_string(),
            name: "Read Notes".to_string(),
            description: "Retrieve working notes written by write_note. \
                          Search by keyword (FTS), filter by symbol names, \
                          file paths, or note kind. Call at session start \
                          to recover context from previous sessions, and \
                          before modifying a symbol to find related decisions \
                          or invariants."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Full-text search query. Omit to retrieve recent notes."
                    },
                    "symbols": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Return only notes mentioning any of these symbols"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Return only notes mentioning any of these file paths"
                    },
                    "kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["decision", "attempt", "invariant", "todo", "reflection"]
                        },
                        "description": "Return only notes of these kinds. Use 'reflection' to read tool calibration notes from prior sessions."
                    },
                    "limit": {
                        "type": "integer",
                        "default": 10,
                        "description": "Maximum number of notes to return (capped at 100)"
                    },
                    "scope": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["global", "feature", "session"]
                        },
                        "description": "ATOS scope filter. Omit for legacy behavior (all scopes). \
                                        Common use: scope=['global','feature'] with feature_id \
                                        returns globals plus one feature's notes."
                    },
                    "feature_id": {
                        "type": "string",
                        "description": "Feature id to pair with scope=['feature']. Notes \
                                        in other features are excluded."
                    }
                },
                "required": []
            }),
            examples: vec![
                ToolExample {
                    situation: "You're starting work on a symbol and want to know if any previous session recorded a decision, invariant, or failed attempt about it. Do this BEFORE reading code — it can save you from re-discovering constraints the hard way.".into(),
                    call: serde_json::json!({ "symbols": ["EmbedSlot"] }),
                },
                ToolExample {
                    situation: "You're picking up a session and want to find open tasks or recent decisions relevant to the area you're working on.".into(),
                    call: serde_json::json!({ "query": "embedding GPU Metal", "kinds": ["invariant", "attempt"] }),
                },
                ToolExample {
                    situation: "You want to check what the session_reflection tool has flagged about a tool you're about to use heavily — surface known blind spots before relying on it.".into(),
                    call: serde_json::json!({ "kinds": ["reflection"], "query": "blast_radius" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Instant,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "notes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id":         { "type": "string" },
                                "kind":       { "type": "string" },
                                "content":    { "type": "string" },
                                "symbols":    { "type": "array", "items": { "type": "string" } },
                                "files":      { "type": "array", "items": { "type": "string" } },
                                "scope":      { "type": "string" },
                                "feature_id": { "type": "string" },
                                "created_at": { "type": "integer" }
                            }
                        }
                    },
                    "total": { "type": "integer" }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    /// Signal: count of unretired `todo`-kind notes. Surfaces backlog
    /// the agent might otherwise miss, without needing a feature
    /// context. Silent when the todo list is empty.
    async fn signal(&self) -> Option<String> {
        // Cap the query at 50 — we only need the count for the signal
        // and an order-of-magnitude is enough context. If there are
        // 50+ open todos the agent already knows there's a backlog.
        let open = self.store.open_todos(50).await.ok()?;
        if open.is_empty() {
            return None;
        }
        let n = open.len();
        let suffix = if n >= 50 { "+" } else { "" };
        Some(format!("{n}{suffix} open todo note(s)"))
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let query = params.get("query").and_then(|v| v.as_str());
        let symbols: Vec<String> = params
            .get("symbols")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let files: Vec<String> = params
            .get("files")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let kinds: Vec<String> = params
            .get("kinds")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(10);

        let scopes: Vec<NoteScope> = params
            .get("scope")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().and_then(NoteScope::parse))
                    .collect()
            })
            .unwrap_or_default();
        let feature_id = params
            .get("feature_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let notes = if scopes.is_empty() && feature_id.is_none() {
            self.store
                .read_notes(query, &symbols, &files, &kinds, limit, false)
                .await
        } else {
            let filter = ScopeFilter { scopes, feature_id };
            self.store
                .read_notes_scoped(query, &symbols, &files, &kinds, limit, false, &filter)
                .await
        }
        .map_err(|e| Error::Tool {
            tool_id: "read_notes".to_string(),
            message: e.to_string(),
        })?;

        let total = notes.len();
        let note_values: Vec<serde_json::Value> = notes
            .into_iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "kind": n.kind,
                    "content": n.content,
                    "symbols": n.symbols,
                    "files": n.files,
                    "session_id": n.session_id,
                    "created_at": n.created_at,
                    "scope": n.scope,
                    "feature_id": n.feature_id,
                })
            })
            .collect();

        Ok(StepOutput::Json(json!({
            "notes": note_values,
            "total": total
        })))
    }
}
