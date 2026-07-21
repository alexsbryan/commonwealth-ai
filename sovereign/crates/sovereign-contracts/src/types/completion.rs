// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split from the monolithic types.rs (ARCH §3.2); re-exported by types/mod.rs,
//! so every sovereign_core::types::* import path is unchanged (behaviour-preserving).
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use crate::oicp;
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};

/// One inference call, provider-neutral: prompt + sampling knobs + the
/// structural constraints (grammars, allowlists, tool schemas) the sampler
/// enforces. Build via `new` / `for_workload` and the `with_*` builders.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// The user-role content; the provider's chat template wraps it (see `system_message`).
    pub prompt: String,
    /// System-role content; `None` = no system turn.
    pub system_message: Option<String>,
    /// Derived slot shadow — see `Speed` (written via `slot_policy::latency_to_speed`, never free-hand).
    pub preferred_speed: Speed,
    /// Generation cap; `None` = provider default. Prefer `with_output_budget`, which also sets the OICP gate.
    pub max_tokens: Option<usize>,
    /// Sampling temperature override; `None` = model-family default.
    pub temperature: Option<f32>,
    /// JSON schema for constrained decoding; when `lark_grammar` is also set, the grammar wins.
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

    /// How `prompt` reaches the tokenizer. `None`/`Templated` (the
    /// default) preserves the historical behaviour: the provider's
    /// chat template wraps `prompt` in a user turn. `Raw` tokenizes
    /// the string verbatim with special-token parsing on and no
    /// template wrapping — the fill-in-the-middle (FIM) inline-
    /// completion path uses this to feed the model's own
    /// `<|fim_prefix|>…<|fim_suffix|>…<|fim_middle|>` markers
    /// (built by `sovereign_inference::fim::build_fim_prompt`).
    /// See `sovereign/docs/INLINE_COMPLETION.md` §3.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_shape: Option<PromptShape>,
}

/// How the request's `prompt` reaches the tokenizer. Default is
/// `Templated` (chat-template wrapping); `Raw` feeds the string
/// verbatim — see `CompletionRequest::prompt_shape`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptShape {
    /// Wrap `prompt` in the model's chat template (historical default).
    Templated,
    /// Tokenize `prompt` verbatim, special-token parsing on, no chat
    /// template, no BOS injection. Used by the FIM inline-completion
    /// path (INLINE_COMPLETION.md §3.1, decision D4).
    Raw,
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
    /// Function name the model emits in a tool call.
    pub name: String,
    /// What the function does — input to the model's tool choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON schema of the function's arguments.
    pub parameters: serde_json::Value,
}

