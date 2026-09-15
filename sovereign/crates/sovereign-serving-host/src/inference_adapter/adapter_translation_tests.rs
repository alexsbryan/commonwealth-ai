// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{strip_tool_call_blocks, SovereignInferenceAdapter};
use oicp_types::openai_types::{
    ChatCompletionRequest, ChatMessage, FunctionCall, ToolCall, ToolDefinition, ToolFunction,
};

fn tool_def(name: &str) -> ToolDefinition {
    ToolDefinition {
        kind: "function".into(),
        function: ToolFunction {
            name: name.into(),
            description: Some(format!("description of {name}")),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"x": {"type": "string"}},
                "required": ["x"],
            }),
        },
    }
}

fn user(content: &str) -> ChatMessage {
    ChatMessage::new("user", content)
}

#[test]
fn flatten_preserves_tool_message_id() {
    let req = ChatCompletionRequest {
        model: None,
        messages: vec![
            user("what's the weather?"),
            ChatMessage {
                role: "assistant".into(),
                content: String::new(),
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_123".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "get_weather".into(),
                        arguments: r#"{"city":"SF"}"#.into(),
                    },
                }]),
            },
            ChatMessage {
                role: "tool".into(),
                content: r#"{"temperature":62}"#.into(),
                tool_call_id: Some("call_123".into()),
                tool_calls: None,
            },
        ],
        temperature: None,
        max_tokens: None,
        stream: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools: Some(vec![tool_def("get_weather")]),
        tool_choice: Some(serde_json::json!("auto")),
        oicp: None,
        response_format: None,
        chat_template_kwargs: None,
        think_budget: None,
        tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        stable_prefix_len: None,
    };
    let (prompt, _system) = SovereignInferenceAdapter::flatten(&req);
    // The prior tool call is replayed as a <tool_call> block so
    // Qwen3.5's template sees the model's own previous turn in a
    // shape it recognizes.
    assert!(prompt.contains("<tool_call>"));
    assert!(prompt.contains("get_weather"));
    // The tool result carries its call id so the model can
    // correlate it with the originating call.
    assert!(prompt.contains("Tool[call_123]:"));
    assert!(prompt.contains(r#"{"temperature":62}"#));
}

#[test]
fn forward_tools_translates_schema() {
    let req = ChatCompletionRequest {
        model: None,
        messages: vec![user("hi")],
        temperature: None,
        max_tokens: None,
        stream: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools: Some(vec![tool_def("a"), tool_def("b")]),
        tool_choice: None,
        oicp: None,
        response_format: None,
        chat_template_kwargs: None,
        think_budget: None,
        tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        stable_prefix_len: None,
    };
    let forwarded = SovereignInferenceAdapter::forward_tools(&req).unwrap();
    assert_eq!(forwarded.len(), 2);
    assert_eq!(forwarded[0].name, "a");
    assert_eq!(forwarded[1].name, "b");
    assert_eq!(
        forwarded[0].description.as_deref(),
        Some("description of a")
    );
}

#[test]
fn forward_tools_empty_returns_none() {
    let req = ChatCompletionRequest {
        model: None,
        messages: vec![],
        temperature: None,
        max_tokens: None,
        stream: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools: Some(Vec::new()),
        tool_choice: None,
        oicp: None,
        response_format: None,
        chat_template_kwargs: None,
        think_budget: None,
        tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        stable_prefix_len: None,
    };
    assert!(SovereignInferenceAdapter::forward_tools(&req).is_none());
}

#[test]
fn strip_tool_call_blocks_removes_markup() {
    let text = "Let me check.\n<tool_call>{\"name\":\"f\",\"arguments\":{}}</tool_call>\nDone.";
    let stripped = strip_tool_call_blocks(text);
    assert!(!stripped.contains("<tool_call>"));
    assert!(!stripped.contains("</tool_call>"));
    // Surrounding prose is preserved.
    assert!(stripped.contains("Let me check."));
    assert!(stripped.contains("Done."));
}

#[test]
fn strip_tool_call_blocks_handles_multiple() {
    let text = "<tool_call>a</tool_call>mid<tool_call>b</tool_call>end";
    let stripped = strip_tool_call_blocks(text);
    assert_eq!(stripped, "midend");
}

#[test]
fn strip_tool_call_blocks_tolerates_unterminated() {
    // Defensive: a truncated model output shouldn't panic — we
    // preserve the tail so operators can see what happened.
    let text = "ok <tool_call>{\"name\":\"never_closed\"";
    let stripped = strip_tool_call_blocks(text);
    assert!(stripped.contains("never_closed"));
}

fn req_with(
    tools: Option<Vec<ToolDefinition>>,
    think_budget: Option<u32>,
) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: None,
        messages: vec![user("hi")],
        temperature: None,
        max_tokens: None,
        stream: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools,
        tool_choice: None,
        oicp: None,
        response_format: None,
        chat_template_kwargs: None,
        think_budget,
        tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        stable_prefix_len: None,
    }
}

