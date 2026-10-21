// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// Scores land in input order, and a short answer is refused rather than
/// shifting the scores it does carry onto the wrong documents.
#[test]
fn rerank_scores_land_in_input_order_and_a_short_answer_is_refused() {
    let response = |indices: &[usize]| RerankResponse {
        model: String::new(),
        results: indices
            .iter()
            .map(|&index| RerankResult {
                index,
                relevance_score: index as f32,
            })
            .collect(),
    };
    assert_eq!(
        response(&[2, 0, 1]).scores_in_input_order(3),
        Ok(vec![0.0, 1.0, 2.0])
    );
    assert_eq!(
        response(&[2, 0]).scores_in_input_order(3),
        Err("rerank response has no score for document 1".to_string())
    );
    assert_eq!(
        response(&[0, 3]).scores_in_input_order(3),
        Err("rerank response names document 3 of 3".to_string())
    );
}

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
            logprobs: None,
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
        reasoning_content: None,
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

/// The failing input: pi's third request, `messages[4]` verbatim
/// (agent-coding smoke 2026-10-06). An assistant turn that only calls
/// tools carries `"content": null` in the OpenAI shape; the daemon
/// refused the whole request with a 422 and pi ended the session.
#[test]
fn message_content_null_on_a_tool_call_turn_is_no_text() {
    let json = r#"{"role": "assistant", "content": null, "tool_calls": [{"id": "call_9852383_0", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"tests/test_integration.py\"}"}}]}"#;
    let msg: ChatMessage = serde_json::from_str(json).unwrap();
    assert_eq!(msg.content, "");
    let calls = msg.tool_calls.expect("the tool calls survive");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "read");
}

/// The same turn with `content` left out, which the OpenAI shape also
/// allows for an assistant message carrying tool calls.
#[test]
fn message_content_absent_on_a_tool_call_turn_is_no_text() {
    let json = r#"{"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{}"}}]}"#;
    let msg: ChatMessage = serde_json::from_str(json).unwrap();
    assert_eq!(msg.content, "");
    assert_eq!(msg.tool_calls.map(|c| c.len()), Some(1));
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
            residency: Some(Residency::Resident),
            advertised_by: vec!["local".into(), "RuggedFox".into()],
        }],
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("qwen3-coder-30b"));
    assert!(json.contains(r#""residency":"resident""#));
    assert!(json.contains("RuggedFox"));
}

/// The two availability fields are `skip_serializing_if`-absent, so an
/// old client parsing a new payload and a new client parsing an old one
/// must both work. The second direction is the one that bites: the
/// orchestrator path emits no `advertised_by`, and a `Vec` field without
/// `#[serde(default)]` would make that a hard parse failure in the
/// Ollama `/api/tags` shim, which round-trips this type.
#[test]
fn model_object_parses_without_the_availability_fields() {
    let json = r#"{"id":"m","object":"model","created":0,"owned_by":"mesh"}"#;
    let m: ModelObject = serde_json::from_str(json).unwrap();
    assert_eq!(m.residency, None);
    assert!(m.advertised_by.is_empty());
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
