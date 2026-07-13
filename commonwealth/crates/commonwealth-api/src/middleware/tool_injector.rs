// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! ## Source of truth (post-manifest split, 2026-05-22)
//!
//! Tool descriptors are now injected at construction time rather than
//! pulled from a global static. The daemon assembling the pipeline
//! passes the subset of its `ToolRegistry::descriptors()` it wants
//! advertised (typically the note-handling tools); tests pass `vec![]`
//! or a hand-built fixture set.
//!
//! Before: `sovereign_tools::manifest::atos_critical_descriptors()`
//! constructed every tool once with throwaway in-memory stores and
//! filtered by id. That forced commonwealth-api to depend on
//! `sovereign-tools[treesitter]` (the code-intel tools live behind
//! that feature) and pulled five tree-sitter grammar crates into
//! every binary downstream of commonwealth-api. The injected-descriptor
//! shape removes that transitive cost AND eliminates the §1 doc-drift
//! risk of having two parallel descriptor sources.

use async_trait::async_trait;

use sovereign_core::types::ToolDescriptor;

use super::{Middleware, MiddlewareError, MiddlewareSession, PipelineContext};
use crate::openai_types::{ChatCompletionRequest, ToolDefinition, ToolFunction};

/// Descriptors the injector advertises on every pipeline request.
///
/// Empty is fine — the middleware becomes a no-op, which is what
/// tests that don't care about tool advertisement want. Production
/// daemons feed in the ATOS-critical subset of their `ToolRegistry`.
pub struct ToolInjector {
    descriptors: Vec<ToolDescriptor>,
}

impl ToolInjector {
    pub fn new(descriptors: Vec<ToolDescriptor>) -> Self {
        Self { descriptors }
    }

    /// Convenience constructor for tests/stubs.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}

impl Default for ToolInjector {
    fn default() -> Self {
        Self::empty()
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
        let atos_tools = render_descriptors(&self.descriptors);
        let existing = request.tools.get_or_insert_with(Vec::new);

        // Collect existing names for dedup.
        let existing_names: std::collections::HashSet<String> =
            existing.iter().map(|t| t.function.name.clone()).collect();
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

/// Render injected `ToolDescriptor`s into OpenAI-shaped `ToolDefinition`s.
///
/// When the source descriptor declares an `output_schema` we append a
/// short hint to the description so the agent sees which keys it can
/// reference in a follow-up tool call's params. OpenAI's
/// function-calling schema has no native `output_schema` slot;
/// description is the only channel that reaches the model.
fn render_descriptors(descriptors: &[ToolDescriptor]) -> Vec<ToolDefinition> {
    descriptors
        .iter()
        .map(|d| {
            let description = match &d.output_schema {
                Some(schema) => match schema
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .filter(|k| !k.is_empty())
                {
                    Some(keys) => format!("{}\n\nOutput keys: {}", d.description, keys.join(", ")),
                    None => d.description.clone(),
                },
                None => d.description.clone(),
            };
            ToolDefinition {
                kind: "function".into(),
                function: ToolFunction {
                    name: d.id.clone(),
                    description: Some(description),
                    parameters: d.parameters.clone(),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::shared::fixtures::{ctx_with, minimal_request};
    use serde_json::json;

    fn ctx() -> PipelineContext {
        ctx_with(Some("fx"), std::env::temp_dir())
    }

    /// Build a fixture descriptor with the given id. The injector
    /// only reads `id`, `description`, `parameters`, and
    /// `output_schema`; the rest are filled with neutral defaults.
    fn descriptor(id: &str) -> ToolDescriptor {
        ToolDescriptor {
            id: id.into(),
            name: id.into(),
            description: format!("test descriptor for {id}"),
            parameters: json!({"type": "object", "properties": {}}),
            examples: Vec::new(),
            effect: sovereign_core::types::Effect::Write,
            idempotency: sovereign_core::types::Idempotency::Idempotent,
            latency: sovereign_core::types::Latency::Fast,
            scope: sovereign_core::types::Scope::Persistent,
            output_schema: None,
        }
    }

    fn critical_set() -> Vec<ToolDescriptor> {
        [
            "notes",
            "note",
            "read_note_digest",
            "read_note_by_id",
            "write_redteam_finding",
        ]
        .iter()
        .map(|id| descriptor(id))
        .collect()
    }

    #[tokio::test]
    async fn injects_all_atos_tools_when_absent() {
        let inj = ToolInjector::new(critical_set());
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let tools = req.tools.unwrap();
        assert!(tools.iter().any(|t| t.function.name == "notes"));
        assert!(tools.iter().any(|t| t.function.name == "note"));
        assert!(tools.iter().any(|t| t.function.name == "read_note_digest"));
        assert!(tools.iter().any(|t| t.function.name == "read_note_by_id"));
        assert!(tools
            .iter()
            .any(|t| t.function.name == "write_redteam_finding"));
    }

    #[tokio::test]
    async fn client_registered_tool_wins_on_name_collision() {
        let inj = ToolInjector::new(critical_set());
        let mut req = minimal_request();
        req.tools = Some(vec![ToolDefinition {
            kind: "function".into(),
            function: ToolFunction {
                name: "note".into(),
                description: Some("client override".into()),
                parameters: json!({"type":"object","properties":{"custom":{"type":"string"}}}),
            },
        }]);
        let mut session = MiddlewareSession::default();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let tools = req.tools.unwrap();
        let wn = tools.iter().find(|t| t.function.name == "note").unwrap();
        assert_eq!(wn.function.description.as_deref(), Some("client override"));
    }

    #[tokio::test]
    async fn idempotent_across_repeated_calls() {
        // Opencode can hit the same handler twice on a streaming +
        // tool-roundtrip pair. Each call should net zero duplicates.
        let inj = ToolInjector::new(critical_set());
        let mut req = minimal_request();
        let mut session = MiddlewareSession::default();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let first_len = req.tools.as_ref().unwrap().len();
        inj.process(&mut req, &mut session, &ctx()).await.unwrap();
        let second_len = req.tools.as_ref().unwrap().len();
        assert_eq!(first_len, second_len);
    }

    #[tokio::test]
    async fn renders_one_definition_per_descriptor() {
        // Source-of-truth seal: render_descriptors maps each input
        // descriptor to exactly one ToolDefinition (no dedup at this
        // layer; dedup happens against the request's existing tools).
        let descriptors = critical_set();
        let defs = render_descriptors(&descriptors);
        assert_eq!(defs.len(), descriptors.len());
        for (d, def) in descriptors.iter().zip(defs.iter()) {
            assert_eq!(def.function.name, d.id);
        }
    }
}
