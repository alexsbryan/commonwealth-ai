// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

use crate::requirements::InferenceRequirements;

/// OpenAI-compatible chat completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// Tools the model may call. When present, the adapter routes to the
    /// Slow slot; Fast-slot models lack a tools-aware chat template and
    /// requests that land on Fast are rejected with `InvalidInput`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    /// "auto" | "none" | "required" | `{type:"function", function:{name:...}}`.
    /// Stored as raw JSON so forward-compat shapes pass through untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// OpenAI-style structured-output declaration. When set to
    /// `{"type":"json_schema","json_schema":{...}}` the daemon
    /// installs a grammar-constrained sampler so the model's output
    /// is forced to be valid JSON conforming to the schema. Used by
    /// the atlas Phase 1 extractor to defeat malformed-JSON drift on
    /// long structured outputs (Gemma-31B / Qwopus-27B).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    /// Commonwealth extension: OICP requirements for model selection.
    #[serde(default)]
    pub oicp: Option<InferenceRequirements>,
    /// Jinja chat-template kwargs forwarded to the model loader.
    /// Today only `enable_thinking: bool` is recognised — other keys
    /// are accepted but ignored. Both vLLM and llama-server accept
    /// this exact shape on the OpenAI-compatible surface, so callers
    /// targeting any of them (Sovereign daemon included) can flip
    /// thinking-mode on a per-request basis without an out-of-band
    /// extension. The daemon unwraps this in
    /// `inference_adapter::extract_enable_thinking`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<serde_json::Value>,
    /// Commonwealth extension: cap on `<think>` block tokens for
    /// thinking models (Qwen3 / Qwen3.5 / SmolLM3). `Some(0)` causes
    /// the daemon to inject `/no_think` into the system prompt,
    /// suppressing the chain-of-thought entirely; useful for
    /// structured-output tasks where the schema constraint already
    /// enforces correctness and thinking is pure overhead. `None`
    /// (default) preserves whatever each model family does by
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_budget: Option<u32>,
    /// Commonwealth extension: name of a tool-profile to apply,
    /// trimming `tools[]` to the profile's allowlist before the
    /// request reaches the model. Set by the inference adapter from
    /// the `X-Commonwealth-Tool-Profile` request header; `None`
    /// resolves to the registry's default profile (allow-all when no
    /// `~/.svrnmesh/tool_profiles.toml` is present).
    /// Commonwealth extension: tool-profile name. Set by the route
    /// handler from the `X-Sovereign-Tool-Profile` HTTP header so
    /// downstream code can look up the profile without re-reading
    /// headers. `None` (default) means "use the registry default".
    /// Profiles filter `tools[]` in place to cut prompt size for
    /// tools-heavy clients (opencode, Aider) that ship every tool
    /// every turn. See `sovereign_serving_host::tool_profile` for semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    /// Commonwealth extension: explicit sampler-profile selector
    /// (`"instruct"` / `"code"` / `"think"`). Lets callers override
    /// the inference layer's auto-picker (which infers mode from
    /// `tools` + `chat_template_kwargs.enable_thinking`). Maps onto
    /// `sovereign_core::types::SamplingMode` via serde rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_mode: Option<crate::completion::SamplingMode>,
    /// Commonwealth extension: prefill text appended after the
    /// rendered chat-template prompt, before the model's first
    /// generation token. Used by frontdoor nudges (read-attractor,
    /// failure-recovery) that need to *structurally* commit the model
    /// to a known-good response prefix rather than nudge it via
    /// instruction. Family-agnostic — every chat template ends with
    /// a generation-position marker (`<|turn>model\n`,
    /// `<|im_start|>assistant\n`, `<start_of_turn>model\n`, …) and
    /// the prefix lands after that marker.
    ///
    /// Threaded into `sovereign_core::types::CompletionRequest`
    /// via `inference_adapter::build_completion_request`; sent to
    /// llama-server by `chat_wire::write_assistant_prefix` as a trailing
    /// assistant message, which it prefills after the template renders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_prefix: Option<String>,
    /// Commonwealth extension: structural cmd-prefix constraint (R2).
    /// When set, the inference layer's tool-envelope schema injects a
    /// `pattern: "^<literal-prefix>"` on the `cmd` parameter of any
    /// `exec_command` tool so the grammar mask forces the literal
    /// prefix as the start of the cmd string. Frontdoor nudges set
    /// this to commit the model to a known-good action shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_prefix: Option<String>,
    /// Commonwealth extension: URL allowlist for grammar-constrained
    /// URL emission. When non-empty, the inference sampler installs a
    /// logit-mask constraint that prevents the model from emitting any
    /// HTTP/HTTPS URL outside this list — byte-by-byte, via the
    /// trie-walking state machine in
    /// `sovereign_inference::url_constraint::UrlAllowlistConstraint`.
    /// Used by tool-result rendering paths (search-gym runner,
    /// production SearchTool) to make URL fabrication structurally
    /// impossible: prose tokens pass through, URL-shaped tokens that
    /// don't match the trie get clamped to `-INFINITY`. Wire path:
    /// extracted here, threaded onto `CompletionRequest.url_allowlist`
    /// by `build_completion_request`, sent on by `chat_wire::write_constraints`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_allowlist: Option<Vec<String>>,
    /// Commonwealth extension (Tier 2 of tool-framework expansion):
    /// evidence-id allowlist for sampler-side citation faithfulness.
    /// Same architecture as `url_allowlist` applied to `ev-Tn-NNNN`
    /// handles. When non-empty, tokens that would extend `[ev-T…`
    /// into an id not in the list are clamped to `-INFINITY`. Wire
    /// path: extracted here, threaded onto
    /// `CompletionRequest.evidence_id_allowlist` by
    /// `build_completion_request`, sent on by
    /// `chat_wire::write_constraints`. Populated upstream by
    /// `apply_evidence_id_allowlist_from_tool_results` (frontdoor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id_allowlist: Option<Vec<String>>,
    /// Commonwealth extension: raw Lark grammar source forwarded to
    /// llguidance for grammar-constrained decoding. Strictly more
    /// expressive than `response_format` (regex tokens, recursion,
    /// custom productions, alternations like `"BREAK" | "CONTINUE"`)
    /// — the escape hatch for non-JSON-Schema constraints. When
    /// present beside `response_format`, the pair is refused, never
    /// layered (`chat_wire::write_constraints`: llama-server lets a
    /// grammar override `response_format`). Wire path:
    /// HTTP body field `lark_grammar` (raw string) → here →
    /// `inference_adapter::build_completion_request` →
    /// `CompletionRequest.lark_grammar` → `chat_wire::write_constraints`
    /// sends it to llama-server as an llguidance `grammar`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lark_grammar: Option<String>,
    /// Commonwealth extension: caller-directed stable-prefix length in
    /// BYTES of the flattened user prompt — sibling requests share this
    /// prefix byte-for-byte, so the engine may checkpoint/restore decode
    /// state at that boundary (`prefix_state.rs`, the recurrent-safe
    /// prefix-reuse path). Advisory; mismatches degrade to full prefill.
    /// Wire path: HTTP body field `stable_prefix_len` → here →
    /// `inference_adapter::build_completion_request` →
    /// `CompletionRequest.stable_prefix_len` → `generate_sync` directed
    /// pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_prefix_len: Option<usize>,
    /// Commonwealth extension: the top-k sampling override
    /// (`CompletionRequest.top_k`). Wire path: HTTP body field `top_k` →
    /// here → `inference_adapter::build_completion_request` → the sampler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Commonwealth extension: the id of the turn this host already
    /// admitted, when this call continues it (`CompletionRequest.admission`).
    /// Honoured only from a caller on this host: the listener that parses
    /// the wire clears it for anyone else
    /// (`sovereign_serving_host::turn_admission`), and only a daemon-backed
    /// `RemoteApiProvider` writes it, never a third-party engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_admission: Option<String>,
    /// DeepSeek's thinking switch (`{"type": "disabled"}`), as a client sends
    /// it to a hosted vendor. Read for a third-party engine only
    /// (`inference_adapter::vendor_fields`); an engine here ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// OpenAI `logprobs`: report the sampled tokens' log probabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,
    /// OpenAI `top_logprobs`: how many alternatives to report per token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// Accepts both `"text"` (string) and `[{"type":"text","text":"..."}]`
    /// (array) formats. Opencode sends the array form for tool-result
    /// messages; the deserializer extracts text parts and joins them.
    /// `null` or absent is an assistant turn that only calls tools (the
    /// OpenAI shape pi sends), and reads as no text.
    #[serde(default, deserialize_with = "deserialize_message_content")]
    pub content: String,
    /// Set on `role="tool"` messages to associate an execution result with
    /// the assistant `tool_calls[].id` that requested it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Present on `role="assistant"` replies when the model requested
    /// tool calls. Populated by the adapter after parsing the model's
    /// output; supplied by the caller when replaying prior turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// The assistant's thinking, apart from `content`, as llama-server
    /// returns it (its default `--reasoning-format`). Set on replies by the
    /// adapter; on history a client sends back, the conversation path hands
    /// it to the model's template, which may render it (Qwen's does, inside
    /// `<think>`, for turns after the last user message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// Custom deserializer for `ChatMessage.content`: accepts either a plain
