// SPDX-License-Identifier: AGPL-3.0-or-later
//! `OpenAiPassthrough` against a stub server that renders with the SAME
//! contracts renderers serve's routes use, so a frame the relay drops or
//! reshapes fails here.

use super::*;
use axum::routing::{get, post};
use sovereign_contracts::oicp::openai_types::{FunctionCall, StreamUsage as WireUsage};
use sovereign_contracts::openai_http::{sse_item, ChunkHeader, SseItem, DONE};

/// The frames every stream test relays, a tool call and usage included.
fn chat_script() -> Vec<WireFrame> {
    vec![
        WireFrame::Token("hel".into()),
        WireFrame::Token("lo".into()),
        WireFrame::ToolCalls(vec![ToolCall {
            id: "call-1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "lookup".into(),
                arguments: "{\"q\":1}".into(),
            },
        }]),
        WireFrame::Finish {
            reason: WireFinish::ToolCalls,
            usage: Some(WireUsage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
            }),
        },
    ]
}

fn sse_body(items: impl IntoIterator<Item = SseItem>) -> String {
    items
        .into_iter()
        .map(|item| match item {
            SseItem::Data(d) => format!("data: {d}\n\n"),
            SseItem::Comment(c) => format!(": {c}\n\n"),
        })
        .collect()
}