impl CompletionRequest {
    /// Minimal request: prompt only, primary-slot shadow (see inline comment), everything else default.
    pub fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
            system_message: None,
            // SLOT_POLICY §8: the default primary shadow. `Speed::Medium`
            // is retired as a construction target — it and `Slow` both
            // resolve to the primary slot locally, so this is behaviourally
            // identical, and it keeps the derived shadow out of the phantom
            // tier. A raw `new()` carries no OICP envelope, so it is never
            // offload-eligible (§5); this default only picks the local slot.
            preferred_speed: Speed::Slow,
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
            prompt_shape: None,
        }
    }

    /// Override the derived slot shadow. Prefer `for_workload` in new code (SLOT_POLICY §8).
    pub fn with_speed(mut self, speed: Speed) -> Self {
        self.preferred_speed = speed;
        self
    }

    /// Detect the forced-choice sentinel and return its candidate labels.
    /// A caller opts in by embedding `{"x_forced_choice": true, "enum":
    /// [...]}` in `structured_output`; the embedded inference path then
    /// answers with a calibrated one-pass distribution over the labels
    /// instead of running the generation loop (advertised as the
    /// `x:forced_choice` feature — [`oicp_types::features::X_FORCED_CHOICE`]).
    /// Returns `None` for every ordinary request, so all existing paths
    /// are unaffected.
    ///
    /// Note the sentinel JSON key is `x_forced_choice` (underscore),
    /// deliberately distinct from the advertised feature string
    /// `x:forced_choice` (colon). Hoisted here from the engine so the
    /// mesh scheduler and the local slot picker share one detector.
    pub fn forced_choice_candidates(&self) -> Option<Vec<String>> {
        let so = self.structured_output.as_ref()?;
        if so.get("x_forced_choice").and_then(|v| v.as_bool()) != Some(true) {
            return None;
        }
        let cands: Vec<String> = so
            .get("enum")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if cands.is_empty() {
            None
        } else {
            Some(cands)
        }
    }

    /// Attach a system message.
    pub fn with_system(mut self, system: &str) -> Self {
        self.system_message = Some(system.to_string());
        self
    }

    /// Attach an OICP requirements envelope — what makes the request scheduler-visible.
    pub fn with_oicp(mut self, requirements: oicp::InferenceRequirements) -> Self {
        self.oicp = Some(requirements);
        self
    }

    /// Workload-resolver constructor (SLOT_POLICY §9.4). The call site
    /// declares WHAT the call is ([`crate::slot_policy::Workload`]); the
    /// scheduler resolves WHERE it runs. Attaches the OICP requirement
    /// bundle, the derived `preferred_speed` shadow (§8 — via
    /// `latency_to_speed`, never a literal), the class think budget, and
    /// emits the glassbox `workload=` tracing event.
    ///
    /// Privacy: LocalOnly. Internal machinery uses this; it is provably
    /// routing-neutral at the mesh privacy gate. Session-posture-aware
    /// callers (grounding judges, EnrichBulk fan-out) use
    /// [`Self::for_workload_shared`].
    pub fn for_workload(workload: crate::slot_policy::Workload, prompt: impl Into<String>) -> Self {
        Self::for_workload_shared(workload, prompt, oicp::ShardingPrivacy::LocalOnly)
    }

    /// [`Self::for_workload`] with an explicit privacy posture — the
    /// only path by which internal work becomes mesh-offloadable.
    /// Threading the session/operator posture (never hardcoding it) is
    /// SLOT_POLICY §2.4.
    pub fn for_workload_shared(
        workload: crate::slot_policy::Workload,
        prompt: impl Into<String>,
        posture: oicp::ShardingPrivacy,
    ) -> Self {
        let bundle = workload.bundle();
        let oicp = workload.requirements(posture);
        tracing::debug!(
            target: "slot_policy",
            workload = workload.as_str(),
            latency_class = ?bundle.latency,
            privacy = ?posture,
            request_id = oicp.request_id.as_deref().unwrap_or(""),
            "workload request constructed"
        );
        let mut req = Self::new(&prompt.into());
        // The ONE canonical shadow derivation (SLOT_POLICY §8).
        req.preferred_speed = crate::slot_policy::latency_to_speed(bundle.latency);
        req.think_budget = bundle.think_budget;
        req.oicp = Some(oicp);
        req
    }

    /// Honest output budget (SLOT_POLICY §2.3): sets `max_tokens` AND
    /// the envelope's `max_output_tokens` together so the serving-side
    /// shadow and the routing contract can't drift. `max_output_tokens`
    /// is a hard routing gate (it selects FastShort vs FastLong);
    /// setting only one is the drift bug this closes. Promoted from the
    /// enrich-client pattern.
    pub fn with_output_budget(mut self, tokens: u32) -> Self {
        self.max_tokens = Some(tokens as usize);
        self.oicp = Some(self.oicp.unwrap_or_default().with_max_output_tokens(tokens));
        self
    }

    /// Honest context budget (SLOT_POLICY §2.3): sets the envelope's
    /// `context_tokens` hard gate.
    pub fn with_context_budget(mut self, tokens: u32) -> Self {
        self.oicp = Some(self.oicp.unwrap_or_default().with_context_tokens(tokens));
        self
    }

    /// Tag this request with an explicit model id. The provider
    /// should route to the matching slot when one is loaded; when no
    /// match exists, the provider's default routing applies.
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    /// Forced yes/no check on the fast slot: 5-token budget, temperature 0, no
    /// thinking. Read the verdict with `CompletionResponse::as_bool`. Used by
    /// `Branch` steps and other binary gates.
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
            // SLOT_POLICY §3 Route: a branch-condition check. One
            // envelope here makes every `yes_no` call site
            // scheduler-visible; the honest 5-token budget rides along
            // as the FastShort hard gate. Speed stays Fast (the shadow
            // Route would derive anyway).
            oicp: Some(
                crate::slot_policy::Workload::Route
                    .requirements(oicp::ShardingPrivacy::LocalOnly)
                    .with_max_output_tokens(5),
            ),
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
            prompt_shape: None,
        }
    }
}

