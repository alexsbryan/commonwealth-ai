// SPDX-License-Identifier: AGPL-3.0-or-later
//! OpenAI Responses API wire types — codex-driven subset.
//!
//! This module declares ONLY the types we accept on the wire and the
//! types we emit on the wire for `/v1/responses`. Translation lives in
//! `routes_responses.rs`. Keep this file shape-only: no IO, no logic.
//!
//! Scope: codex (`@openai/codex` 0.130.x) drives a strict subset of
//! the public Responses spec — text + function tools, streaming SSE,
//! no stateful `previous_response_id`, no parallel-tool fan-out, no
//! reasoning items. We accept that subset and reject (with a 400)
//! features we don't support, rather than silently ignoring them.

use serde::{Deserialize, Serialize};

/// Incoming POST /v1/responses request body.
///
/// Codex sends `input` as either a plain `String` (single-turn user
/// message) or an array of `ResponsesInputItem`. The handler upgrades
/// the string form before translation.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    pub model: Option<String>,

    /// Either a bare string (shorthand for `[{role:"user", content:[{type:"input_text", text}]}]`)
    /// or an array of typed input items.
    pub input: ResponsesInput,

    /// System-message-equivalent. Prepended as `{role:"system"}` ahead
    /// of all `input` items when translating to chat.completions.
    #[serde(default)]
    pub instructions: Option<String>,

    /// Flat tool definitions: `{type:"function", name, description?, parameters, strict?}`.
    /// Translated to nested `{type:"function", function:{name,...}}` chat.completions shape.
    #[serde(default)]
    pub tools: Option<Vec<ResponsesTool>>,

    /// Passthrough — same wire shape as chat.completions `tool_choice`.
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,

    #[serde(default)]
    pub stream: Option<bool>,

    /// Forwarded as chat.completions `max_tokens`.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,

    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,

    /// Stateful conversation chaining. We don't implement server-side
    /// state — handler 400s when this is set so codex falls back to
    /// resending full history rather than silently dropping context.
    #[serde(default)]
    pub previous_response_id: Option<String>,

    /// Server-side response storage. We don't store; field accepted
    /// for forward-compat and ignored.
    #[serde(default)]
    pub store: Option<bool>,

    /// Codex sends this; accepted and ignored. Function-tool parallelism
    /// is a property of the model's emit pattern, not the wire request.
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,

    /// Reasoning-summary controls. Accepted and ignored — local models
    /// don't expose OpenAI's reasoning surface; `<think>` blocks are
    /// stripped before output_text deltas.
    #[serde(default)]
    pub reasoning: Option<serde_json::Value>,

    /// Per-request metadata. Echoed in `response.metadata` on completion.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Untagged: codex sends either a string or an array.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    Text(String),
    Items(Vec<ResponsesInputItem>),
}

/// One element of `input[]`.
///
/// Codex emits four shapes here, all dispatched by field presence
/// (`#[serde(untagged)]`) so an explicit `type` discriminator is
/// optional. The `type` field, when present, is consumed as an
/// extra/ignored field.
///
///   1. `{type:"message", role, content:[...]}` — typed message
///   2. `{role, content:[...]}` — untyped message
///   3. `{type:"function_call", call_id, name, arguments}` — replayed assistant call
///   4. `{type:"function_call_output", call_id, output}` — tool result
///
/// Order matters: `Message` first (most common), then the two
/// function-call shapes. `FunctionCall` is tried before
/// `FunctionCallOutput` because it has the strictest field set.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInputItem {
    Message(MessageItem),
    FunctionCall(FunctionCallItem),
    FunctionCallOutput(FunctionCallOutputItem),
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageItem {
    pub role: String,
    pub content: MessageContent,
}

/// `content` is either a bare string or an array of content parts.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ResponsesContentPart>),
}

/// One content-part of an `input_message`. We accept input_text and
/// output_text on the input side because codex replays prior assistant
/// turns using output_text — and ignores any other types (image_url,
/// input_audio, …) by extracting text-only content.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesContentPart {
    InputText {
        text: String,
    },
    OutputText {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallItem {
    pub call_id: String,
    pub name: String,
    /// Stringified JSON, matching chat.completions `tool_calls[].function.arguments`.
    pub arguments: String,
    /// Optional client-supplied id; not required.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallOutputItem {
    pub call_id: String,
    /// Tool result content, as a string. Codex stringifies before send.
    pub output: String,
}

/// Flat function tool — wire-shape differs from chat.completions
/// (which nests under `function:{...}`).
///
/// `name` is intentionally optional: codex's tool list also carries
/// built-in tools (`web_search`, `file_search`, `local_shell`,
/// `image_generation_call`, `computer_use_preview`, `mcp`) whose wire
/// shapes do NOT include a top-level `name`. The handler filters
/// these out — local models have no path to invoke them — so making
/// `name` mandatory at the wire layer would 422 every codex request
/// before we get a chance to drop the unsupported tools.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesTool {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
    /// Accepted, ignored — chat.completions doesn't model `strict`.
    #[serde(default)]
    pub strict: Option<bool>,
}

