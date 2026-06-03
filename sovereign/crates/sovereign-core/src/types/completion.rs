//! Split from the monolithic types.rs (ARCH §3.2); re-exported by types/mod.rs,
//! so every sovereign_core::types::* import path is unchanged (behaviour-preserving).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use crate::oicp;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub prompt: String,
    pub system_message: Option<String>,
    pub preferred_speed: Speed,
    pub max_tokens: Option<usize>,
    pub temperature: Option<f32>,
    pub structured_output: Option<serde_json::Value>,
    /// Overrides the default think-block token budget for this request.
    /// `None` falls back to the value in `InferenceConfig` (or the
    /// compiled-in `THINK_BUDGET` constant if unavailable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_budget: Option<usize>,
    /// Override the family-default top-k sampling parameter.
    /// `None` falls back to `ModelQuirks::default_top_k`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Override the family-default top-p (nucleus) sampling parameter.
    /// `None` falls back to `ModelQuirks::default_top_p`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// OICP capability requirements. Used by providers that support
    /// OICP to select the best model. Ignored by providers that don't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oicp: Option<oicp::InferenceRequirements>,
    /// Tool schemas the model may call. Present only when the caller is
    /// an agent driver (opencode, Claude Code via MCP) using
    /// OpenAI-compatible function-calling.
    ///
    /// Invariant: when `tools.is_some()`, `preferred_speed` must be
    /// `Slow` or `Medium` — Fast-slot models (Qwen3-1.7B in the current
    /// stack) do not have tools-aware chat templates. The slot router
    /// returns [`Error::InvalidInput`] for Fast + tools requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    /// Tool-choice hint (`"auto"` | `"none"` | `"required"` |
    /// `{"type":"function","function":{"name":"..."}}`). Raw JSON so
    /// forward-compatible shapes pass through untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Operator-declared model identifier. When set, the inference
    /// provider should route to the named slot if one matches; when
    /// `None`, the provider falls back to its default routing
    /// (Speed-based for `EmbeddedLlamaCpp`, OICP scoring for the
    /// mesh provider). The id is the model name as advertised on
    /// `/v1/models` — the gguf file stem.
    ///
    /// Threaded through from the OpenAI `model` field by
    /// `sovereign_mesh::inference_adapter::build_completion_request`,
    /// and from `ChatPrompt::phase_id` resolved against
    /// `EnrichConfig.chat_models` by the enrich-side inference
    /// client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Per-request override for the chat template's `enable_thinking`
    /// kwarg. Affects thinking-mode model families (Qwen3.x, …) where
    /// the Jinja template prepends a `<think>` block when this is
    /// `true` and skips it when `false`.
    ///
    /// Default (`None`) preserves the historical embedded-path
    /// behaviour: thinking is OFF, which on a heavily thinking-trained
    /// model means the planning text leaks as plain assistant prose
    /// (no `<think>` wrapper, but the verbosity remains). Setting
    /// this to `Some(true)` for a witness-register call lets the
    /// template wrap the planning trace formally so callers (or the
    /// `strip_think_blocks` post-process) can drop it cleanly and
    /// surface the post-`</think>` reply.
    ///
    /// Wire path: serialized into the OpenAI request body as
    /// `chat_template_kwargs: { enable_thinking: <bool> }` by
    /// `RemoteApiProvider::build_request`; parsed back out by the
    /// daemon's `inference_adapter::extract_enable_thinking` and
    /// applied at `embedded::apply_chat_template_oaicompat` time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,

    /// Explicit sampler-profile override. When set, takes precedence
    /// over the inference layer's mode picker (which infers from
    /// `enable_thinking` + tools presence). Lets non-codex callers
    /// (atlas pipelines, ATOS, eval harnesses) declare exactly which
    /// profile they want without spoofing the inference-layer signals.
    ///
    /// Wire path: serialized as `"sampling_mode": "instruct"` / `"code"`
    /// / `"think"` on the request body. Inference layer maps this
    /// onto `ModelQuirks.instruct_*` / `code_*` / `default_*` blocks
    /// in `build_sampler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_mode: Option<SamplingMode>,

    /// Prefill text to append after the rendered chat-template prompt,
    /// before the model's first generation token. Used to "continue"
    /// an assistant turn from a known-good prefix — e.g. when the
    /// read-attractor nudge fires, the frontdoor sets this to the
    /// canonical `<tool_call>{"name":"exec_command","arguments":{"cmd":"apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: "`
    /// opener and the model has to sample continuations of a literal
    /// known-good prefix rather than choosing between read-vs-write
    /// from scratch.
    ///
    /// Structural lever over prompt-only nudging. Family-agnostic:
    /// works for any backend whose chat-template path emits a
    /// generation-position marker (`<|turn>model\n`, `<|im_start|>assistant\n`,
    /// `<start_of_turn>model\n`, etc.) — we append after that marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_prefix: Option<String>,

    /// Structural constraint on the `cmd` field of `exec_command` tool
    /// calls (R2). When set, `inference_adapter::tool_envelope_schema_for`
    /// decorates the `cmd` parameter schema with a `pattern: "^<literal-prefix>"`
    /// and `JsonConstraint`'s string-body walker masks any byte that
    /// wouldn't extend the literal prefix until it is fully emitted.
    /// After the prefix point, normal string-body sampling resumes.
    ///
    /// Set by frontdoor's `apply_read_attractor_nudge_chat` when it
    /// fires — the model's only legal continuation is the canonical
    /// `apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: ` opener
    /// followed by free-form body. Doesn't compose with
    /// `assistant_prefix` — pick one mechanism per turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_prefix: Option<String>,
    /// URL allowlist for grammar-constrained URL emission. When
    /// `Some`, the inference sampler installs a logit-mask constraint
    /// that prevents the model from emitting any HTTP/HTTPS URL outside
    /// this list — byte-by-byte, via the trie-walking state machine in
    /// `sovereign_inference::url_constraint::UrlAllowlistConstraint`.
    ///
    /// Used by tool-call result-rendering paths (search_gym runner,
    /// future production `SearchTool` integration) to make URL
    /// fabrication structurally impossible: the model literally cannot
    /// sample a token that would extend the cursor past a valid trie
    /// edge. Prose tokens pass through; URL-shaped tokens that don't
    /// match the trie get clamped to `-INFINITY`.
    ///
    /// `None` (default) leaves URL emission unconstrained. Empty list
    /// is treated as "no URLs allowed" — useful for tool-result
    /// renderings that contained zero URLs and where any URL emission
    /// is automatically a fabrication.
    ///
    /// Wire path: extracted from the OpenAI request body's
    /// `url_allowlist` field in `routes_inference.rs`; consumed by
    /// `embedded::build_sampler` which constructs the constraint and
    /// attaches it to `ConstrainedSampler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_allowlist: Option<Vec<String>>,
    /// Evidence-id allowlist for sampler-side citation faithfulness
    /// (Tier 2 of tool-framework expansion). Same architecture as
    /// `url_allowlist` — a byte-trie of valid `ev-Tn-NNNN` handles
    /// gets attached to the sampler; tokens that would extend
    /// `[ev-T…` into a non-existent id get clamped to `-INFINITY`.
    /// Prose tokens pass through. Combined with Tier 1's payload
    /// memory, this makes cross-turn citation fabrication
    /// structurally impossible, not just discouraged by prompt.
    ///
    /// `None` leaves citation emission unconstrained. Empty list
    /// is treated as "no citations allowed" — useful for synthesis
    /// turns where no prior tool returned evidence and any
    /// `[ev-T…` emission would be a fabrication.
    ///
    /// Wire path: extracted from the OpenAI request body's
    /// `evidence_id_allowlist` field in `routes_inference.rs`
    /// (populated by `apply_evidence_id_allowlist_from_tool_results`
    /// in `frontdoor.rs`); consumed by `embedded::build_sampler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id_allowlist: Option<Vec<String>>,
    /// Lark grammar that constrains the model's per-turn output.
    /// Mutually exclusive with `structured_output`: when both are
    /// set the lark path wins (this is the newer mechanism).
    ///
    /// Set by `sovereign_mesh::inference_adapter::build_completion_request`
    /// when `tools.is_some()` and `tool_choice != Some("none")` AND
    /// the `SOVEREIGN_ALTERNATION_GRAMMAR` env var is truthy. The
    /// rendered grammar is the alternation shape
    /// `start: text_branch | tool_envelope`, which lets the model
    /// emit either a parseable tool-call envelope OR plain text;
    /// closes the `parse_failed_envelope` + `loop_trap` failure
    /// classes the agent-bench scanner surfaced on 2026-05-21.
    ///
    /// Built via `llguidance_constraint::build_tool_alternation_grammar`.
    /// Consumed by `embedded::build_sampler`, which constructs a
    /// `LlguidanceConstraint` and attaches it to `ConstrainedSampler`
    /// in place of the usual `JsonConstraint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lark_grammar: Option<String>,
}

