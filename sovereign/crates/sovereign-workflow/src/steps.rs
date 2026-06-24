// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `Step` trait and its three P1 implementations — a deterministic
//! `transform`, a `model` call (daemon-routed inference), and a `tool` call
//! (built-in or MCP) — plus the registry that resolves a `uses` string into a
//! concrete step. Inference + tools are *injected* (core traits), so this stays
//! decoupled from any provider.

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_core::error::{Error, Result};
use sovereign_core::oicp::{InferenceRequirements, LatencyClass};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Effect, Speed, StepOutput, ToolContext};

use crate::kind::StepKind;
use crate::model::{Artifact, ResolvedArgs, ResourceNeed, StepDescriptor};

/// Per-step run context (item identity for tracing; P2 adds an approval channel
/// + the content-cache handle).
#[derive(Debug, Clone, Default)]
pub struct StepCtx {
    pub item_id: String,
}

/// A typed transform: takes resolved args, returns one `Artifact`.
#[async_trait]
pub trait Step: Send + Sync {
    fn descriptor(&self) -> StepDescriptor;
    async fn run(&self, args: &ResolvedArgs, ctx: &StepCtx) -> Result<Artifact>;
}

// ─── transform: deterministic CPU step ────────────────────────

pub struct TransformStep {
    name: String,
}

#[async_trait]
impl Step for TransformStep {
    fn descriptor(&self) -> StepDescriptor {
        StepDescriptor {
            id: format!("transform:{}", self.name),
            name: self.name.clone(),
            description: "deterministic text transform".into(),
            resources: ResourceNeed::None,
            effect: Effect::Read,
            deterministic: true,
        }
    }

    async fn run(&self, args: &ResolvedArgs, _ctx: &StepCtx) -> Result<Artifact> {
        // `json` is a SHAPE transform: emit the resolved `params` object as a
        // `Json` artifact (the inverse of a text transform). With a lone
        // `{step.output}` ref splicing as a value (see `template::resolve_value`),
        // this assembles an envelope — e.g. `{ schema_version = 1,
        // questions_by_chapter = "{atoms.output}" }` — from upstream outputs,
        // purely as data. Zero domain knowledge: it's how a workflow wraps a
        // collection in the `Phase1Output`-shaped object the real pipeline writes.
        if self.name == "json" {
            let value = args.params.clone().ok_or_else(|| {
                Error::Execution("transform:json needs a `params` object to shape".into())
            })?;
            return Ok(Artifact::new("json", StepOutput::Json(value)));
        }
        let input = args.input.clone().unwrap_or_default();
        let out = match self.name.as_str() {
            "upper" => input.to_uppercase(),
            "lower" => input.to_lowercase(),
            "trim" => input.trim().to_string(),
            "identity" => input,
            other => return Err(Error::Execution(format!("unknown transform `{other}`"))),
        };
        Ok(Artifact::new("text", StepOutput::Text(out)))
    }
}

// ─── model: daemon-routed inference step ──────────────────────

pub struct ModelStep {
    provider: Arc<dyn InferenceProvider>,
    latency: LatencyClass,
}

#[async_trait]
impl Step for ModelStep {
    fn descriptor(&self) -> StepDescriptor {
        let label = latency_label(self.latency);
        StepDescriptor {
            id: format!("model:{label}"),
            name: format!("model:{label}"),
            description: "local-model completion (routed to the daemon)".into(),
            resources: ResourceNeed::Inference,
            // A model call produces output but no external side effect, so it's
            // cacheable: the cache makes a non-deterministic call effectively
            // deterministic across re-runs (the resume property we want).
            effect: Effect::Read,
            deterministic: false,
        }
    }

