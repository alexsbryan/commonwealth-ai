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
//!
//! ## Manifest source of truth (D1)
//!
//! The tool descriptors are pulled from
//! [`sovereign_tools::manifest::atos_critical_descriptors`], which
//! constructs every tool once with throwaway in-memory stores and
//! calls `Tool::descriptor()`. That eliminates the hand-maintained
//! parallel JSON-schema block this module used to carry — which had
//! already drifted from the real parameter shapes by the time the
//! Phase 3 refactor landed.
//!
//! Adding a new ATOS-critical tool: add its id to the `IDS` array in
//! `sovereign_tools::manifest::atos_critical_descriptors`; the schema
//! flows in automatically from the tool's actual descriptor().

use async_trait::async_trait;

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

/// ATOS MCP tools advertised on every pipeline session. Pulled from
/// the shared sovereign-tools manifest — the tool's real
/// `descriptor()` is the schema, so there is no hand-maintained
/// parameters JSON to drift.
///
/// When the tool declares an `output_schema` we append a short hint
/// to the description so the agent sees which keys it can reference
/// in a follow-up tool call's params. OpenAI's function-calling
/// schema has no native `output_schema` slot; description is the
/// only channel that reaches the model.
pub fn atos_tool_descriptors() -> Vec<ToolDefinition> {
    sovereign_tools::manifest::atos_critical_descriptors()
        .into_iter()
        .map(|d| {
            let description = match &d.output_schema {
                Some(schema) => match schema
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .filter(|k| !k.is_empty())
                {
                    Some(keys) => format!(
                        "{}\n\nOutput keys: {}",
                        d.description,
                        keys.join(", ")
                    ),
                    None => d.description.clone(),
                },
                None => d.description.clone(),
            };
            ToolDefinition {
                kind: "function".into(),
                function: ToolFunction {
                    name: d.id,
                    description: Some(description),
                    parameters: d.parameters,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai_types::{ChatCompletionRequest, ChatMessage};
    use serde_json::json;

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
            response_format: None,
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

    #[tokio::test]
    async fn descriptor_schemas_match_registry_source() {
        // D1 drift-seal: every injected tool's `parameters` JSON
        // schema is whatever the tool's real Tool::descriptor()
        // returns. Any mismatch between this module's output and the
        // registry is a compile failure (the field is cloned
        // directly), so this test just verifies the pipeline is
        // wired — if an id went missing, we'd see a shorter list.
        let defs = atos_tool_descriptors();
        assert_eq!(
            defs.len(),
            sovereign_tools::manifest::atos_critical_descriptors().len(),
            "tool_injector output count must match the manifest; \
             if you added an ATOS-critical tool, update the IDS list in \
             sovereign_tools::manifest::atos_critical_descriptors"
        );
    }
}
