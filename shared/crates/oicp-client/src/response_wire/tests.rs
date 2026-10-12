// SPDX-License-Identifier: AGPL-3.0-or-later
//! Projection tests: llama-server's responses through the client's real
//! parsing code, judged as the conformance battery judges the engine's
//! (`bench/lanes/engine-swap/conformance.toml`, the `projection.*` rows).
//!
//! The bodies are llama-server's shapes at the battery's pinned revision
//! (`target/llama-server-vanilla/llama.cpp-035e227…`): a chunk per message
//! diff (`server-chat.cpp:607`), the finish chunk and the usage chunk
//! (`server-task.cpp:462-520`), and an error as one `data:` line carrying
//! `{"error": …}` with no `[DONE]` after it (`server-context.cpp:4294`).

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::{json, Value};
use sovereign_contracts::oicp::chat_turn::parse_assistant_turn;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{CompletionRequest, FinishReason, StreamFrame};

use crate::RemoteApiProvider;

/// One llama-server stream chunk around `delta`.
fn chunk(delta: Value, finish: Option<&str>) -> String {
    let body = json!({
        "choices": [{"finish_reason": finish, "index": 0, "delta": delta}],
        "created": 1, "id": "chatcmpl-1", "model": "m",
        "system_fingerprint": "b1", "object": "chat.completion.chunk",
    });
    format!("data: {body}\n\n")
}

/// llama-server's usage chunk, sent only when the request asked for it.
fn usage_chunk(prompt: u32, completion: u32) -> String {
    let body = json!({
        "choices": [], "created": 1, "id": "chatcmpl-1", "model": "m",
        "object": "chat.completion.chunk",
        "usage": {"completion_tokens": completion, "prompt_tokens": prompt,
                  "total_tokens": prompt + completion},
    });
    format!("data: {body}\n\n")
}

/// A llama-server that streams `chunks`, adds the usage chunk when the
/// request set `stream_options.include_usage` (as `server-task.cpp:502`
/// does), ends with `[DONE]`, and keeps every request body it saw.
async fn sse_server(chunks: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let chat = move |axum::Json(body): axum::Json<Value>| {
        let include_usage = body["stream_options"]["include_usage"] == json!(true);
        s.lock().unwrap().push(body);
        let mut out = chunks.concat();
        if include_usage {
            out.push_str(&usage_chunk(273, 63));
        }
        out.push_str("data: [DONE]\n\n");
        async move {
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                out,
            )
        }
    };
    (serve(chat).await, seen)
}

/// A llama-server answering every chat call with `body`.
async fn json_server(body: Value) -> String {
    serve(move || {
        let body = body.clone();
        async move { axum::Json(body) }
    })
    .await
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

/// A server that writes `head` and then each of `parts` as its own write,
/// a pause between them so each arrives as its own byte chunk, then closes.
async fn raw_server(head: &'static str, parts: Vec<&'static str>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the whole request so closing never resets it.
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap_or(0);
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        socket.write_all(head.as_bytes()).await.unwrap();
        for part in parts {
            socket.write_all(part.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
    url
}

fn provider(url: &str) -> RemoteApiProvider {
    RemoteApiProvider::new(url, None, "m", 8192).originating()
}

fn ask(prompt: &str) -> CompletionRequest {
    CompletionRequest {
        prompt: prompt.into(),
        ..Default::default()
    }
}

/// The streamed text, and the terminal frame.
async fn stream(url: &str, request: &CompletionRequest) -> (String, StreamFrame) {
    let mut frames = provider(url)
        .complete_stream_with_finish(request)
        .await
        .unwrap();
    let mut text = String::new();
    let mut terminal = None;
    while let Some(frame) = frames.next().await {
        match frame {
            StreamFrame::Token(t) => {
                assert!(terminal.is_none(), "a token after the terminal frame");
                text.push_str(&t);
            }
            done => terminal = Some(done),
        }
    }
    (
        text,
        terminal.expect("every stream ends in a terminal frame"),
    )
}

// ── projection.reasoning ────────────────────────────────────────────────

/// llama-server moves thinking into `reasoning_content` (its default
/// reasoning format). The engine returns it inline, opening the text with
/// the `<think>` its prompt left open, and every caller (the adapter's
/// `parse_reply` and `stream_turn`, the battery's judge) splits it from there.
/// Before this the client read `content` alone and the thinking was lost.
#[tokio::test]
async fn a_reasoning_reply_reaches_the_caller_as_the_engines_think_block() {
    let url = json_server(json!({
        "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant",
            "content": "Paris.", "reasoning_content": "The capital of France is Paris."}}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 9, "total_tokens": 21},
    }))
    .await;
    let text = provider(&url)
        .complete(&ask("capital?"))
        .await
        .unwrap()
        .text;
    let turn = parse_assistant_turn(&text, &[]);
    assert_eq!(
        turn.reasoning_content.as_deref(),
        Some("The capital of France is Paris."),
        "{text:?}"
    );
    assert_eq!(turn.content, "Paris.");
}