    async fn run(&self, args: &ResolvedArgs, _ctx: &StepCtx) -> Result<Artifact> {
        let prompt = args
            .prompt
            .clone()
            .ok_or_else(|| Error::Execution("a `model:` step requires a `prompt`".into()))?;
        // A `system_file` loads the system prompt from disk — the bespoke `.md`
        // enrichment prompts referenced as data, not re-typed — overriding inline
        // `system`. A read failure is loud (the prompt is load-bearing).
        let system_message = match &args.system_file {
            Some(path) => Some(std::fs::read_to_string(path).map_err(|e| {
                Error::Execution(format!("model: read system_file `{path}`: {e}"))
            })?),
            None => args.system.clone(),
        };
        // Build a protocol-native request: the OICP envelope carries the
        // latency class — the standard signal the daemon's slot picker routes on
        // (`pick_slot_for_oicp`). `preferred_speed` is set coherently for any
        // pre-OICP consumer, but the OICP envelope is the primary signal, so we
        // don't rely on the provider's `Speed`→`latency_class` rescue.
        let request = CompletionRequest {
            prompt,
            system_message,
            preferred_speed: speed_for(self.latency),
            max_tokens: None,
            temperature: Some(0.7),
            // Structured output / grammar are general `model:` primitives,
            // declared as data on the step — so an extraction (enrichment atoms,
            // a classification) constrains its output shape in TOML rather than
            // parsing free text.
            structured_output: args.structured_output.clone(),
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: Some(InferenceRequirements::new().with_latency_class(self.latency)),
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: args.grammar.clone(),
        };
        let resp = self.provider.complete(&request).await?;
        // A structured step (one that declared a `structured_output` schema or a
        // `grammar`) returns a parsed `Json` artifact, so downstream steps
        // compose on the structure — `for_each` over it, store it — instead of
        // re-parsing a string. Falls back to Text (with a warn) if the model's
        // output isn't valid JSON, so a flaky model degrades visibly.
        // `raw_output` keeps the grammar constraint but skips auto-parsing, so a
        // downstream step (e.g. `pipeline_parse`) sees the exact model text.
        let structured = (request.structured_output.is_some() || request.lark_grammar.is_some())
            && !args.raw_output;
        let output = if structured {
            match serde_json::from_str::<serde_json::Value>(resp.text.trim()) {
                Ok(v) => StepOutput::Json(v),
                Err(e) => {
                    tracing::warn!(
                        target: "workflow", error = %e,
                        "model: structured output is not valid JSON — returning text"
                    );
                    StepOutput::Text(resp.text)
                }
            }
        } else {
            StepOutput::Text(resp.text)
        };
        let type_tag = if matches!(output, StepOutput::Json(_)) {
            "json"
        } else {
            "text"
        };
        Ok(Artifact::new(type_tag, output))
    }
}

/// OICP latency class → the legacy `Speed` slot, for the `preferred_speed` field
/// any pre-OICP consumer still reads. The OICP envelope is the primary routing
/// signal; this just keeps the request struct internally coherent.
fn speed_for(latency: LatencyClass) -> Speed {
    match latency {
        LatencyClass::Fast => Speed::Fast,
        LatencyClass::Normal => Speed::Medium,
        LatencyClass::Extended => Speed::Slow,
    }
}

fn latency_label(latency: LatencyClass) -> &'static str {
    match latency {
        LatencyClass::Fast => "fast",
        LatencyClass::Normal => "normal",
        LatencyClass::Extended => "extended",
    }
}

// ─── embed: daemon-routed embedding step ──────────────────────

pub struct EmbedStep {
    provider: Arc<dyn InferenceProvider>,
    model: String,
}

#[async_trait]
impl Step for EmbedStep {
    fn descriptor(&self) -> StepDescriptor {
        StepDescriptor {
            id: format!("embed:{}", self.model),
            name: format!("embed:{}", self.model),
            description: "embedding of the input text (routed to the daemon)".into(),
            resources: ResourceNeed::Inference,
            effect: Effect::Read,
            deterministic: false,
        }
    }

    async fn run(&self, args: &ResolvedArgs, _ctx: &StepCtx) -> Result<Artifact> {
        let text = args
            .input
            .clone()
            .ok_or_else(|| Error::Execution("an `embed:` step requires an `input`".into()))?;
        let vector = self.provider.embed(&text).await?;
        let arr = serde_json::Value::Array(
            vector
                .into_iter()
                .map(|f| serde_json::Value::from(f as f64))
                .collect(),
        );
        Ok(Artifact::new("embedding", StepOutput::Json(arr)))
    }
}