#[test]
fn think_budget_defaults_to_zero_when_tools_present() {
    // Witnesses the FINAL-Bench-35B failure mode: 14.5K think
    // tokens, then a truncated `<tool_call>`. opencode/Aider/MCP
    // clients don't know about think_budget, so the daemon
    // defaults to Some(0) when tools are present so the model
    // emits the tool call without a chain-of-thought prelude.
    let req = req_with(Some(vec![tool_def("write")]), None);
    assert_eq!(super::resolve_think_budget(&req), Some(0));
}

#[test]
fn think_budget_respects_explicit_caller_value_with_tools() {
    // If a caller did want thinking with tools (e.g. a debugging
    // session), don't override their explicit choice.
    let req = req_with(Some(vec![tool_def("write")]), Some(2048));
    assert_eq!(super::resolve_think_budget(&req), Some(2048));
}

#[test]
fn think_budget_is_none_without_tools() {
    // No tools and no explicit budget → fall through to the
    // embedded provider's chat-template default.
    let req = req_with(None, None);
    assert_eq!(super::resolve_think_budget(&req), None);
}

#[test]
fn think_budget_is_none_when_tools_array_is_empty() {
    // Empty tools array is semantically "no tools" — same as None.
    let req = req_with(Some(vec![]), None);
    assert_eq!(super::resolve_think_budget(&req), None);
}

fn req_with_tool_choice(
    tools: Option<Vec<ToolDefinition>>,
    tool_choice: Option<serde_json::Value>,
) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: None,
        messages: vec![user("hi")],
        temperature: None,
        max_tokens: None,
        stream: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        tools,
        tool_choice,
        oicp: None,
        response_format: None,
        chat_template_kwargs: None,
        think_budget: None,
        tool_profile: None,
        sampling_mode: None,
        assistant_prefix: None,
        cmd_prefix: None,
        url_allowlist: None,
        evidence_id_allowlist: None,
        lark_grammar: None,
        stable_prefix_len: None,
    }
}

#[test]
fn tool_envelope_schema_engages_on_required_with_tools() {
    let req = req_with_tool_choice(
        Some(vec![tool_def("write")]),
        Some(serde_json::json!("required")),
    );
    let schema = super::tool_envelope_schema_for(&req).expect("schema should be built");
    // oneOf with one variant for the single tool.
    let variants = schema.get("oneOf").and_then(|v| v.as_array()).unwrap();
    assert_eq!(variants.len(), 1);
    // The variant binds `name` to the tool's literal name via
    // `enum: ["..."]` (the JsonConstraint compiler can't validate
    // bare `const` without `type`, so we use enum-of-one instead).
    let name_enum = variants[0]
        .pointer("/properties/name/enum")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(name_enum.len(), 1);
    assert_eq!(name_enum[0].as_str(), Some("write"));
    // additionalProperties is false so the model can't emit
    // unrecognised top-level keys alongside `name`+`arguments`.
    assert_eq!(
        variants[0].pointer("/additionalProperties"),
        Some(&serde_json::json!(false))
    );
}

#[test]
fn tool_envelope_schema_skipped_on_auto_choice() {
    // tool_choice="auto" must not engage — the model needs to be
    // free to emit text-only turns to end an opencode session.
    let req = req_with_tool_choice(
        Some(vec![tool_def("write")]),
        Some(serde_json::json!("auto")),
    );
    assert!(super::tool_envelope_schema_for(&req).is_none());
}

