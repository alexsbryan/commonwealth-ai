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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
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
        assert_eq!(req.tool_choice.as_ref().and_then(|v| v.as_str()), Some("auto"));
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
}
