// SPDX-License-Identifier: AGPL-3.0-or-later
//! The prompt envelope and the one completion closure port: what ingest's
//! enrichment pipelines send to a model, and what svrn's own diagnostic
//! verbs send through the same closure shape. Moved from corpus-engine
//! (`enrichment::pipeline::types::ChatPrompt`, `types::InferenceFn`), which
//! re-exports both at their historical paths (pb-cli-llm-ingest-move-remainder).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Chat message prompt ready to submit to an OpenAI-compatible endpoint.
///
/// When `response_schema` is set, callers signal that the daemon
/// should run grammar-constrained generation against this JSON
/// Schema — an OpenAI-style `response_format: { type: "json_schema",
/// ... }` request. Used by Phase 1 to force the model into valid
/// JSON shape, eliminating the "missing comma / unclosed bracket"
/// failure mode observed on Gemma-31B for long structured outputs.
// `Eq` was dropped from the derive list when `temperature: Option<f32>`
// was added — `f32` doesn't implement `Eq` (NaN is not reflexive).
// `PartialEq` is sufficient for every test that compares ChatPrompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatPrompt {
    pub system: String,
    pub user: String,
    /// JSON Schema for grammar-constrained generation. When `None`,
    /// the request runs without constraints (backwards-compatible).
    /// When `Some`, the chat client serialises this into the
    /// OpenAI-style `response_format.json_schema.schema` field; the
    /// server's adapter maps it to `CompletionRequest.structured_output`
    /// which `build_sampler` consumes via `LlamaSampler::llguidance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
    /// Schema name for the OpenAI `response_format.json_schema.name`
    /// field. Only meaningful when `response_schema` is `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema_name: Option<String>,
    /// Stable identifier of the pipeline phase that produced this
    /// prompt — `"phase1_seed"`, `"phase1_extract"`, `"phase3_name"`,
    /// `"phase5_tensions"`, `"phase7_configure"`, etc. Carried so the
    /// chat client can route the request to a phase-specific model
    /// when the operator has declared one (e.g. small/fast for bulk
    /// extraction, large/reasoning for synthesis). When the client has
    /// no per-phase override for this id (or this field is `None`),
    /// the client falls back to its default `chat_model`.
    ///
    /// The recipe-side compose functions are the source of truth for
    /// which phase id a prompt carries. The chat client side never
    /// invents one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// Output-token budget for this prompt. When set, the chat client
    /// forwards it as `InferenceRequirements.max_output_tokens` so the
    /// OICP scheduler can hard-gate against each candidate claim's
    /// `max_output` (per OICP-v0.3 §2.4). Used to route short-call
    /// phases (phase1b coverage, phase3 cluster naming, phase5
    /// positions, phase6 tensions) to a high-throughput batched
    /// FastShort claim and keep long-output Phase 1 on FastLong.
    /// `None` leaves the budget unconstrained — the client falls back
    /// to its model-default cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature (0.0–2.0). When set, the chat client
    /// forwards it as the request `temperature` field instead of the
    /// dispatcher / provider default. Phase composers attach this when
    /// the atlas operator has a per-phase override (e.g. `0.0` for
    /// classifier phases, `0.3` for interpretive Phase 8). `None`
    /// falls through to the provider's `default_temperature` and
    /// finally the dispatcher's hardcoded fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Reasoning / extended-thinking budget in tokens. Anthropic
    /// thinking models, OpenAI o1-class, and DeepSeek-reasoner consume
    /// this differently, but all interpret it as "spend up to N tokens
    /// in a hidden reasoning block before emitting the visible
    /// response". `Some(0)` disables thinking explicitly; `None`
    /// inherits the provider default. Per-phase overrides matter for
    /// Phase 1 (which benefits from reasoning when the article is
    /// dense or dialectical) versus the judge / classifier phases
    /// (which don't).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<u32>,
}

impl ChatPrompt {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            user: user.into(),
            response_schema: None,
            response_schema_name: None,
            phase_id: None,
            max_output_tokens: None,
            temperature: None,
            thinking_tokens: None,
        }
    }

    /// Attach a JSON Schema for grammar-constrained generation.
    pub fn with_response_schema(
        mut self,
        name: impl Into<String>,
        schema: serde_json::Value,
    ) -> Self {
        self.response_schema_name = Some(name.into());
        self.response_schema = Some(schema);
        self
    }

    /// Tag this prompt with the pipeline phase that produced it. The
    /// chat client uses this to look up a phase-specific model in the
    /// operator's `chat_models` map and route the request there. See
    /// [`ChatPrompt::phase_id`] for the schema of the id strings.
    pub fn with_phase_id(mut self, phase_id: impl Into<String>) -> Self {
        self.phase_id = Some(phase_id.into());
        self
    }

    /// Cap the output-token budget for this prompt. The chat client
    /// forwards the value as `InferenceRequirements.max_output_tokens`
    /// so OICP scheduling can hard-gate (per v0.3 §2.4) against each
    /// candidate claim's `max_output`. Short-call phases set a small
    /// value (e.g. 512) to opt into the high-throughput FastShort
    /// claim; long-output phases either omit it or set it large.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }

    /// Override the sampling temperature for this prompt. See
    /// [`ChatPrompt::temperature`] for semantics.
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Override the thinking-token budget for this prompt. See
    /// [`ChatPrompt::thinking_tokens`] for semantics.
    pub fn with_thinking_tokens(mut self, n: u32) -> Self {
        self.thinking_tokens = Some(n);
        self
    }
}

// ─── Inference Function ─────────────────────────────────

/// The ONE completion closure port, injected by the caller — the
/// enrichment pipeline runs claim/relationship extraction, section
/// naming, reconciliation and synthesis prompts through it. Sovereign
/// passes its Primary slot; Commonwealth passes the mesh chat
/// endpoint; tests pass a deterministic closure returning canned JSON.
///
/// Converged 2026-09-17 (ARCH 8) from three aliases that named this one
/// capability: the single-message `InferenceFn`, the multi-message
/// `ChatCompletionFn`, and `ChatCompletionWithTokensFn`, its
/// per-call `max_tokens` arm. The prompt is a [`ChatPrompt`], which
/// carries the system and user messages, the optional JSON Schema for
/// grammar-constrained generation (read from
/// `Domain::entity_extraction_schema()` and threaded through), the
/// per-phase sampling controls and the prompt's own output budget.
/// The second argument is the per-call output-token override the retry
/// paths need: `Some(n)` wins over the prompt's budget, `None` defers
/// to it. Single-message callers pass `ChatPrompt::new("", prompt)`.
pub type InferenceFn = Arc<
    dyn Fn(&ChatPrompt, Option<u32>) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;