#[test]
fn tool_envelope_schema_skipped_when_no_tool_choice_set() {
    // Caller didn't set tool_choice. Default behaviour is "auto"
    // semantically, and we must not constrain.
    let req = req_with_tool_choice(Some(vec![tool_def("write")]), None);
    assert!(super::tool_envelope_schema_for(&req).is_none());
}

#[test]
fn tool_envelope_schema_skipped_when_tools_empty_under_required() {
    // `required` with zero tools is malformed; refuse to constrain
    // (empty oneOf would mask everything → model can never finish).
    let req = req_with_tool_choice(Some(vec![]), Some(serde_json::json!("required")));
    assert!(super::tool_envelope_schema_for(&req).is_none());
}

/// Per-test-module lock for tests that mutate
/// `SOVEREIGN_FORCE_TOOL_CALLS`. Three callers, all in this file.
/// The promise "tests run fast so the race won't matter" turned
/// out to be a flake under repo-wide parallel `cargo test` — pin
/// it with an actual mutex.
fn force_tool_calls_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[test]
fn force_tool_calls_env_engages_schema_when_choice_omitted() {
    let _guard = force_tool_calls_env_lock();
    std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
    let req = req_with_tool_choice(Some(vec![tool_def("write")]), None);
    let schema = super::tool_envelope_schema_for_with_env(&req);
    std::env::remove_var("SOVEREIGN_FORCE_TOOL_CALLS");
    assert!(
        schema.is_some(),
        "env override should synthesize tool_choice=required"
    );
}

#[test]
fn force_tool_calls_env_overrides_auto() {
    let _guard = force_tool_calls_env_lock();
    // Operator opted in via env: even an explicit "auto" gets
    // upgraded to "required" so the grammar engages. The opt-in
    // is global to the daemon; clients that need text-only turns
    // shouldn't run against a daemon with this var set.
    std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
    let req = req_with_tool_choice(
        Some(vec![tool_def("write")]),
        Some(serde_json::json!("auto")),
    );
    let schema = super::tool_envelope_schema_for_with_env(&req);
    std::env::remove_var("SOVEREIGN_FORCE_TOOL_CALLS");
    assert!(
        schema.is_some(),
        "env override should upgrade auto to required"
    );
}

#[test]
fn alternation_grammar_env_engages_schema_on_auto() {
    // Test that SOVEREIGN_ALTERNATION_GRAMMAR=1 also triggers
    // the envelope-schema synthesis when tool_choice is "auto"
    // (or omitted). Same env-var gating as
    // SOVEREIGN_FORCE_TOOL_CALLS but without the loop trap —
    // the grammar gives the model a text branch to escape.
    let _guard = force_tool_calls_env_lock();
    std::env::set_var("SOVEREIGN_ALTERNATION_GRAMMAR", "1");
    let req = req_with_tool_choice(
        Some(vec![tool_def("write")]),
        Some(serde_json::json!("auto")),
    );
    let schema = super::tool_envelope_schema_for_with_env(&req);
    std::env::remove_var("SOVEREIGN_ALTERNATION_GRAMMAR");
    assert!(
        schema.is_some(),
        "alternation env should synthesize tool_choice=required like force does"
    );
}

#[test]
fn force_tool_calls_env_respects_explicit_none() {
    let _guard = force_tool_calls_env_lock();
    // tool_choice="none" semantically means "model must NOT call a
    // tool". Even with the env var set, refuse to override that.
    std::env::set_var("SOVEREIGN_FORCE_TOOL_CALLS", "1");
    let req = req_with_tool_choice(
        Some(vec![tool_def("write")]),
        Some(serde_json::json!("none")),
    );
    let schema = super::tool_envelope_schema_for_with_env(&req);
    std::env::remove_var("SOVEREIGN_FORCE_TOOL_CALLS");
    assert!(schema.is_none(), "explicit none must NOT be overridden");
}

#[test]
fn parse_tool_envelope_direct_strips_trailing_close_marker() {
    // 2026-05-21 regression: fast-slot model emitted bare JSON
    // followed by a `</tool_call>` close (chat-template suffix).
    // serde_json rejects trailing data so direct-parse returned
    // empty unless the close is stripped first.
    let text = r#"{"name":"read","arguments":{"path":"src/lib.rs"}}</tool_call>"#;
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(calls.len(), 1, "trailing </tool_call> must be stripped");
    assert_eq!(calls[0].name, "read");
}