/// string or an array of `{"type":"text","text":"..."}` parts (OpenAI
/// multimodal format). Opencode sends the array form for tool-result
/// messages.
///
/// Non-text parts (`image_url`, `input_audio`, etc.) are dropped: this
/// adapter only forwards text content downstream. We *do* match on
/// `type == "text"` rather than just extracting any field named `text`
/// so an `image_url` part with an `alt`-style `text` field can't be
/// mislabelled as message content.
///
/// Parts whose `type` field is missing entirely are also accepted as
/// text — opencode's tool-result wire format omits the discriminator
/// in some versions.
///
/// `null` is no text: an assistant message that only requests tools
/// carries `"content": null` in the OpenAI shape. Refusing it turned every
/// pi session into a 422 at its second tool round (2026-10-06); the
/// message's meaning lives in its `tool_calls`, which are kept.
fn deserialize_message_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrParts {
        String(String),
        Parts(Vec<ContentPart>),
    }
    #[derive(Deserialize)]
    struct ContentPart {
        #[serde(rename = "type", default)]
        kind: Option<String>,
        #[serde(default)]
        text: Option<String>,
    }
    match Option::<StringOrParts>::deserialize(deserializer)? {
        None => Ok(String::new()),
        Some(StringOrParts::String(s)) => Ok(s),
        Some(StringOrParts::Parts(parts)) => {
            let texts: Vec<String> = parts
                .into_iter()
                .filter(|p| matches!(p.kind.as_deref(), None | Some("text")))
                .filter_map(|p| p.text)
                .collect();
            Ok(texts.join("\n"))
        }
    }
}