#[tokio::test]
async fn a_streamed_reasoning_reply_reaches_the_caller_as_the_engines_think_block() {
    let (url, _) = sse_server(vec![
        chunk(json!({"role": "assistant", "content": null}), None),
        chunk(json!({"reasoning_content": "The capital"}), None),
        chunk(json!({"reasoning_content": " is Paris."}), None),
        chunk(json!({"content": "Paris"}), None),
        chunk(json!({"content": "."}), None),
        chunk(json!({}), Some("stop")),
    ])
    .await;
    let (text, _) = stream(&url, &ask("capital?")).await;
    let turn = parse_assistant_turn(&text, &[]);
    assert_eq!(
        turn.reasoning_content.as_deref(),
        Some("The capital is Paris."),
        "{text:?}"
    );
    assert_eq!(turn.content, "Paris.");
}

/// A structured answer is the JSON the caller parses: the thinking beside
/// it is not put in front of it. A guard, true before and after.
#[tokio::test]
async fn a_structured_answer_stays_the_json_alone() {
    let url = json_server(json!({
        "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant",
            "content": "{\"answer\":\"Paris\"}", "reasoning_content": "Easy."}}],
    }))
    .await;
    let request = CompletionRequest {
        structured_output: Some(json!({"type": "object"})),
        ..ask("capital?")
    };
    let text = provider(&url).complete(&request).await.unwrap().text;
    assert_eq!(text, "{\"answer\":\"Paris\"}");
}

// ── projection.stream-usage ─────────────────────────────────────────────

/// llama-server sends usage on a stream only when asked
/// (`stream_options.include_usage`); the engine's Finish frame always
/// carries it.
#[tokio::test]
async fn a_streamed_request_asks_for_usage_and_the_finish_frame_carries_it() {
    let (url, seen) = sse_server(vec![
        chunk(json!({"content": "hi"}), None),
        chunk(json!({}), Some("stop")),
    ])
    .await;
    let (text, terminal) = stream(&url, &ask("hello")).await;
    assert_eq!(text, "hi");
    assert_eq!(
        seen.lock().unwrap()[0]["stream_options"],
        json!({"include_usage": true})
    );
    let StreamFrame::Finish { reason, usage } = terminal else {
        panic!("{terminal:?}")
    };
    assert_eq!(reason, FinishReason::Stop);
    let usage = usage.expect("the usage chunk reaches the Finish frame");
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        ),
        (273, 63, 336)
    );
}

// ── projection.stream-tool-calls and C1 ─────────────────────────────────

