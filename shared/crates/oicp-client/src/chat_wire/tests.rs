// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat wire's request-side tests: bodies through the client's real
//! builder ([`crate::RemoteApiProvider::build_request`]).

use super::*;
use serde_json::json;

fn schema() -> Value {
    json!({"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]})
}

/// THE FAILING INPUT for the third spelling: the body a bare llama-server
/// received before 2026-09-08 carried `think_budget` and `thinking`, neither
/// of which it reads, and no `chat_template_kwargs`, so a Qwen3 template
/// thought through the whole output budget under a JSON grammar and returned
/// an empty `content`.
#[test]
fn a_zero_budget_is_spelled_for_llama_server_too() {
    let mut body = json!({});
    write_thinking(&mut body, Some(0), Some(false));
    assert_eq!(body["think_budget"], json!(0));
    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert_eq!(
        body["chat_template_kwargs"],
        json!({"enable_thinking": false}),
        "the one spelling llama-server / vLLM / SGLang read"
    );
}

/// A positive budget opts in where a host has a budget notion. With no
/// `enable_thinking` (a conversation's, [`template_thinking`]) it says
/// nothing to the chat template: the template's default stands.
#[test]
fn a_positive_budget_leaves_the_template_default_alone() {
    let mut body = json!({});
    write_thinking(&mut body, Some(2048), None);
    assert_eq!(body["think_budget"], json!(2048));
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 2048})
    );
    assert!(
        body.get("chat_template_kwargs").is_none(),
        "no forced enable_thinking on a model whose template ships it off"
    );
}

#[test]
fn json_schema_is_strict_and_named_in_the_openai_alphabet() {
    let mut body = json!({});
    write_structured_output(
        &mut body,
        &titled_schema(&schema(), Some("phase 1 (atlas)")),
        StructuredOutputMode::JsonSchema,
    );
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["title"], "phase 1 (atlas)",
        "the label travels as the schema's own annotation"
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        "phase_1__atlas_"
    );
    assert!(body.get("tools").is_none());
}

#[test]
fn json_object_drops_the_schema_from_the_wire() {
    let mut body = json!({});
    write_structured_output(&mut body, &schema(), StructuredOutputMode::JsonObject);
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
}

#[test]
fn a_forced_tool_carries_the_schema_and_forces_the_call() {
    let mut body = json!({});
    write_structured_output(
        &mut body,
        &titled_schema(&schema(), Some("s")),
        StructuredOutputMode::ToolUseForced,
    );
    assert!(body.get("response_format").is_none());
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["required"],
        json!(["q"])
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "function": {"name": "s"}})
    );
}

/// THE FAILING INPUT: DeepSeek answers a forced `tool_choice` in thinking
/// mode with 400 ("Thinking mode does not support this tool_choice"), and
/// Anthropic's API refuses the same pair. A schema forced as a function
/// therefore goes out with thinking off in every spelling, whatever the
/// request asked for.
#[test]
fn a_forced_function_turns_thinking_off() {
    let mut body = json!({});
    write_thinking(&mut body, Some(2048), Some(true));
    write_structured_output(&mut body, &schema(), StructuredOutputMode::ToolUseForced);
    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert_eq!(body["think_budget"], json!(0));
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false)
    );
}

/// A caller's own tool catalog survives: the schema function is added beside
/// it, not written over it.
#[test]
fn a_tool_mode_appends_to_the_callers_tools() {
    let mut body = json!({"tools": [{"type": "function", "function": {"name": "lookup"}}]});
    write_structured_output(
        &mut body,
        &titled_schema(&schema(), Some("s")),
        StructuredOutputMode::ToolUseAuto,
    );
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lookup", "s"]);
    assert_eq!(body["tool_choice"], "auto");
}

/// A host that answers every chat request 400 and counts them.
async fn refusing_host() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let chat = move || {
        counter.fetch_add(1, SeqCst);
        async {
            let refusal = r#"{"error":{"message":"This response_format type is unavailable now"}}"#;
            (axum::http::StatusCode::BAD_REQUEST, refusal)
        }
    };
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}/v1"), hits)
}