/// OpenAI-compatible tool schema entry. Only `type="function"` is
/// supported; other shapes round-trip as unknown fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the function's parameters. Held as an opaque
    /// `serde_json::Value` so we don't have to track every JSON Schema
    /// keyword.
    pub parameters: serde_json::Value,
}

/// One tool call the assistant issued. `arguments` is the raw JSON
/// string the model produced — it is NOT parsed by the adapter, so
/// malformed tool-call bodies surface as a JSON parse error at the
/// caller rather than a silent truncation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// OpenAI-compatible chat completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
    /// The engine's token logprobs (OpenAI `choices[].logprobs`), verbatim,
    /// when asked for and the engine reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

impl ChatMessage {
    /// Convenience constructor for the common `{role, content}` shape —
    /// avoids sprinkling `tool_call_id: None, tool_calls: None` across
    /// every call site.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ─── Streaming framing (typed finish_reason) ───────────────────
//
// `LocalInferenceService::chat_completion_stream` yields these
// frames instead of the legacy `Result<String, String>` shape, so
// the SSE bridge in `routes_inference::serve_local_stream` can emit
// an OpenAI-shaped terminal chunk with a real `finish_reason`
// rather than always lying with `null`.
//
// These mirror `sovereign_core::types::{StreamFrame, FinishReason,
// StreamUsage}` exactly. Translation lives in
// `sovereign-serving-host::inference_adapter::SovereignInferenceAdapter`.
// We don't share the type because commonwealth has no `sovereign`
// dep — keeping a parallel definition here is preferable to a new
// cross-project re-export through `oicp-types`.

/// Why a stream stopped. Maps onto OpenAI `finish_reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Cancelled,
    Error(String),
}

impl FinishReason {
    /// OpenAI-compatible string for the SSE wire field. Matches
    /// `sovereign_core::types::FinishReason::as_openai_str` so the
    /// adapter translation is identity at the wire layer.
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

    /// Parse an OpenAI-compatible `finish_reason` string into a typed
    /// variant. Unknown strings collapse to `Stop` — matches the
    /// `inference_adapter::synthesize_tool_stream` fallback, where an
    /// unrecognised reason from a provider becomes a clean
    /// `finish_reason: "stop"` rather than a synthetic error variant.
    ///
    /// `"error"` round-trips with an empty cause string because the
    /// wire `finish_reason` field does not itself carry the cause.
    pub fn from_openai_str(s: &str) -> Self {
        match s {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            "cancelled" => FinishReason::Cancelled,
            "error" => FinishReason::Error(String::new()),
            _ => FinishReason::Stop,
        }
    }
}