/// A stub serve: chat (both shapes), FIM, and the manifest. `truncate` ends
/// every stream before its finish frame.
async fn stub(truncate: bool) -> String {
    let chat = move |axum::Json(req): axum::Json<serde_json::Value>| async move {
        if req["stream"] == serde_json::json!(true) {
            let header = ChunkHeader::new(Some("m".into()));
            let mut frames = chat_script();
            if truncate {
                frames.pop();
            }
            let mut items: Vec<SseItem> =
                frames.into_iter().map(|f| sse_item(&header, f)).collect();
            if !truncate {
                items.push(SseItem::Data(DONE.to_string()));
            }
            return ([("content-type", "text/event-stream")], sse_body(items)).into_response();
        }
        axum::Json(serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "created": 1,
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": format!("echo:{}", req["turn_admission"].as_str().unwrap_or("none"))},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }))
        .into_response()
    };
    let fim = |axum::Json(req): axum::Json<serde_json::Value>| async move {
        assert_eq!(req["prefix"], "fn main(");
        assert_eq!(req["stream"], true);
        let start = FimStreamStart {
            stream: Box::pin(futures::stream::iter(vec![
                WireFrame::Token("x".into()),
                WireFrame::Debug(serde_json::json!({"rule": "eol"})),
                WireFrame::Finish {
                    reason: WireFinish::Stop,
                    usage: None,
                },
            ])),
            model_id: "coder".into(),
            slot: "edit".into(),
            fim_style: "qwen_coder".into(),
        };
        let items: Vec<SseItem> = sovereign_contracts::fim_http::fim_sse_items(start, true, None)
            .collect()
            .await;
        ([("content-type", "text/event-stream")], sse_body(items))
    };
    let manifest = || async {
        axum::Json(serde_json::json!({
            "oicp_version": "0.4",
            "models": [],
        }))
    };
    let app = axum::Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/completions", post(fim))
        .route("/oicp/v1/capabilities", get(manifest));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

use axum::response::IntoResponse;

fn passthrough_to(base: &str) -> OpenAiPassthrough {
    let split = SplitInferenceProvider::new(
        &format!("{base}/v1"),
        "primary".into(),
        "embed".into(),
        4096,
        String::new(),
    );
    let local: Arc<dyn InferenceProvider> = Arc::new(SplitInferenceProvider::new(
        &format!("{base}/v1"),
        "primary".into(),
        "embed".into(),
        4096,
        String::new(),
    ));
    OpenAiPassthrough::new(local, &split)
}

fn request() -> ChatCompletionRequest {
    serde_json::from_value(serde_json::json!({
        "messages": [{"role": "user", "content": "hi"}],
        "turn_admission": "turn-7",
    }))
    .unwrap()
}

#[tokio::test]
async fn a_chat_turn_is_relayed_with_the_admission_the_route_left() {
    let base = stub(false).await;
    let answer = passthrough_to(&base)
        .chat_completion(request())
        .await
        .expect("the stub answers");
    let text = serde_json::to_value(&answer).unwrap()["choices"][0]["message"]["content"].clone();
    assert_eq!(text, "echo:turn-7");
}

#[tokio::test]
async fn a_streamed_turn_comes_back_frame_for_frame() {
    let base = stub(false).await;
    let frames: Vec<WireFrame> = passthrough_to(&base)
        .chat_completion_stream(request())
        .await
        .expect("the stream starts")
        .collect()
        .await;
    assert_eq!(
        format!("{frames:?}"),
        format!("{:?}", chat_script()),
        "every frame serve renders must come back unchanged"
    );
}

#[tokio::test]
async fn a_stream_cut_short_ends_in_an_error_never_an_invented_finish() {
    let base = stub(true).await;
    let frames: Vec<WireFrame> = passthrough_to(&base)
        .chat_completion_stream(request())
        .await
        .expect("the stream starts")
        .collect()
        .await;
    match frames.last() {
        Some(WireFrame::Error(e)) => assert!(e.contains("without a finish reason"), "{e}"),
        other => panic!("a truncated stream must end in an Error frame, got {other:?}"),
    }
}

#[tokio::test]
async fn an_absent_server_is_an_error_naming_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let err = passthrough_to(&base)
        .chat_completion(request())
        .await
        .expect_err("nothing listens there");
    assert!(err.to_string().contains("failed"), "{err}");
}

#[tokio::test]
async fn the_manifest_is_the_servers_once_read_and_none_before() {
    let base = stub(false).await;
    let relay = passthrough_to(&base);
    assert!(
        relay.provider_manifest().is_none(),
        "never read: no manifest"
    );
    assert!(relay.read_manifest().await);
    assert!(relay.provider_manifest().is_some());
}

#[tokio::test]
async fn a_fim_stream_comes_back_with_its_debug_frame() {
    let base = stub(false).await;
    let start = passthrough_to(&base)
        .fim_completion_stream(FimCompletionRequest {
            prefix: "fn main(".into(),
            suffix: String::new(),
            path: None,
            language: None,
            max_tokens: None,
            temperature: None,
            stop: Vec::new(),
            debug: true,
            raw_prompt: None,
        })
        .await
        .expect("the stream starts");
    let frames: Vec<WireFrame> = start.stream.collect().await;
    assert!(matches!(frames.first(), Some(WireFrame::Token(t)) if t == "x"));
    assert!(frames.iter().any(|f| matches!(f, WireFrame::Debug(_))));
    assert!(matches!(
        frames.last(),
        Some(WireFrame::Finish {
            reason: WireFinish::Stop,
            ..
        })
    ));
}

/// The names of the `fn`s declared in `text`, in order.
fn fn_names(text: &str) -> Vec<&str> {
    text.split("fn ")
        .skip(1)
        .filter_map(|rest| rest.split('(').next())
        .filter(|name| name.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .collect()
}

/// The module doc's "every `InferenceProvider` method forwards" as code: a
/// method with a default compiles unforwarded and answers for the relay.
#[test]
fn the_relay_forwards_every_inference_provider_method() {
    let traits = include_str!("../../sovereign/crates/sovereign-contracts/src/traits.rs");
    let start = traits
        .find("pub trait InferenceProvider")
        .expect("the trait is declared in traits.rs");
    let body = &traits[start..];
    let end = body.find("\n}\n").expect("the trait's closing brace");
    let methods = fn_names(&body[..end]);
    assert!(methods.len() > 20, "parsed too few methods: {methods:?}");

    let relay = include_str!("openai_passthrough.rs");
    let start = relay
        .find("impl InferenceProvider for OpenAiPassthrough")
        .expect("the relay's impl");
    let body = &relay[start..];
    let end = body.find("\n}\n").expect("the impl's closing brace");
    let impl_text = &body[..end];
    let forwarded = fn_names(impl_text);
    let missing: Vec<&str> = methods
        .iter()
        .copied()
        .filter(|m| !forwarded.contains(m) || !impl_text.contains(&format!(".{m}(")))
        .collect();
    assert!(
        missing.is_empty(),
        "OpenAiPassthrough does not forward: {missing:?}"
    );
}
