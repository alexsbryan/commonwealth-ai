//! `ToolInjector` — merges the ATOS MCP tool descriptors into the
//! `request.tools` array so the model sees them as available
//! without opencode having to advertise them explicitly.
//!
//! The model's view: these tools were just there when the
//! conversation started. From the model's perspective that's
//! indistinguishable from any other provider-advertised tool.
//! ToolInjector is how ATOS gets that property.
//!
//! Dedup policy: an opencode-registered tool with the same name
//! as an ATOS tool wins. The client's explicit registration is
//! treated as authoritative — if Yara's team has a local
//! `write_note` that captures their conventions, they're not
//! overridden.

use async_trait::async_trait;
use serde_json::json;

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext};
use crate::openai_types::{ChatCompletionRequest, ToolDefinition, ToolFunction};

pub struct ToolInjector;

impl ToolInjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolInjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for ToolInjector {
    fn id(&self) -> &'static str {
        "tool_injector"
    }

    async fn process(
        &self,
        request: &mut ChatCompletionRequest,
        _session: &mut MiddlewareSession,
        _ctx: &PipelineContext,
    ) -> Result<(), MiddlewareError> {
        let atos_tools = atos_tool_descriptors();
        let existing = request.tools.get_or_insert_with(Vec::new);

        // Collect existing names for dedup.
        let existing_names: std::collections::HashSet<String> = existing
            .iter()
            .map(|t| t.function.name.clone())
            .collect();
        let mut appended = 0;
        for tool in atos_tools {
            if existing_names.contains(&tool.function.name) {
                continue;
            }
            existing.push(tool);
            appended += 1;
        }
        tracing::debug!(
            appended,
            total = existing.len(),
            "tool_injector: appended ATOS tool descriptors"
        );
        Ok(())
    }
}

/// Static list of ATOS MCP tools advertised to every pipeline
/// session. Kept in sync by hand with `sovereign-tools/src/code/` —
/// M5 will drop the duplication by pulling descriptors from the
/// tool-handler code directly.
pub fn atos_tool_descriptors() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "read_notes".into(),
                description: Some(
                    "Retrieve notes by symbol, file, kind, or full-text query. Use at \
                     session start + before modifying a symbol."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query":     { "type": "string" },
                        "symbols":   { "type": "array", "items": { "type": "string" } },
                        "files":     { "type": "array", "items": { "type": "string" } },
                        "kinds":     { "type": "array", "items": { "type": "string" } },
                        "scope":     { "type": "array", "items": { "type": "string" } },
                        "feature_id":{ "type": "string" },
                        "limit":     { "type": "integer", "default": 10 }
                    },
                    "required": []
                }),
            },
        },
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "read_note_by_id".into(),
                description: Some("Fetch one note row by its UUID.".into()),
                parameters: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
        },
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "read_note_digest".into(),
                description: Some(
                    "Markdown digest of scope/feature/kinds-filtered notes. Use at \
                     session start or post-compaction."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "scope":     { "type": "array", "items": { "type": "string" } },
                        "feature_id":{ "type": "string" },
                        "kinds":     { "type": "array", "items": { "type": "string" } },
                        "limit":     { "type": "integer", "default": 100 }
                    },
                    "required": []
                }),
            },
        },
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "write_note".into(),
                description: Some(
                    "Persist a decision/attempt/invariant/todo/uncertainty/postmortem_pointer \
                     note. Scope defaults to 'global'; pass scope='feature' + feature_id to \
                     tag to an ATOS feature."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "kind":       {
                            "type": "string",
                            "enum": ["decision","attempt","invariant","todo","uncertainty","postmortem_pointer"]
                        },
                        "content":    { "type": "string" },
                        "symbols":    { "type": "array", "items": { "type": "string" } },
                        "files":      { "type": "array", "items": { "type": "string" } },
                        "scope":      { "type": "string", "enum": ["global","feature","session"] },
                        "feature_id": { "type": "string" }
                    },
                    "required": ["kind", "content"]
                }),
            },
        },
        ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "write_redteam_finding".into(),
                description: Some(
                    "Record a red-team review finding. Only valid from mode=redteam sessions."
                        .into(),
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "feature_id": { "type": "string" },
                        "invariant":  { "type": "string" },
                        "status":     {
                            "type": "string",
                            "enum": ["violated","potentially_violated","not_found"]
                        },
                        "evidence":   { "type": "string" },
                        "confidence": { "type": "string", "enum": ["high","medium","low"] },
                        "files":      { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["feature_id", "invariant", "status", "confidence"]
                }),
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::{ChatCompletionRequest, ChatMessage};

    fn minimal_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: None,
            messages: vec![ChatMessage::new("user", "hi")],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            tools: None,
            tool_choice: None,
            oicp: None,
        }
    }

    fn ctx() -> PipelineContext {
        PipelineContext {
            pipeline_name: "test".into(),
            model_id: "qwen-27b-coder".into(),
            context_config: Default::default(),
            feature_id: Some("fx".into()),
            session_id: Some("s".into()),
            repo_root: std::env::temp_dir(),
        }
    }

    #[tokio::test]
    async fn injects_all_atos_tools_when_absent() {
        let inj = ToolInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| t.function.name == "read_notes"));
        assert!(tools.iter().any(|t| t.function.name == "write_note"));
        assert!(tools.iter().any(|t| t.function.name == "read_note_digest"));
        assert!(tools.iter().any(|t| t.function.name == "read_note_by_id"));
        assert!(tools.iter().any(|t| t.function.name == "write_redteam_finding"));
    }

    #[tokio::test]
    async fn client_registered_tool_wins_on_name_collision() {
        let inj = ToolInjector::new();
        let mut req = minimal_request();
        req.tools = Some(vec![ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "write_note".into(),
                description: Some("client override".into()),
                parameters: json!({"type":"object","properties":{"custom":{"type":"string"}}}),
            },
        }]);
        let mut session = MiddlewareSession::default();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let tools = req.tools.unwrap();
        let wn = tools.iter().find(|t| t.function.name == "write_note").unwrap();
        assert_eq!(wn.function.description.as_deref(), Some("client override"));
    }

    #[tokio::test]
    async fn idempotent_across_repeated_calls() {
        // Opencode can hit the same handler twice on a streaming +
        // tool-roundtrip pair. Each call should net zero duplicates.
        let inj = ToolInjector::new();
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let first_len = req.tools.as_ref().unwrap().len();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let second_len = req.tools.as_ref().unwrap().len();
        assert_eq!(first_len, second_len);
    }
}