/// Typed view over the OpenAI `role` field on a chat message.
/// The wire field stays a `String` on `ChatMessage` (legitimate §2.2
/// exception — OpenAI's `role` vocabulary is open by spec; clients
/// ship "developer", "function", etc.). Internal dispatch uses this
/// enum so the closed-set roles get exhaustive matching and the
/// open-set tail goes through `Other`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
    /// Any role outside the OpenAI core vocabulary (`developer`,
    /// `function`, vendor-specific roles). Carries the original
    /// string so the adapter can echo it back to the model.
    Other(String),
}

impl Role {
    pub const fn as_openai_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            // `Other` carries its own string; the &'static fallback
            // here is only reached when a caller wants a constant
            // label for the variant tag (rare).
            Role::Other(_) => "other",
        }
    }

    pub fn from_openai_str(s: &str) -> Self {
        match s {
            "system" => Role::System,
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            other => Role::Other(other.to_string()),
        }
    }
}

/// Token-usage counters carried on the terminal stream frame.
/// Distinct serialised type from [`Usage`] only because we want to
/// keep the streaming and non-streaming surfaces orthogonal —
/// fields are identical.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// One frame on a typed completion stream. Streams MUST end with
/// either [`StreamFrame::Finish`] or [`StreamFrame::Error`];
/// receivers treat a closed channel without a terminal frame as
/// `Cancelled`.
///
/// `ToolCalls` was added to support streaming responses that include
/// tool calls. Local backends extract tool calls from a fully-buffered
/// model response (the `<tool_call>` markup parser runs after
/// generation completes), so a tools-streaming run yields one
/// `ToolCalls(...)` frame containing every parsed call rather than
/// the per-character `arguments` deltas the OpenAI spec also permits.
/// Both shapes are wire-legal — clients accumulate `tool_calls[i]`
/// fragments by `index` regardless of how many chunks the server
/// chose to split them across.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamFrame {
    Token(String),
    /// A piece of the assistant's thinking; rendered as
    /// `delta.reasoning_content`, apart from the reply's `content`.
    Reasoning(String),
    ToolCalls(Vec<ToolCall>),
    Finish {
        reason: FinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<StreamUsage>,
    },
    Error(String),
    /// Out-of-band glassbox payload (INLINE_COMPLETION.md §6). The
    /// FIM adapter emits one immediately before the terminal frame;
    /// the `/v1/completions` route attaches it to the response when
    /// the request opted in with `debug: true` and drops it
    /// otherwise. Never produced on the chat path — `serve_local_stream`
    /// discards it defensively.
    Debug(serde_json::Value),
}

/// `/v1/completions` request — the FIM inline-completion surface
/// (`svrn/docs/INLINE_COMPLETION.md` §3.4, decision D6). Dual
/// shape: the OpenAI-legacy fields (`model`/`prompt`/`suffix`/
/// `max_tokens`/`stop`/`stream`) keep generic OpenAI-compat clients
/// and curl working; the rich fields (`prefix`/`path`/`language`/
/// `debug`) are what the first-party VSCode extension sends.
/// `prefix` wins over `prompt` when both are present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionsRequestWire {
    /// Legacy: model id. Echoed in the response envelope, but the
    /// configured FIM slot always serves regardless.
    #[serde(default)]
    pub model: Option<String>,
    /// Legacy: the prompt (treated as the FIM prefix).
    #[serde(default)]
    pub prompt: Option<String>,
    /// Legacy + rich: code after the cursor.
    #[serde(default)]
    pub suffix: Option<String>,
    /// Rich: code before the cursor. Wins over `prompt`.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Rich: file path (language fallback + debug echo).
    #[serde(default)]
    pub path: Option<String>,
    /// Rich: explicit language id.
    #[serde(default)]
    pub language: Option<String>,
    /// Generation cap override.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Temperature override.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Extra stop strings; OpenAI allows a bare string or an array.
    #[serde(default)]
    pub stop: Option<StopParam>,
    /// SSE streaming when true.
    #[serde(default)]
    pub stream: Option<bool>,
    /// Opt-in glassbox payload on the terminal chunk / response.
    #[serde(default)]
    pub debug: Option<bool>,
    /// The next-edit lane's whole prompt, decoded verbatim (the adapter's
    /// `FimCompletionRequest::raw_prompt`). Honoured by serve's route, which
    /// the svrn daemon dials for that lane's model call
    /// (pb-svrn-dials-serve); the daemon's own editor door builds its
    /// request field by field and never reads it.
    #[serde(default)]
    pub raw_prompt: Option<String>,
}

impl CompletionsRequestWire {
    /// Effective prefix: rich `prefix` wins over legacy `prompt`.
    pub fn effective_prefix(&self) -> Option<&str> {
        self.prefix.as_deref().or(self.prompt.as_deref())
    }
}