/// Battery case 005b4492621e, as llama-server streamed it: the call in
/// `delta.tool_calls` fragments (id and name first, then the arguments in
/// pieces), and `tool_calls` as the finish reason. It arrived as a bare
/// Finish frame. It must arrive as the envelope the non-stream path
/// re-emits, and finish as the engine does (C1).
#[tokio::test]
async fn a_streamed_tool_call_arrives_as_the_one_envelope() {
    let call = |f: Value| json!({"tool_calls": [f]});
    let (url, _) = sse_server(vec![
        chunk(json!({"reasoning_content": "Read it."}), None),
        chunk(
            call(json!({"index": 0, "id": "abc", "type": "function",
                "function": {"name": "read_file"}})),
            None,
        ),
        chunk(
            call(json!({"index": 0, "function": {"arguments": "{\"path\":"}})),
            None,
        ),
        chunk(
            call(json!({"index": 0, "function": {"arguments": "\"src/main.rs\"}"}})),
            None,
        ),
        chunk(json!({}), Some("tool_calls")),
    ])
    .await;
    let (text, terminal) = stream(&url, &ask("Read src/main.rs.")).await;
    let turn = parse_assistant_turn(&text, &[]);
    assert_eq!(
        turn.reasoning_content.as_deref(),
        Some("Read it."),
        "{text:?}"
    );
    assert_eq!(turn.content, "");
    assert_eq!(turn.tool_calls.len(), 1, "{text:?}");
    assert_eq!(turn.tool_calls[0].name, "read_file");
    let args: Value = serde_json::from_str(&turn.tool_calls[0].arguments).unwrap();
    assert_eq!(args, json!({"path": "src/main.rs"}));
    assert!(
        matches!(
            terminal,
            StreamFrame::Finish {
                reason: FinishReason::Stop,
                ..
            }
        ),
        "the engine reports stop for a call it ended at EOS: {terminal:?}"
    );
}

/// C1 on the non-stream path (case 595a3db924f6): llama-server says
/// `tool_calls`, the engine `stop`, for the same call. The call is carried
/// in the text, and the adapter derives `tool_calls` from it for either.
#[tokio::test]
async fn a_tool_call_finishes_as_the_engine_does() {
    let url = json_server(json!({
        "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant",
            "content": "", "tool_calls": [{"id": "c", "type": "function",
            "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}}]}}],
        "usage": {"prompt_tokens": 280, "completion_tokens": 70, "total_tokens": 350},
    }))
    .await;
    let response = provider(&url).complete(&ask("README")).await.unwrap();
    assert_eq!(response.finish_reason, Some(FinishReason::Stop));
    let turn = parse_assistant_turn(&response.text, &[]);
    assert_eq!(turn.tool_calls[0].name, "read_file");
}

// ── projection.stream-error ─────────────────────────────────────────────

/// llama-server reports a failure mid-stream as one `data: {"error": …}`
/// line and closes, with no `[DONE]`. The engine ends in Finish{Error}.
#[tokio::test]
async fn an_error_chunk_ends_the_stream_in_an_error_finish() {
    let url = serve(|| async {
        let out = chunk(json!({"content": "par"}), None)
            + "data: {\"error\":{\"code\":500,\"message\":\"slot died\",\"type\":\"server_error\"}}\n\n";
        ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], out)
    })
    .await;
    let (text, terminal) = stream(&url, &ask("hello")).await;
    assert_eq!(text, "par");
    let StreamFrame::Finish {
        reason: FinishReason::Error(message),
        ..
    } = &terminal
    else {
        panic!("an error chunk must end in an error finish: {terminal:?}")
    };
    assert!(message.contains("slot died"), "{message}");
}

/// The connection drops mid-body: a body promised 4096 bytes and closed
/// after one chunk.
#[tokio::test]
async fn a_transport_error_ends_the_stream_in_an_error_finish() {
    let url = raw_server(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\n\r\n",
        vec!["data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"par\"}}]}\n\n"],
    )
    .await;
    let (text, terminal) = stream(&url, &ask("hello")).await;
    assert_eq!(text, "par");
    assert!(
        matches!(
            terminal,
            StreamFrame::Finish {
                reason: FinishReason::Error(_),
                ..
            }
        ),
        "a dropped connection is not a clean stop: {terminal:?}"
    );
}

// ── projection.stream-split-line ────────────────────────────────────────

/// One SSE line arriving in two byte chunks, through the untyped stream a
/// bare Split provider serves (CLI chat, desktop attach).
#[tokio::test]
async fn an_sse_line_split_across_two_chunks_is_delivered_whole() {
    let url = raw_server(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        vec![
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello \"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"del",
            "ta\":{\"content\":\"world\"}}]}\n\ndata: [DONE]\n\n",
        ],
    )
    .await;
    let mut pieces = provider(&url).complete_stream(&ask("hello")).await.unwrap();
    let mut text = String::new();
    while let Some(piece) = pieces.next().await {
        text.push_str(&piece.unwrap());
    }
    assert_eq!(text, "hello world");
}