// ─── Non-streaming response shape ───────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesResponse {
    pub id: String,
    pub object: &'static str,
    pub created_at: u64,
    pub status: &'static str,
    pub model: String,
    pub output: Vec<ResponsesOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ResponsesUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// We don't implement reasoning items; echo `null` so codex sees
    /// the field present.
    pub reasoning: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesOutputItem {
    Message(OutputMessage),
    FunctionCall(OutputFunctionCall),
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputMessage {
    pub id: String,
    pub status: &'static str,
    pub role: &'static str,
    pub content: Vec<OutputContentPart>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentPart {
    OutputText {
        text: String,
        annotations: Vec<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputFunctionCall {
    pub id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub status: &'static str,
}

/// Responses uses `input_tokens` / `output_tokens` (chat.completions
/// uses `prompt_tokens` / `completion_tokens`). We translate.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ResponsesUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_string_input() {
        let req: ResponsesRequest =
            serde_json::from_str(r#"{"model":"x","input":"hello"}"#).unwrap();
        match req.input {
            ResponsesInput::Text(s) => assert_eq!(s, "hello"),
            _ => panic!("expected text input"),
        }
    }

    #[test]
    fn deserialize_typed_input_array() {
        let req: ResponsesRequest = serde_json::from_str(
            r#"{
                "model": "x",
                "input": [
                    {"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},
                    {"type":"function_call_output","call_id":"c1","output":"42"}
                ]
            }"#,
        )
        .unwrap();
        let items = match req.input {
            ResponsesInput::Items(v) => v,
            _ => panic!("expected items"),
        };
        assert_eq!(items.len(), 2);
        match &items[0] {
            ResponsesInputItem::Message(m) => {
                assert_eq!(m.role, "user");
                match &m.content {
                    MessageContent::Parts(parts) => {
                        assert!(matches!(parts[0], ResponsesContentPart::InputText { .. }));
                    }
                    _ => panic!("expected parts"),
                }
            }
            _ => panic!("expected message"),
        }
        match &items[1] {
            ResponsesInputItem::FunctionCallOutput(o) => {
                assert_eq!(o.call_id, "c1");
                assert_eq!(o.output, "42");
            }
            _ => panic!("expected function_call_output"),
        }
    }

    #[test]
    fn deserialize_untyped_message_input() {
        // Codex sometimes omits `type:"message"` — bare `{role, content}`.
        // The untagged enum picks `Message` by field presence.
        let req: ResponsesRequest = serde_json::from_str(
            r#"{
                "model": "x",
                "input": [{"role":"user","content":"plain"}]
            }"#,
        )
        .unwrap();
        let items = match req.input {
            ResponsesInput::Items(v) => v,
            _ => panic!("expected items"),
        };
        assert_eq!(items.len(), 1);
        match &items[0] {
            ResponsesInputItem::Message(m) => {
                assert_eq!(m.role, "user");
                match &m.content {
                    MessageContent::Text(s) => assert_eq!(s, "plain"),
                    _ => panic!("expected text content"),
                }
            }
            _ => panic!("expected message"),
        }
    }

    #[test]
    fn deserialize_function_call_replay() {
        // Codex replays prior assistant tool-call invocations.
        let req: ResponsesRequest = serde_json::from_str(
            r#"{
                "model": "x",
                "input": [
                    {"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":\"ls\"}"}
                ]
            }"#,
        )
        .unwrap();
        let items = match req.input {
            ResponsesInput::Items(v) => v,
            _ => panic!("expected items"),
        };
        match &items[0] {
            ResponsesInputItem::FunctionCall(c) => {
                assert_eq!(c.call_id, "c1");
                assert_eq!(c.name, "shell");
            }
            other => panic!("expected function_call, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_flat_tool() {
        let req: ResponsesRequest = serde_json::from_str(
            r#"{
                "model": "x",
                "input": "go",
                "tools": [{
                    "type": "function",
                    "name": "shell",
                    "description": "run a shell command",
                    "parameters": {"type":"object","properties":{"cmd":{"type":"string"}}}
                }]
            }"#,
        )
        .unwrap();
        let tools = req.tools.expect("tools present");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_deref(), Some("shell"));
        assert_eq!(tools[0].kind, "function");
    }

    #[test]
    fn serialize_response_shape() {
        let resp = ResponsesResponse {
            id: "resp_abc".into(),
            object: "response",
            created_at: 1_700_000_000,
            status: "completed",
            model: "m1".into(),
            output: vec![ResponsesOutputItem::Message(OutputMessage {
                id: "msg_1".into(),
                status: "completed",
                role: "assistant",
                content: vec![OutputContentPart::OutputText {
                    text: "hi".into(),
                    annotations: vec![],
                }],
            })],
            usage: Some(ResponsesUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            }),
            metadata: None,
            reasoning: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["object"], "response");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["output"][0]["type"], "message");
        assert_eq!(json["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(json["usage"]["input_tokens"], 10);
    }

    #[test]
    fn serialize_function_call_output_item() {
        let item = ResponsesOutputItem::FunctionCall(OutputFunctionCall {
            id: "fc_1".into(),
            call_id: "call_1".into(),
            name: "shell".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
            status: "completed",
        });
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["type"], "function_call");
        assert_eq!(v["call_id"], "call_1");
        assert_eq!(v["name"], "shell");
    }
}