#[test]
fn parse_tool_envelope_direct_strips_think_tags_keeps_content() {
    // Old behaviour stripped the whole `<think>...</think>` block.
    // New behaviour strips only the tags so the JSON envelope
    // INSIDE the block (which happens under llguidance schema +
    // Qwen chat template) survives parsing.
    // The think prefix here contains arbitrary prose; the
    // envelope-shaped JSON is INSIDE the tags.
    let text = r#"<think>{"name":"read","arguments":{"path":"a"}}</think>"#;
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(calls.len(), 1, "JSON inside think must survive strip");
    assert_eq!(calls[0].name, "read");
}

#[test]
fn parse_tool_envelope_direct_extracts_clean_json() {
    // Grammar-constrained output is a single balanced JSON object
    // with no `<tool_call>` wrapper.
    let text = r#"{"name":"write","arguments":{"filePath":"a.rs","content":"fn main(){}"}}"#;
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "write");
    assert!(calls[0].arguments.contains("filePath"));
}

#[test]
fn parse_tool_envelope_direct_returns_empty_on_non_envelope() {
    // Random text or malformed JSON returns empty so the caller
    // can fall back to the marker-based parser.
    assert!(super::parse_tool_envelope_direct("hello world").is_empty());
    assert!(super::parse_tool_envelope_direct(r#"{"foo":"bar"}"#).is_empty());
    assert!(super::parse_tool_envelope_direct("").is_empty());
}

#[test]
fn parse_tool_envelope_direct_normalizes_raw_newlines() {
    // Same Qwen-Coder failure mode the marker-based parser
    // already handles: raw \n inside content string.
    let text = "{\"name\":\"write\",\"arguments\":{\"path\":\"x\",\"content\":\"line1\nline2\"}}";
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(calls.len(), 1, "normalization should recover");
    assert_eq!(calls[0].name, "write");
}

#[test]
fn parse_tool_envelope_direct_ignores_trailing_open_marker() {
    // 2026-05-21 N=3 sweep failure mode: 35B emits envelope + a
    // stray `<tool_call>` opener at the end (intended to chain a
    // second call but ran out of tokens). Strict serde_json
    // rejected trailing data → daemon emitted as text → pi lost
    // the tool call. Streaming parser must consume first complete
    // JSON value and ignore the trailing marker.
    let text = r#"{"name":"write","arguments":{"path":"src/lib.rs","content":"pub fn f() {}\n"}}
<tool_call>"#;
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(
        calls.len(),
        1,
        "should extract envelope despite trailing <tool_call>"
    );
    assert_eq!(calls[0].name, "write");
    assert!(calls[0].arguments.contains("src/lib.rs"));
}

#[test]
fn parse_tool_envelope_direct_ignores_trailing_garbage_text() {
    // Class B fix generalizes beyond `<tool_call>` — any trailing
    // garbage after a valid envelope is fine.
    let text = r#"{"name":"bash","arguments":{"command":"ls"}} blah blah"#;
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "bash");
}

#[test]
fn parse_tool_envelope_direct_ignores_second_envelope() {
    // Two complete envelopes in a row: take the first, leave the
    // second (pi protocol is one tool call per turn).
    let text =
        r#"{"name":"read","arguments":{"path":"a"}} {"name":"read","arguments":{"path":"b"}}"#;
    let calls = super::parse_tool_envelope_direct(text);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read");
    assert!(calls[0].arguments.contains("\"a\""));
}

#[test]
fn tool_envelope_schema_oneof_includes_each_tool() {
    let req = req_with_tool_choice(
        Some(vec![tool_def("a"), tool_def("b"), tool_def("c")]),
        Some(serde_json::json!("required")),
    );
    let schema = super::tool_envelope_schema_for(&req).unwrap();
    let variants = schema.get("oneOf").and_then(|v| v.as_array()).unwrap();
    assert_eq!(variants.len(), 3);
    let names: Vec<String> = variants
        .iter()
        .map(|v| {
            v.pointer("/properties/name/enum")
                .and_then(|x| x.as_array())
                .and_then(|arr| arr.first())
                .and_then(|x| x.as_str())
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}
