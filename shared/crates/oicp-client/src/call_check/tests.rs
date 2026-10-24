// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

use serde_json::{json, Value};
use sovereign_contracts::engine_config::GrammarSupport;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, ToolSchema};

use crate::RemoteApiProvider;

fn required_bash() -> CompletionRequest {
    CompletionRequest {
        prompt: "list the files".into(),
        tools: Some(vec![ToolSchema {
            name: "bash".into(),
            description: Some("run".into()),
            parameters: json!({"type": "object"}),
        }]),
        tool_choice: Some(json!("required")),
        ..Default::default()
    }
}

/// THE FAILING INPUT (battery case 77a7138f2caa): llama-server answered a
/// `required` request in prose, and the prose reached the caller as the
/// reply. It is asked once more, then the call fails naming the miss.
#[tokio::test]
async fn a_prose_reply_to_required_is_asked_again_then_fails_naming_the_miss() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let chat = move |axum::Json(body): axum::Json<Value>| {
        h.fetch_add(1, SeqCst);
        assert_eq!(body["tool_choice"], "required", "{body}");
        async {
            axum::Json(
                json!({"choices": [{"message": {"content": "I would run ls."},
                "finish_reason": "stop"}]}),
            )
        }
    };
    let provider = RemoteApiProvider::new(&serve(chat).await, None, "m", 8192)
        .originating()
        .with_grammar(GrammarSupport::Llguidance);
    let err = provider
        .complete(&required_bash())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(hits.load(SeqCst), 2, "one ask, one retry");
    assert!(err.contains("answered in prose"), "{err}");
    assert!(err.contains("I would run ls."), "{err}");
}

/// A miss and then a call: the retry's reply stands.
#[tokio::test]
async fn a_call_on_the_retry_stands() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let chat = move |axum::Json(_): axum::Json<Value>| {
        let n = h.fetch_add(1, SeqCst);
        async move {
            axum::Json(if n == 0 {
                json!({"choices": [{"message": {"content": "thinking aloud"}}]})
            } else {
                json!({"choices": [{"message": {"content": "", "tool_calls": [{"id": "c",
                    "type": "function", "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}]},
                    "finish_reason": "tool_calls"}]})
            })
        }
    };
    let provider = RemoteApiProvider::new(&serve(chat).await, None, "m", 8192)
        .originating()
        .with_grammar(GrammarSupport::Llguidance);
    let reply = provider.complete(&required_bash()).await.unwrap();
    assert_eq!(hits.load(SeqCst), 2);
    assert!(reply.text.contains("\"bash\""), "{}", reply.text);
}

/// A daemon enforces the call itself and may answer it as text: one ask,
/// its reply stands unchecked.
#[tokio::test]
async fn a_daemon_reply_is_not_checked() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let chat = move |axum::Json(_): axum::Json<Value>| {
        h.fetch_add(1, SeqCst);
        async { axum::Json(json!({"choices": [{"message": {"content": "prose"}}]})) }
    };
    let provider = RemoteApiProvider::new(&serve(chat).await, None, "m", 8192).originating();
    let reply = provider.complete(&required_bash()).await.unwrap();
    assert_eq!((hits.load(SeqCst), reply.text.as_str()), (1, "prose"));
}

/// The router's own answer to a Codex turn is a NATIVE call, not prose
/// (measured on `kind = "llama-server"`, Qwen3.5-2B, 8 of 8): this is what
/// es-t4's change sends it — a conversation — so the check must pass it
/// through unchanged, on the first ask, with nothing re-asked.
#[tokio::test]
async fn a_native_call_to_the_router_is_not_asked_again() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let chat = move |axum::Json(body): axum::Json<Value>| {
        h.fetch_add(1, SeqCst);
        // The conversation, whole, and the budget the router keeps.
        assert_eq!(body["messages"][0]["role"], "system", "{body}");
        assert_eq!(body["think_budget"], 0, "{body}");
        async {
            axum::Json(json!({"choices": [{"message": {"content": "", "tool_calls": [
                {"id": "c", "type": "function", "function": {
                    "name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}]},
                "finish_reason": "tool_calls"}]}))
        }
    };
    let provider = RemoteApiProvider::new(&serve(chat).await, None, "m", 8192)
        .originating()
        .with_grammar(GrammarSupport::Llguidance);
    let mut request = required_bash();
    request.think_budget = Some(0);
    request.prompt_shape = Some(sovereign_contracts::types::PromptShape::Conversation {
        messages: vec![json!({"role": "system", "content": "You are a coding agent."})],
    });
    let reply = provider.complete(&request).await.unwrap();
    assert_eq!(hits.load(SeqCst), 1, "the call stood, so nothing was re-asked");
    assert!(reply.text.contains("<tool_call>"), "{}", reply.text);
    assert!(reply.text.contains("\"bash\""), "{}", reply.text);
}

async fn serve<H, T>(chat: H) -> String
where
    H: axum::handler::Handler<T, ()>,
    T: 'static,
{
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}