/// DeepSeek's chat API in miniature: 400 to a `json_schema` request, a
/// forced function call answered.
async fn schema_refusing_host() -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    let chat = move |axum::Json(body): axum::Json<Value>| {
        log.lock().unwrap().push(body.clone());
        async move {
            if body.get("response_format").is_some() {
                let refusal =
                    json!({"error": {"message": "This response_format type is unavailable now"}});
                return (axum::http::StatusCode::BAD_REQUEST, axum::Json(refusal));
            }
            let name = body["tool_choice"]["function"]["name"].clone();
            let call = json!({"id": "c1", "type": "function",
                "function": {"name": name, "arguments": "{\"q\":\"why?\"}"}});
            let reply = json!({"choices": [{"message": {"content": null, "tool_calls": [call]},
                "finish_reason": "tool_calls"}]});
            (axum::http::StatusCode::OK, axum::Json(reply))
        }
    };
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{addr}/v1"), seen)
}

/// THE FAILING INPUT: a vendor on the default `json_schema` that refuses
/// it. Before 2026-10-03 the only way through was a per-provider
/// `structured_output_mode = "tool-use-forced"`. Now the refusal is asked
/// once more as a forced function call, the answer is the call's
/// arguments, and the provider remembers: the next call goes that way.
#[tokio::test]
async fn a_refused_json_schema_is_asked_again_as_a_function_and_remembered() {
    use sovereign_contracts::traits::InferenceProvider;
    let (url, seen) = schema_refusing_host().await;
    let request = CompletionRequest {
        prompt: "p".into(),
        model_id: Some("m".into()),
        structured_output: Some(titled_schema(&schema(), Some("phase 1 (atlas)"))),
        ..Default::default()
    };
    let host = RemoteApiProvider::new(&url, None, "m", 0).waiting_out_sheds();
    assert_eq!(
        host.complete(&request).await.unwrap().text,
        r#"{"q":"why?"}"#
    );
    assert_eq!(
        host.complete(&request).await.unwrap().text,
        r#"{"q":"why?"}"#
    );

    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        3,
        "one refusal, one retry, one call straight to the function"
    );
    assert_eq!(seen[0]["response_format"]["type"], "json_schema");
    for (i, body) in seen.iter().enumerate().skip(1) {
        assert!(body.get("response_format").is_none(), "body {i}: {body}");
        assert_eq!(body["tool_choice"]["function"]["name"], "phase_1__atlas_");
    }
}

/// A peer's refusal is a routing signal: reported at once, never re-asked.
/// Only a last-resort endpoint gets the forced-function retry.
#[tokio::test]
async fn only_a_last_resort_host_is_asked_again() {
    use sovereign_contracts::traits::InferenceProvider;
    use std::sync::atomic::Ordering::SeqCst;
    let (url, hits) = refusing_host().await;
    let request = CompletionRequest {
        prompt: "p".into(),
        model_id: Some("m".into()),
        structured_output: Some(schema()),
        ..Default::default()
    };
    let peer = RemoteApiProvider::new(&url, None, "m", 0);
    assert!(peer.complete(&request).await.is_err());
    assert_eq!(hits.load(SeqCst), 1, "a peer is not re-asked");

    let last_resort = RemoteApiProvider::new(&url, None, "m", 0).waiting_out_sheds();
    let err = last_resort
        .complete(&request)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(hits.load(SeqCst), 3, "the last resort is asked once more");
    assert!(
        err.contains("forced function call"),
        "both refusals named: {err}"
    );
}

#[test]
fn an_untitled_schema_is_named_structured() {
    let mut body = json!({});
    write_structured_output(&mut body, &schema(), StructuredOutputMode::JsonSchema);
    assert_eq!(body["response_format"]["json_schema"]["name"], "structured");
}

#[test]
fn an_unfoldable_name_still_names_a_function() {
    assert_eq!(openai_function_name(""), "emit_response");
    assert_eq!(openai_function_name(&"x".repeat(80)).len(), 64);
}

fn provider() -> crate::RemoteApiProvider {
    crate::RemoteApiProvider::new("http://127.0.0.1:9/v1", None, "m", 0)
}

fn named_request() -> sovereign_contracts::types::CompletionRequest {
    sovereign_contracts::types::CompletionRequest {
        prompt: "p".into(),
        model_id: Some("m".into()),
        ..Default::default()
    }
}