// ─── tool: built-in or MCP tool step ──────────────────────────

pub struct ToolStep {
    tools: Arc<ToolRegistry>,
    tool_id: String,
}

#[async_trait]
impl Step for ToolStep {
    fn descriptor(&self) -> StepDescriptor {
        // Delegate the effect to the wrapped tool so the cache knows whether
        // this step is safe to skip (a `Read` tool like read_memo) or must
        // always run (a `Write` tool like write_note). An unresolvable tool is
        // conservatively `Write` — never cached.
        let effect = self
            .tools
            .get(&self.tool_id)
            .map(|t| t.descriptor().effect)
            .unwrap_or(Effect::Write);
        StepDescriptor {
            id: self.tool_id.clone(),
            name: self.tool_id.clone(),
            description: "built-in or MCP tool call".into(),
            resources: ResourceNeed::Tool,
            effect,
            deterministic: false,
        }
    }

    async fn run(&self, args: &ResolvedArgs, _ctx: &StepCtx) -> Result<Artifact> {
        let params = args.params.clone().unwrap_or_else(|| serde_json::json!({}));
        // P1 runs without a per-step approval gate: the user authored and ran
        // the workflow (add-time trust, like the CLI chat's AutoApprove). P2
        // can thread an ApprovalChannel for Write/Network steps.
        let ctx = ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        };
        let tool = self.tools.get(&self.tool_id)?;
        let output = tool.execute(&params, &ctx).await?;
        Ok(Artifact::new("tool", output))
    }
}

// ─── registry: `uses` string → concrete Step ──────────────────

/// Resolves a workflow's `uses` strings against the injected inference provider
/// + tool registry. The single subsumed vocabulary the spec calls for: the same
/// place a `model:` slot, a `tool:` id, an `mcp:` tool, and a `transform:` all
/// become a `Step`.
pub struct StepRegistry {
    /// `None` when no daemon is wired — a workflow with no `model:` steps still
    /// runs; a `model:` step then errors clearly instead of panicking.
    inference: Option<Arc<dyn InferenceProvider>>,
    tools: Arc<ToolRegistry>,
}

impl StepRegistry {
    pub fn new(inference: Option<Arc<dyn InferenceProvider>>, tools: Arc<ToolRegistry>) -> Self {
        Self { inference, tools }
    }

    /// Resolve a typed `StepKind` into a concrete `Step`, injecting the daemon
    /// provider / tool registry. The `uses` string was parsed to a `StepKind` at
    /// the workflow boundary (see `kind.rs`), so this is an exhaustive match over
    /// the taxonomy — no string arms (ARCH §2.1).
    pub fn resolve(&self, kind: &StepKind) -> Result<Arc<dyn Step>> {
        match kind {
            StepKind::Model { latency } => {
                let provider = self.inference.as_ref().ok_or_else(|| {
                    Error::Execution(
                        "a `model:` step needs the daemon — none is wired (run `sovereign daemon`)"
                            .into(),
                    )
                })?;
                Ok(Arc::new(ModelStep {
                    provider: Arc::clone(provider),
                    latency: *latency,
                }))
            }
            StepKind::Embed { model } => {
                let provider = self.inference.as_ref().ok_or_else(|| {
                    Error::Execution("an `embed:` step needs the daemon — none is wired".into())
                })?;
                Ok(Arc::new(EmbedStep {
                    provider: Arc::clone(provider),
                    model: model.clone(),
                }))
            }
            StepKind::Tool { id } => Ok(Arc::new(ToolStep {
                tools: Arc::clone(&self.tools),
                tool_id: id.clone(),
            })),
            StepKind::Mcp { server, tool } => Ok(Arc::new(ToolStep {
                tools: Arc::clone(&self.tools),
                tool_id: format!("mcp_{server}_{tool}"),
            })),
            StepKind::Transform { name } => Ok(Arc::new(TransformStep {
                name: name.clone(),
            })),
        }
    }
}