/// OpenAI's `stop` accepts a bare string or an array of strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StopParam {
    /// One stop string.
    Single(String),
    /// Up to four per the OpenAI spec (not enforced here).
    Multi(Vec<String>),
}

impl StopParam {
    /// Flatten to a vec for the tracker's union scan.
    pub fn into_vec(self) -> Vec<String> {
        match self {
            StopParam::Single(s) => vec![s],
            StopParam::Multi(v) => v,
        }
    }
}

/// OpenAI-compatible `/v1/embeddings` request. `input` may be a single
/// string or a list of strings; the handler fans out over the list
/// and returns one `EmbeddingData` per item with a stable `index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: EmbeddingInput,
    /// OpenAI reserves this for `float` / `base64` output encoding.
    /// We emit `float` unconditionally; the field is accepted but
    /// ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(String),
    Batch(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingData {
    pub object: String,
    pub embedding: Vec<f32>,
    pub index: usize,
}

/// `/v1/rerank` request, in the shape llama-server, Jina and Cohere share:
/// score each document against the query with the served cross-encoder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankRequest {
    /// Echoed back; the node serves its one rerank model whatever it says.
    #[serde(default)]
    pub model: String,
    /// The query every document is scored against.
    pub query: String,
    /// The documents, scored in this order.
    pub documents: Vec<String>,
}

/// `/v1/rerank` response: one result per document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankResponse {
    /// The request's `model`, echoed.
    pub model: String,
    /// One per input document, in input order.
    pub results: Vec<RerankResult>,
}

impl RerankResponse {
    /// The scores for a request of `documents` documents, in input order,
    /// whatever order the server listed them in. Refuses a result that names
    /// a document past the end and a document left without a score, so a
    /// short answer never shifts scores onto the wrong documents. The one
    /// decoder every `/v1/rerank` client reads.
    pub fn scores_in_input_order(self, documents: usize) -> Result<Vec<f32>, String> {
        let mut scores = vec![None; documents];
        for result in self.results {
            let slot = scores.get_mut(result.index).ok_or_else(|| {
                format!(
                    "rerank response names document {} of {documents}",
                    result.index
                )
            })?;
            *slot = Some(result.relevance_score);
        }
        scores
            .into_iter()
            .enumerate()
            .map(|(i, s)| s.ok_or_else(|| format!("rerank response has no score for document {i}")))
            .collect()
    }
}

/// One document's score. Scores are model-specific logits; never compare
/// them across models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankResult {
    /// The document's position in the request.
    pub index: usize,
    /// Higher is more relevant.
    pub relevance_score: f32,
}

/// OpenAI-compatible model list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelListResponse {
    pub object: String,
    pub data: Vec<ModelObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObject {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub owned_by: String,
    /// Commonwealth extension: OICP capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
    /// Commonwealth extension: performance estimates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<ModelPerformance>,
    /// Commonwealth extension: whether a holder has these weights in
    /// memory right now. See [`Residency`] — the distinction it draws is
    /// the one `performance.loaded` could not, because that flag answers
    /// only for the daemon being asked and is absent entirely when no
    /// shard plan exists (which was 4 of 6 real entries on a live mesh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<Residency>,
    /// Commonwealth extension: display names of the nodes advertising this
    /// id, `"local"` for this one. Empty only on the orchestrator path,
    /// which tracks models without attributing them to a node.
    ///
    /// `owned_by` cannot carry this: it is a single string, it is part of
    /// the OpenAI baseline shape, and it read `"mesh"` for every real
    /// entry — so a client could see that a model existed somewhere and
    /// never which somewhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advertised_by: Vec<String>,
}

/// Whether a listed model's weights are in memory, on a node that holds
/// them. Every id `/v1/models` returns is dispatchable by name — that is
/// the list's contract — so the open question is only what the first
/// request pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Residency {
    /// A holder reports the weights resident. First token is a decode away.
    Resident,
    /// A holder has the weights and loads them on demand. The first request
    /// pays a cold load — tens of seconds for a large primary, which is
    /// normal operation and not an availability fault.
    Cold,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPerformance {
    pub estimated_tokens_per_sec: f32,
    pub estimated_ttft_ms: u32,
    pub loaded: bool,
}

/// OpenAI-compatible error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: Option<String>,
}

impl ErrorResponse {
    pub fn new(message: impl Into<String>, error_type: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                message: message.into(),
                error_type: error_type.into(),
                code: None,
            },
        }
    }
}

#[cfg(test)]
#[path = "openai_types_tests.rs"]
mod tests;