/// THE FAILING INPUT: enrich's calls to its own daemon, sent as forwards,
/// would carry `forward_budget: 0` (an unstated budget is one hop, and a
/// forward spends it), so the daemon could never hand them to a peer.
#[test]
fn an_originating_request_spends_no_hop_and_synthesizes_no_envelope() {
    let forwarded = provider().build_request(&named_request()).unwrap();
    assert_eq!(
        forwarded["oicp"]["forward_budget"], 0,
        "a forward spends the hop"
    );

    let origin = provider().originating();
    assert!(origin
        .build_request(&named_request())
        .unwrap()
        .get("oicp")
        .is_none());

    let mut composed = named_request();
    composed.oicp = Some(
        sovereign_contracts::oicp::InferenceRequirements::new()
            .with_latency_class(sovereign_contracts::oicp::LatencyClass::Fast)
            .with_max_output_tokens(512),
    );
    let body = origin.build_request(&composed).unwrap();
    assert_eq!(body["oicp"]["latency_class"], "fast");
    assert_eq!(body["oicp"]["max_output_tokens"], 512);
    assert!(
        body["oicp"].get("forward_budget").is_none(),
        "the caller's envelope crosses verbatim: {body}"
    );
}

#[test]
fn extra_params_land_last_and_can_override() {
    let p = provider().with_extra_params(Some(json!({
        "provider": {"require_parameters": true},
        "temperature": 0.9,
    })));
    let mut req = named_request();
    req.temperature = Some(0.2);
    let body = p.build_request(&req).unwrap();
    assert_eq!(body["provider"], json!({"require_parameters": true}));
    assert_eq!(body["temperature"], 0.9);
}

#[test]
fn the_hosts_mode_spells_the_requests_schema() {
    let p = provider().with_structured_output_mode(StructuredOutputMode::ToolUseForced);
    let mut req = named_request();
    req.structured_output = Some(titled_schema(&schema(), Some("phase 1 (atlas)")));
    let body = p.build_request(&req).unwrap();
    assert!(body.get("response_format").is_none(), "{body}");
    assert_eq!(body["tools"][0]["function"]["name"], "phase_1__atlas_");
    assert_eq!(body["tool_choice"]["function"]["name"], "phase_1__atlas_");
}

