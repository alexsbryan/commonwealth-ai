// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

use commonwealth_inference::oicp::InferenceRequirements;

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
    /// `~/.sovereign/tool_profiles.toml` is present).
    /// Commonwealth extension: tool-profile name. Set by the route
    /// handler from the `X-Sovereign-Tool-Profile` HTTP header so
    /// downstream code can look up the profile without re-reading
    /// headers. `None` (default) means "use the registry default".
    /// Profiles filter `tools[]` in place to cut prompt size for
    /// tools-heavy clients (opencode, Aider) that ship every tool
    /// every turn. See `sovereign_mesh::tool_profile` for semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    /// Commonwealth extension: explicit sampler-profile selector
    /// (`"instruct"` / `"code"` / `"think"`). Lets callers override
    /// the inference layer's auto-picker (which infers mode from
    /// `tools` + `chat_template_kwargs.enable_thinking`). Maps onto
    /// `sovereign_core::types::SamplingMode` via serde rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_mode: Option<sovereign_core::types::SamplingMode>,
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
    /// via `inference_adapter::build_completion_request`; consumed
    /// in `embedded::build_chat_prompt` after the chat template
    /// renders.
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
    /// by `build_completion_request`, consumed by `embedded::build_sampler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_allowlist: Option<Vec<String>>,
    /// Commonwealth extension (Tier 2 of tool-framework expansion):
    /// evidence-id allowlist for sampler-side citation faithfulness.
    /// Same architecture as `url_allowlist` applied to `ev-Tn-NNNN`
    /// handles. When non-empty, tokens that would extend `[ev-T…`
    /// into an id not in the list are clamped to `-INFINITY`. Wire
    /// path: extracted here, threaded onto
    /// `CompletionRequest.evidence_id_allowlist` by
    /// `build_completion_request`, consumed by
    /// `embedded::build_sampler`. Populated upstream by
    /// `apply_evidence_id_allowlist_from_tool_results` (frontdoor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_id_allowlist: Option<Vec<String>>,
    /// Commonwealth extension: raw Lark grammar source forwarded to
    /// llguidance for grammar-constrained decoding. Strictly more
    /// expressive than `response_format` (regex tokens, recursion,
    /// custom productions, alternations like `"BREAK" | "CONTINUE"`)
    /// — the escape hatch for non-JSON-Schema constraints. When
    /// present, takes precedence over `response_format` (both engines
    /// mask the same logit chain; layering would deadlock — see
    /// `embedded::build_sampler` comment at line ~7964). Wire path:
    /// HTTP body field `lark_grammar` (raw string) → here →
    /// `inference_adapter::build_completion_request` →
    /// `CompletionRequest.lark_grammar` → `embedded::build_sampler`
    /// instantiates `LlguidanceConstraint::new(lark, model)`.
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// Accepts both `"text"` (string) and `[{"type":"text","text":"..."}]`
    /// (array) formats. Opencode sends the array form for tool-result
    /// messages; the deserializer extracts text parts and joins them.
    #[serde(deserialize_with = "deserialize_message_content")]
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
    match StringOrParts::deserialize(deserializer)? {
        StringOrParts::String(s) => Ok(s),
        StringOrParts::Parts(parts) => {
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
// `sovereign-mesh::inference_adapter::SovereignInferenceAdapter`.
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
/// (`sovereign/docs/INLINE_COMPLETION.md` §3.4, decision D6). Dual
/// shape: the OpenAI-legacy fields (`model`/`prompt`/`suffix`/
/// `max_tokens`/`stop`/`stream`) keep generic OpenAI-compat clients
/// and curl working; the rich fields (`prefix`/`path`/`language`/
/// `debug`) are what the first-party VSCode extension sends.
/// `prefix` wins over `prompt` when both are present.
#[derive(Debug, Clone, Default, Deserialize)]
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
mod tests {
    use super::*;

    #[test]
    fn chat_completion_request_deserialize_minimal() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Hello"}]
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert!(req.model.is_none());
        assert!(req.oicp.is_none());
    }

    #[test]
    fn chat_completion_request_with_oicp() {
        let json = r#"{
            "messages": [{"role": "user", "content": "Write code"}],
            "oicp": {
                "oicp_version": "0.2.0",
                "capabilities": {
                    "required": {"code": 2},
                    "preferred": {"code": 4}
                }
            }
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert!(req.oicp.is_some());
        let oicp = req.oicp.unwrap();
        assert_eq!(oicp.oicp_version, "0.2.0");
    }

    #[test]
    fn chat_completion_response_serialize() {
        let resp = ChatCompletionResponse {
            id: "chatcmpl-123".into(),
            object: "chat.completion".into(),
            created: 1700000000,
            model: "qwen3-coder-30b".into(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage::new("assistant", "Hello!"),
                finish_reason: Some("stop".into()),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("chatcmpl-123"));
        assert!(json.contains("Hello!"));
    }

    #[test]
    fn error_response_serialize() {
        let err = ErrorResponse::new("model not found", "invalid_request_error");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("model not found"));
    }

    #[test]
    fn chat_completion_request_with_tools_round_trip() {
        let json = r#"{
            "messages": [
                {"role": "user", "content": "What's the weather in SF?"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Return the current weather",
                    "parameters": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }
            }],
            "tool_choice": "auto"
        }"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        let tools = req.tools.as_ref().expect("tools present");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].function.name, "get_weather");
        assert_eq!(
            req.tool_choice.as_ref().and_then(|v| v.as_str()),
            Some("auto")
        );
    }

    #[test]
    fn tool_message_preserves_tool_call_id() {
        let json = r#"{
            "role": "tool",
            "content": "{\"temperature\": 62}",
            "tool_call_id": "call_abc123"
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_abc123"));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn assistant_message_with_tool_calls_round_trips() {
        let msg = ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "get_weather".into(),
                    arguments: r#"{"city":"SF"}"#.into(),
                },
            }]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        let calls = back.tool_calls.expect("tool_calls survive");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn legacy_message_without_tool_fields_deserializes() {
        // Existing clients that don't know about tool_call_id / tool_calls
        // must continue to work. This fixture is exactly what M1 emitted.
        let json = r#"{"role":"user","content":"hi"}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, "user");
        assert!(msg.tool_call_id.is_none());
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn message_content_accepts_array_of_text_parts() {
        // Opencode sends tool-result messages in this form.
        let json = r#"{
            "role": "tool",
            "content": [
                {"type": "text", "text": "first chunk"},
                {"type": "text", "text": "second chunk"}
            ],
            "tool_call_id": "call_42"
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "first chunk\nsecond chunk");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_42"));
    }

    #[test]
    fn message_content_drops_non_text_parts() {
        // image_url / input_audio / similar parts are silently dropped:
        // this adapter doesn't forward multimodal content downstream.
        let json = r#"{
            "role": "user",
            "content": [
                {"type": "text", "text": "look at this"},
                {"type": "image_url", "image_url": {"url": "data:..."}},
                {"type": "text", "text": "what is it?"}
            ]
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "look at this\nwhat is it?");
    }

    #[test]
    fn message_content_does_not_extract_text_from_non_text_part() {
        // Defensive: a part with type=image_url + a `text` field
        // (e.g. alt text) must NOT be promoted to message content.
        let json = r#"{
            "role": "user",
            "content": [
                {"type": "image_url", "text": "ALT-TEXT-LEAK", "image_url": {"url": "x"}}
            ]
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "");
    }

    #[test]
    fn message_content_accepts_part_with_missing_type() {
        // Some opencode versions omit the discriminator. A bare
        // `{"text": "..."}` part is treated as text.
        let json = r#"{
            "role": "user",
            "content": [{"text": "no type field here"}]
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "no type field here");
    }

    #[test]
    fn message_content_string_form_unchanged() {
        // Regression: the existing string-content path must not change.
        let json = r#"{"role":"user","content":"plain text"}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "plain text");
    }

    #[test]
    fn message_content_empty_array_yields_empty_string() {
        let json = r#"{"role":"user","content":[]}"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "");
    }

    #[test]
    fn model_list_response_serialize() {
        let resp = ModelListResponse {
            object: "list".into(),
            data: vec![ModelObject {
                id: "qwen3-coder-30b".into(),
                object: "model".into(),
                created: 1700000000,
                owned_by: "mesh".into(),
                capabilities: None,
                performance: Some(ModelPerformance {
                    estimated_tokens_per_sec: 45.0,
                    estimated_ttft_ms: 1100,
                    loaded: true,
                }),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("qwen3-coder-30b"));
    }

    #[test]
    fn role_round_trips_core_vocabulary() {
        for raw in ["system", "user", "assistant", "tool"] {
            let parsed = Role::from_openai_str(raw);
            assert_eq!(parsed.as_openai_str(), raw, "round-trip for {raw}");
        }
    }

    #[test]
    fn role_other_preserves_unknown_vocabulary() {
        let parsed = Role::from_openai_str("developer");
        assert!(matches!(parsed, Role::Other(ref s) if s == "developer"));
    }

    #[test]
    fn finish_reason_round_trips_wire_vocabulary() {
        for raw in [
            "stop",
            "length",
            "tool_calls",
            "content_filter",
            "cancelled",
        ] {
            let parsed = FinishReason::from_openai_str(raw);
            assert_eq!(parsed.as_openai_str(), raw, "round-trip for {raw}");
        }
    }

    #[test]
    fn finish_reason_unknown_collapses_to_stop() {
        // Matches the adapter's behaviour: unrecognised reasons are
        // treated as a clean stop rather than synthesising an error.
        assert_eq!(
            FinishReason::from_openai_str("future_variant"),
            FinishReason::Stop
        );
    }

    #[test]
    fn finish_reason_error_round_trips_lossy() {
        // `Error(_)` carries a cause string that the wire field does
        // not. `from_openai_str("error")` gives back an empty-cause
        // Error; round-tripping through `as_openai_str` still yields
        // `"error"`. Pin this so the lossy step is documented in code.
        let parsed = FinishReason::from_openai_str("error");
        assert_eq!(parsed, FinishReason::Error(String::new()));
        assert_eq!(parsed.as_openai_str(), "error");
    }
}