/// Sampler-profile selector. Mirrored in the inference layer's
/// internal enum — when `CompletionRequest.sampling_mode` is set,
/// `build_sampler` reads this value and picks the matching
/// `ModelQuirks.<mode>_*` block, overriding the auto-picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    /// Tool-picking / non-thinking general work. Qwen instruct
    /// profile (T=0.7, top_p=0.80, presence=1.5 on Qwen3).
    Instruct,
    /// Composing code or structured output. Qwen thinking-coding
    /// profile (T=0.6, top_p=0.95, presence=0.0 on Qwen3).
    Code,
    /// General reasoning. Qwen thinking-general profile (T=1.0,
    /// top_p=0.95, presence=1.5 on Qwen3).
    Think,
}

/// JSON-Schema view of a function the model may call. Mirrors
/// `commonwealth_api::openai_types::ToolFunction` but lives in the
/// provider-neutral core so `InferenceProvider` implementations don't
/// depend on the Commonwealth API crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

impl CompletionRequest {
    pub fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
            system_message: None,
            preferred_speed: Speed::Medium,
            max_tokens: None,
            temperature: None,
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
        }
    }

    pub fn with_speed(mut self, speed: Speed) -> Self {
        self.preferred_speed = speed;
        self
    }

    pub fn with_system(mut self, system: &str) -> Self {
        self.system_message = Some(system.to_string());
        self
    }

    pub fn with_oicp(mut self, requirements: oicp::InferenceRequirements) -> Self {
        self.oicp = Some(requirements);
        self
    }

    /// Tag this request with an explicit model id. The provider
    /// should route to the matching slot when one is loaded; when no
    /// match exists, the provider's default routing applies.
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    pub fn yes_no(condition: &str, context: &str) -> Self {
        Self {
            prompt: format!(
                "Given the following context:\n{context}\n\n\
                 Answer this yes/no question with only \"yes\" or \"no\":\n{condition}"
            ),
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(5),
            temperature: Some(0.0),
            structured_output: None,
            think_budget: Some(0), // No thinking needed for yes/no
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub text: String,
    /// Total tokens (prompt + completion). Kept as the historical
    /// "single number" telemetry stat. For the OpenAI Usage split,
    /// callers use `prompt_tokens` and derive completion as
    /// `tokens_used - prompt_tokens`.
    pub tokens_used: usize,
    /// Number of tokens in the formatted prompt (chat template +
    /// user content). Defaults to 0 for providers that don't track
    /// the split — the adapter then falls back to `tokens_used` in
    /// the legacy single-number form.
    #[serde(default)]
    pub prompt_tokens: usize,
    pub model_id: String,
    pub latency_ms: u64,
    /// OICP metadata from the provider, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oicp_meta: Option<oicp::OicpResponseMeta>,
    /// Why generation stopped — `Length` when the model hit
    /// `max_tokens`, `Stop` on EOS, etc. `None` from providers that
    /// don't track the distinction (older tests, stub providers).
    /// Surfaced into `ResponseProvenance` so the desktop cutoff chip
    /// works on non-streaming handler paths the same way it works on
    /// streaming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Completion tokens generated (excludes prompt). Mirrors the
    /// OpenAI `usage.completion_tokens` split so the cutoff chip can
    /// say "hit the 2048-token limit (1947 generated)" without
    /// fudging from `tokens_used - prompt_tokens`. `None` when the
    /// provider doesn't track it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
}

impl CompletionResponse {
    pub fn as_bool(&self) -> bool {
        let lower = self.text.trim().to_lowercase();
        lower.starts_with("yes") || lower.starts_with("true")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub max_context_tokens: usize,
    pub supports_structured_output: bool,
    pub relative_speed: Speed,
    pub relative_reasoning: Depth,
}

// ─── Stream framing (typed finish_reason) ──────────────────────
//
// The OpenAI streaming surface ends each stream with a chunk
// carrying `finish_reason` ∈ {"stop","length","tool_calls",
// "content_filter"}. The legacy `Stream<Item = Result<String>>`
// shape on `InferenceProvider::complete_stream` could not carry
// that signal — every truncation looked identical to a clean stop
// at the wire, so the desktop saw a clipped reply with no way to
// tell whether the model actually finished or hit its budget.
//
// `complete_stream_with_finish` (new on `InferenceProvider`,
// optional override) yields `StreamFrame` instead, with a
// terminal `Finish { reason, usage }` frame. Providers that want
// accurate semantics override; the default impl wraps the legacy
// stream and synthesises `Stop`.

/// Why a stream stopped emitting tokens. Mirrors the OpenAI
/// `finish_reason` enum plus a `Cancelled` variant for our
/// CancellationToken / receiver-drop paths and a free-form
/// `Error` variant for provider-side faults.
///
/// Serialization uses the OpenAI-compatible lowercase string
/// (`"stop"` / `"length"` / `"tool_calls"` / `"content_filter"` /
/// `"cancelled"` / `"error"`) rather than the Rust enum-variant
/// shape. Frontend code, persisted message metadata, and SSE wire
/// frames all consume this string form; using the derive default
/// (`"Stop"`, `"Length"`) would silently break the desktop chip
/// renderer + any legacy message persisted under the original
/// `Option<String>` shape on `ResponseProvenance.finish_reason`.
/// `Error(msg)` collapses to `"error"` on the wire — the inner
/// message is internal-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// Model emitted EOS (or an equivalent end-of-generation token).
    Stop,
    /// Generation hit `max_tokens`.
    Length,
    /// Model produced a tool-call payload. Reserved — streaming +
    /// tools is not currently supported on this surface, but the
    /// variant exists so callers don't have to guess.
    ToolCalls,
    /// Content filter or safety layer blocked the output.
    ContentFilter,
    /// Cancellation token tripped or the receiver was dropped.
    Cancelled,
    /// Provider-side error mid-stream. The string is the
    /// human-readable cause; the SSE bridge maps this to a wire
    /// `finish_reason: "error"`.
    Error(String),
}

impl FinishReason {
    /// OpenAI-compatible string for the SSE `finish_reason` field.
    /// `Cancelled` and `Error` are not OpenAI-native; we surface
    /// `"cancelled"` and `"error"` so clients that need to
    /// distinguish them can, while OpenAI-strict clients can treat
    /// any non-`"stop"` value as truncation.
    pub const fn as_openai_str(&self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::ContentFilter => "content_filter",
            FinishReason::Cancelled => "cancelled",
            FinishReason::Error(_) => "error",
        }
    }

    /// Parse the OpenAI wire string back into the typed enum. Used
    /// when decoding peer-emitted SSE chunks and when deserializing
    /// legacy persisted message metadata (originally shipped as
    /// `Option<String>` on `ResponseProvenance.finish_reason`).
    /// Unknown strings round-trip to `None` so callers can decide
    /// whether to treat them as `Stop` (lenient) or surface the bug.
    pub fn from_openai_str(s: &str) -> Option<Self> {
        match s {
            "stop" => Some(FinishReason::Stop),
            "length" => Some(FinishReason::Length),
            "tool_calls" => Some(FinishReason::ToolCalls),
            "content_filter" => Some(FinishReason::ContentFilter),
            "cancelled" => Some(FinishReason::Cancelled),
            // Inner message is lost on the wire — recover the variant
            // with an empty payload. Callers that need the message
            // get it from the surrounding tracing event.
            "error" => Some(FinishReason::Error(String::new())),
            _ => None,
        }
    }
}

impl Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_openai_str())
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s: String = Deserialize::deserialize(d)?;
        FinishReason::from_openai_str(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown OpenAI finish_reason: {s:?} (expected one of \
                 stop/length/tool_calls/content_filter/cancelled/error)"
            ))
        })
    }
}

/// Token-usage counters carried on the terminal stream frame.
/// Mirrors the OpenAI `usage` object so the SSE bridge can emit a
/// matching final chunk without a second source of truth.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// One frame on a typed completion stream. Replaces the legacy
/// `Result<String, Error>` items: `Token` carries a partial text
/// delta, `Finish` is the terminal frame with the reason we
/// stopped, and `Error` lets a provider abort mid-stream without
/// ambiguity. Streams MUST end with either `Finish` or `Error`;
/// receivers treat a closed channel without a terminal frame as
/// `Cancelled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamFrame {
    Token(String),
    Finish {
        reason: FinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<StreamUsage>,
    },
    Error(String),
}