/// THE FAILING INPUT: an agent's turn the daemon took the conversation
/// path for. Sent as `prompt`, the whole history went to llama-server as
/// one user turn of `Tool[id]:` lines (e2e-swe arm D, 2026-10-09).
#[test]
fn a_conversation_goes_out_as_its_own_messages() {
    let history = vec![
        json!({"role": "system", "content": "You are an agent."}),
        json!({"role": "user", "content": "Fix the bug."}),
        json!({"role": "assistant", "content": "", "tool_calls": [{"id": "call_1",
            "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]}),
        json!({"role": "tool", "tool_call_id": "call_1", "content": "src"}),
    ];
    let mut req = named_request();
    req.prompt =
        "User: Fix the bug.\n\nAssistant: <tool_call>…\n\nTool[call_1]: src\n\nAssistant:".into();
    req.system_message = Some("You are an agent.".into());
    req.prompt_shape = Some(PromptShape::Conversation {
        messages: history.clone(),
    });
    let body = provider().build_request(&req).unwrap();
    assert_eq!(body["messages"], json!(history), "{body}");

    req.prompt_shape = None;
    let flat = provider().build_request(&req).unwrap();
    assert_eq!(flat["messages"][1]["content"], json!(req.prompt));
}

fn llguidance() -> crate::RemoteApiProvider {
    provider().with_grammar(GrammarSupport::Llguidance)
}

fn constrained(lark: Option<&str>, urls: &[&str]) -> sovereign_contracts::types::CompletionRequest {
    let mut req = named_request();
    req.lark_grammar = lark.map(str::to_string);
    if !urls.is_empty() {
        req.url_allowlist = Some(urls.iter().map(|u| u.to_string()).collect());
    }
    req
}

fn with_tools(
    mut req: sovereign_contracts::types::CompletionRequest,
) -> sovereign_contracts::types::CompletionRequest {
    req.tools = Some(vec![sovereign_contracts::types::ToolSchema {
        name: "bash".into(),
        description: Some("run".into()),
        parameters: json!({"type": "object"}),
    }]);
    req
}

/// A daemon far end keeps its extension fields, as before.
#[test]
fn a_sovereign_host_gets_the_extension_fields() {
    let mut req = constrained(Some("start: \"a\""), &["https://a.test/x"]);
    req.evidence_id_allowlist = Some(vec!["ev-T1-0001".into()]);
    let body = provider().build_request(&req).unwrap();
    assert_eq!(body["lark_grammar"], "start: \"a\"");
    assert_eq!(body["url_allowlist"], json!(["https://a.test/x"]));
    assert_eq!(body["evidence_id_allowlist"], json!(["ev-T1-0001"]));
    assert!(body.get("grammar").is_none(), "{body}");
}

/// THE FAILING INPUT: a deep-research draft on a remote llama-server. Its
/// `url_allowlist` went out as a field llama-server ignores, so the draft
/// was unmasked (14 of 15 URLs outside the list, 2026-10-09 probe).
#[test]
fn an_llguidance_host_gets_the_allow_list_as_its_grammar() {
    let body = llguidance()
        .build_request(&constrained(None, &["https://a.test/x"]))
        .unwrap();
    let urls = vec!["https://a.test/x".to_string()];
    let want = decode_allowlist::llguidance_lark(&[(&decode_allowlist::URL, &urls)]).unwrap();
    assert_eq!(body["grammar"], json!(want));
    assert!(body.get("url_allowlist").is_none(), "{body}");
}

#[test]
fn an_llguidance_host_gets_a_lark_grammar_with_its_header_once() {
    let body = llguidance()
        .build_request(&constrained(Some("start: \"a\""), &[]))
        .unwrap();
    assert_eq!(body["grammar"], "%llguidance {}\nstart: \"a\"");
    assert!(body.get("lark_grammar").is_none(), "{body}");
    let headed = "%llguidance {}\nstart: \"b\"";
    let body = llguidance()
        .build_request(&constrained(Some(headed), &[]))
        .unwrap();
    assert_eq!(body["grammar"], headed);
}

/// llama-server answers 400 to a grammar beside tools; its own tool-call
/// grammar stands in for an envelope grammar, but nothing stands in for an
/// allow-list.
#[test]
fn beside_tools_a_lark_grammar_is_dropped_and_an_allow_list_refused() {
    let body = llguidance()
        .build_request(&with_tools(constrained(Some("start: \"a\""), &[])))
        .unwrap();
    assert!(
        body.get("grammar").is_none() && body.get("lark_grammar").is_none(),
        "{body}"
    );
    assert!(body["tools"].as_array().is_some_and(|t| t.len() == 1));
    let refused = llguidance().build_request(&with_tools(constrained(None, &["https://a.test/x"])));
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn what_one_grammar_cannot_carry_is_refused() {
    let both =
        llguidance().build_request(&constrained(Some("start: \"a\""), &["https://a.test/x"]));
    assert!(both.is_err(), "{both:?}");
    let mut beside_schema = constrained(None, &["https://a.test/x"]);
    beside_schema.structured_output = Some(schema());
    let refused = llguidance()
        .with_structured_output_mode(StructuredOutputMode::JsonSchema)
        .build_request(&beside_schema);
    assert!(refused.is_err(), "{refused:?}");
}

/// THE FAILING INPUT for prompt.assistant-prefix: llama-server copied the
/// `assistant_prefix` field and never read it, so the model started a fresh
/// turn and continue-truncated-synthesis restarted instead of continuing.
/// It takes the prefix as a trailing assistant message, which it continues;
/// a daemon keeps the extension field its adapter reads.
#[test]
fn an_llguidance_host_continues_the_assistant_prefix_as_a_trailing_message() {
    let mut req = named_request();
    req.system_message = Some("s".into());
    req.assistant_prefix = Some("Title:".into());
    let body = llguidance().build_request(&req).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{body}");
    assert_eq!(
        messages.last(),
        Some(&json!({"role": "assistant", "content": "Title:"}))
    );
    assert!(body.get("assistant_prefix").is_none(), "{body}");

    let daemon = provider().build_request(&req).unwrap();
    assert_eq!(daemon["assistant_prefix"], "Title:");
    assert_eq!(daemon["messages"].as_array().unwrap().len(), 2, "{daemon}");

    req.assistant_prefix = Some(String::new());
    let empty = llguidance().build_request(&req).unwrap();
    assert_eq!(
        empty["messages"].as_array().unwrap().len(),
        2,
        "an empty prefix continues nothing: {empty}"
    );
}

/// THE FAILING INPUT for prompt.templated-thinking-default: synthesis
/// (think_budget 512, enable_thinking unset) went to llama-server with no
/// kwarg, and a thinking template opened the think block the engine renders
/// closed. A conversation keeps its template's own default, on both sides.
#[test]
fn a_templated_request_goes_out_with_the_engines_thinking_default() {
    let kwarg = |req: &CompletionRequest| {
        llguidance()
            .build_request(req)
            .unwrap()
            .pointer("/chat_template_kwargs/enable_thinking")
            .cloned()
    };
    let mut req = named_request();
    assert_eq!(kwarg(&req), Some(json!(false)), "nothing set");
    req.think_budget = Some(512);
    assert_eq!(kwarg(&req), Some(json!(false)), "a positive budget");
    req.enable_thinking = Some(true);
    assert_eq!(kwarg(&req), Some(json!(true)), "asked for");
    req.enable_thinking = None;
    req.prompt_shape = Some(PromptShape::Conversation {
        messages: vec![json!({"role": "user", "content": "p"})],
    });
    assert_eq!(kwarg(&req), None, "a conversation: the template decides");
}

/// THE FAILING INPUT for decode.think-budget-positive: the budget went out
/// as `think_budget` and `thinking`, neither of which llama-server reads on
/// chat completions, so an open think block ran past it. llama-server reads
/// `reasoning_budget_tokens` (LS server-common.cpp:1360-1371).
#[test]
fn a_positive_budget_reaches_llama_server_as_reasoning_budget_tokens() {
    let mut req = named_request();
    req.enable_thinking = Some(true);
    req.think_budget = Some(512);
    let body = llguidance().build_request(&req).unwrap();
    assert_eq!(body["reasoning_budget_tokens"], json!(512), "{body}");
    req.think_budget = Some(0);
    let body = llguidance().build_request(&req).unwrap();
    assert!(body.get("reasoning_budget_tokens").is_none(), "{body}");
}

/// THE FAILING INPUT for decode.json-schema: the schema went to the host as
/// written, and a host follows JSON Schema, so `{"topic": "x", "extra": 1}`
/// passed where the engine, which closes declared objects, refused it. The
/// client sends the schema the engine compiles; an object that declares
/// nothing stays open on both (C7).
#[test]
fn a_schema_goes_out_closed_as_the_engine_compiles_it() {
    let mut req = named_request();
    req.structured_output = Some(schema());
    let body = llguidance().build_request(&req).unwrap();
    let sent = &body["response_format"]["json_schema"]["schema"];
    assert_eq!(sent["additionalProperties"], json!(false), "{body}");
    assert_eq!(sent["required"], json!(["q"]));

    req.structured_output = Some(json!({"type": "object"}));
    let body = llguidance().build_request(&req).unwrap();
    assert_eq!(
        body["response_format"]["json_schema"]["schema"],
        json!({"type": "object"}),
        "{body}"
    );
}

/// THE FAILING INPUT for decode.tool-choice-named: a named function went to
/// llama-server, which read it as "auto", so the call was optional and could
/// be any tool. It was refused there; it now goes as "required" over the one
/// named tool, the spelling the server reads, and the reply is checked for
/// the call (`call_check`; engine-swap decision 1). A name the list does not
/// offer is still refused. A daemon, and the string choices, pass unchanged.
#[test]
fn a_named_tool_choice_goes_as_required_over_that_tool_where_the_host_would_loosen_it() {
    let mut req = with_tools(named_request());
    req.tools
        .as_mut()
        .unwrap()
        .push(sovereign_contracts::types::ToolSchema {
            name: "read".into(),
            description: None,
            parameters: json!({"type": "object"}),
        });
    req.tool_choice = Some(json!({"type": "function", "function": {"name": "bash"}}));
    let narrowed = llguidance().build_request(&req).unwrap();
    assert_eq!(narrowed["tool_choice"], "required", "{narrowed}");
    let tools = narrowed["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{narrowed}");
    assert_eq!(tools[0]["function"]["name"], "bash");
    let daemon = provider().build_request(&req).unwrap();
    assert_eq!(daemon["tool_choice"]["function"]["name"], "bash");
    assert_eq!(daemon["tools"].as_array().unwrap().len(), 2);

    req.tool_choice = Some(json!({"type": "function", "function": {"name": "absent"}}));
    let refused = llguidance().build_request(&req);
    assert!(refused.is_err(), "{refused:?}");

    req.tool_choice = Some(json!("required"));
    let body = llguidance().build_request(&req).unwrap();
    assert_eq!(body["tool_choice"], "required");
}