/// A completed (non-streaming) inference result plus its telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// The generated text.
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
    /// Model that actually served the request — peer-attributed on mesh routes.
    pub model_id: String,
    /// End-to-end call latency, milliseconds.
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
    /// Lenient verdict-read for `yes_no` requests: true when the trimmed text starts with "yes" or "true".
    pub fn as_bool(&self) -> bool {
        let lower = self.text.trim().to_lowercase();
        lower.starts_with("yes") || lower.starts_with("true")
    }
}

/// Static self-description an `InferenceProvider` advertises — display and coarse-routing metadata, not a live measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Largest context window this provider can serve.
    pub max_context_tokens: usize,
    /// Whether `structured_output` schemas are actually enforced (vs ignored).
    pub supports_structured_output: bool,
    /// Coarse speed tier relative to the fleet.
    pub relative_speed: Speed,
    /// Coarse reasoning depth relative to the fleet.
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
    /// Tokens in the rendered prompt.
    pub prompt_tokens: u32,
    /// Tokens generated (excludes prompt).
    pub completion_tokens: u32,
    /// Prompt + completion.
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
    /// A partial text delta.
    Token(String),
    /// Terminal frame: why the stream stopped. Every stream must end with `Finish` or `Error`.
    Finish {
        /// Why generation stopped.
        reason: FinishReason,
        /// Token counters, when the provider tracks them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<StreamUsage>,
    },
    /// Provider-side abort mid-stream (the string is the cause). Also terminal.
    Error(String),
}

#[cfg(test)]
mod workload_builder_tests {
    use super::*;
    use crate::oicp::{LatencyClass, ShardingPrivacy, OICP_VERSION};
    use crate::slot_policy::Workload;

    #[test]
    fn for_workload_always_sets_oicp_version() {
        // Pins the structural-422 invariant: an envelope missing
        // `oicp_version` is rejected at the daemon's Json extractor.
        for w in Workload::ALL {
            let req = CompletionRequest::for_workload(w, "p");
            let oicp = req.oicp.expect("workload envelope");
            assert_eq!(oicp.oicp_version, OICP_VERSION, "{}", w.as_str());
        }
    }

    #[test]
    fn for_workload_defaults_to_local_only() {
        let req = CompletionRequest::for_workload(Workload::Route, "p");
        assert_eq!(req.oicp.unwrap().sharding(), ShardingPrivacy::LocalOnly);
    }

    #[test]
    fn for_workload_shared_threads_posture() {
        let req = CompletionRequest::for_workload_shared(
            Workload::Judge,
            "p",
            ShardingPrivacy::MeshAllowed,
        );
        assert_eq!(req.oicp.unwrap().sharding(), ShardingPrivacy::MeshAllowed);
    }

    #[test]
    fn for_workload_tags_request_id() {
        let req = CompletionRequest::for_workload(Workload::Housekeep, "p");
        let id = req.oicp.unwrap().request_id.expect("tag");
        assert!(id.starts_with("wl-housekeep-"), "{id}");
    }

    #[test]
    fn with_output_budget_sets_both_max_tokens_and_envelope() {
        let req = CompletionRequest::for_workload(Workload::Route, "p").with_output_budget(5);
        assert_eq!(req.max_tokens, Some(5));
        assert_eq!(req.oicp.unwrap().max_output_tokens, Some(5));
    }

    #[test]
    fn yes_no_carries_route_envelope_and_stays_fast() {
        let yn = CompletionRequest::yes_no("is it?", "ctx");
        assert_eq!(yn.preferred_speed, Speed::Fast);
        let oicp = yn.oicp.expect("route envelope");
        assert_eq!(oicp.effective_latency_class(), LatencyClass::Fast);
        assert_eq!(oicp.max_output_tokens, Some(5));
        assert_eq!(oicp.sharding(), ShardingPrivacy::LocalOnly);
    }
}
