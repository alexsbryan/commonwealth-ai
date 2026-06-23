// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `Step` trait and its three P1 implementations — a deterministic
//! `transform`, a `model` call (daemon-routed inference), and a `tool` call
//! (built-in or MCP) — plus the registry that resolves a `uses` string into a
//! concrete step. Inference + tools are *injected* (core traits), so this stays
//! decoupled from any provider.

use std::sync::Arc;

use async_trait::async_trait;
use sovereign_core::error::{Error, Result};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Effect, Speed, StepOutput, ToolContext};

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
        let input = args.input.clone().unwrap_or_default();
        let out = match self.name.as_str() {
            "upper" => input.to_uppercase(),
            "lower" => input.to_lowercase(),
            "trim" => input.trim().to_string(),
            "identity" => input,
            other => return Err(Error::Execution(format!("unknown transform `{other}`"))),
        };
        Ok(Artifact {
            type_tag: "text".into(),
            output: StepOutput::Text(out),
        })
    }
}

// ─── model: daemon-routed inference step ──────────────────────

pub struct ModelStep {
    provider: Arc<dyn InferenceProvider>,
    speed: Speed,
    slot: String,
}

#[async_trait]
impl Step for ModelStep {
    fn descriptor(&self) -> StepDescriptor {
        StepDescriptor {
            id: format!("model:{}", self.slot),
            name: format!("model:{}", self.slot),
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
        // Mirror the executor's Reason-step request (executor.rs:501). P1 maps
        // the model slot → preferred_speed; the `resources` hint + oicp
        // scheduling are P2.
        let request = CompletionRequest {
            prompt,
            system_message: args.system.clone(),
            preferred_speed: self.speed,
            max_tokens: None,
            temperature: Some(0.7),
            structured_output: None,
            think_budget: None,
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
        };
        let resp = self.provider.complete(&request).await?;
        Ok(Artifact {
            type_tag: "text".into(),
            output: StepOutput::Text(resp.text),
        })
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
        Ok(Artifact {
            type_tag: "embedding".into(),
            output: StepOutput::Json(arr),
        })
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
        Ok(Artifact {
            type_tag: "tool".into(),
            output,
        })
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

    /// `model:<slot>` | `tool:<id>` | `mcp:<server>:<tool>` | `transform:<name>`.
    pub fn resolve(&self, uses: &str) -> Result<Arc<dyn Step>> {
        let (kind, rest) = uses.split_once(':').ok_or_else(|| {
            Error::Execution(format!("step `uses` must be `<kind>:<name>` — got `{uses}`"))
        })?;
        match kind {
            "model" => {
                let provider = self.inference.as_ref().ok_or_else(|| {
                    Error::Execution(
                        "a `model:` step needs the daemon — none is wired (run `sovereign daemon`)"
                            .into(),
                    )
                })?;
                let speed = match rest {
                    "fast" => Speed::Fast,
                    "thoughtful" | "slow" => Speed::Slow,
                    _ => Speed::Medium,
                };
                Ok(Arc::new(ModelStep {
                    provider: Arc::clone(provider),
                    speed,
                    slot: rest.to_string(),
                }))
            }
            "embed" => {
                let provider = self.inference.as_ref().ok_or_else(|| {
                    Error::Execution(
                        "an `embed:` step needs the daemon — none is wired".into(),
                    )
                })?;
                Ok(Arc::new(EmbedStep {
                    provider: Arc::clone(provider),
                    model: rest.to_string(),
                }))
            }
            "tool" => Ok(Arc::new(ToolStep {
                tools: Arc::clone(&self.tools),
                tool_id: rest.to_string(),
            })),
            "mcp" => {
                let (server, tool) = rest.split_once(':').ok_or_else(|| {
                    Error::Execution(format!(
                        "an `mcp:` step must be `mcp:<server>:<tool>` — got `{uses}`"
                    ))
                })?;
                Ok(Arc::new(ToolStep {
                    tools: Arc::clone(&self.tools),
                    tool_id: format!("mcp_{server}_{tool}"),
                }))
            }
            "transform" => Ok(Arc::new(TransformStep {
                name: rest.to_string(),
            })),
            other => Err(Error::Execution(format!(
                "unknown step kind `{other}` in `{uses}`"
            ))),
        }
    }
}
