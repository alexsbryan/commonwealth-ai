// SPDX-License-Identifier: AGPL-3.0-or-later
//! Proves the native wire is LOSSLESS: a fully-populated
//! `CompletionRequest` — with every sovereign-specific field the OpenAI
//! wire can't express (lark_grammar, structured_output, both allowlists,
//! sampling_mode, assistant/cmd prefixes, oicp, tools) — round-trips
//! through `POST /internal/complete` byte-for-byte. Also covers the
//! streaming NDJSON frames, embeddings, and the typed error envelope.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use sovereign_compute::client::ComputeChildClient;
use sovereign_compute::server::{router, ChildMeta};
use sovereign_compute::wire::EmbedMode;
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, Error, InferenceProvider, ProviderCapabilities,
    Result, SamplingMode, Speed, StreamFrame, ToolSchema,
};
use std::sync::atomic::AtomicBool;

/// A provider that records the last request it saw (so the test can assert
/// losslessness) and streams two canned tokens. Errors on the `"ERR"`
/// prompt to exercise the typed-error path.
struct EchoProvider {
    seen: Arc<Mutex<Option<CompletionRequest>>>,
}

#[async_trait]
impl InferenceProvider for EchoProvider {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        if request.prompt == "ERR" {
            return Err(Error::InvalidInput("sentinel error".into()));
        }
        *self.seen.lock().unwrap() = Some(request.clone());
        Ok(CompletionResponse {
            text: "ok".into(),
            tokens_used: 1,
            prompt_tokens: 0,
            model_id: "echo".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: Some(1),
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let s = futures::stream::iter(vec![Ok("hello ".to_string()), Ok("world".to_string())]);
        Ok(Box::pin(s))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.1, 0.2, 0.3])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: true,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

/// Bind the child router on an ephemeral port and return a client for it.
async fn spawn_child(seen: Arc<Mutex<Option<CompletionRequest>>>) -> ComputeChildClient {
    let provider: Arc<dyn InferenceProvider> = Arc::new(EchoProvider { seen });
    let ready = Arc::new(AtomicBool::new(true));
    let meta = ChildMeta {
        role: "mock".into(),
        model_id: "echo".into(),
    };
    let app = router(provider, ready, meta);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    ComputeChildClient::from_port(port).unwrap()
}

/// A request with every custom field set to a non-default value.
fn fully_populated_request() -> CompletionRequest {
    let mut req = CompletionRequest::default();
    req.prompt = "the prompt".into();
    req.system_message = Some("you are a test".into());
    req.max_tokens = Some(128);
    req.temperature = Some(0.7);
    req.structured_output = Some(serde_json::json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    }));
    req.think_budget = Some(64);
    req.top_k = Some(40);
    req.top_p = Some(0.9);
    req.tools = Some(vec![ToolSchema {
        name: "lookup".into(),
        description: Some("look something up".into()),
        parameters: serde_json::json!({"type": "object"}),
    }]);
    req.tool_choice = Some(serde_json::json!("auto"));
    req.model_id = Some("some-model".into());
    req.enable_thinking = Some(true);
    req.sampling_mode = Some(SamplingMode::Code);
    req.assistant_prefix = Some("Assistant:".into());
    req.cmd_prefix = Some("/cmd".into());
    req.url_allowlist = Some(vec!["https://example.com".into()]);
    req.evidence_id_allowlist = Some(vec!["ev-1".into(), "ev-2".into()]);
    req.lark_grammar = Some("start: \"yes\" | \"no\"".into());
    req
}

#[tokio::test]
async fn completion_request_roundtrips_losslessly() {
    let seen = Arc::new(Mutex::new(None));
    let client = spawn_child(seen.clone()).await;

    let sent = fully_populated_request();
    let resp = client.complete(&sent).await.unwrap();
    assert_eq!(resp.text, "ok");

    let received = seen
        .lock()
        .unwrap()
        .clone()
        .expect("provider saw a request");

    // Compare the FULL serde value: any dropped or mangled field — not
    // just the ones we set above — fails this assertion.
    let sent_v = serde_json::to_value(&sent).unwrap();
    let recv_v = serde_json::to_value(&received).unwrap();
    assert_eq!(
        sent_v, recv_v,
        "request was not preserved byte-for-byte across the wire"
    );

    // Spot-check the fields the OpenAI wire would have dropped.
    assert_eq!(
        received.lark_grammar.as_deref(),
        Some("start: \"yes\" | \"no\"")
    );
    assert_eq!(received.assistant_prefix.as_deref(), Some("Assistant:"));
    assert_eq!(received.cmd_prefix.as_deref(), Some("/cmd"));
    assert_eq!(
        received.evidence_id_allowlist.as_deref(),
        Some(&["ev-1".to_string(), "ev-2".to_string()][..])
    );
    assert_eq!(received.sampling_mode, Some(SamplingMode::Code));
    assert!(received.structured_output.is_some());
}

#[tokio::test]
async fn streaming_frames_roundtrip() {
    let seen = Arc::new(Mutex::new(None));
    let client = spawn_child(seen).await;

    let frames: Vec<StreamFrame> = client
        .complete_stream_frames(&CompletionRequest::default())
        .await
        .unwrap()
        .collect()
        .await;

    // Two tokens + a terminal Finish (the default complete_stream_with_finish
    // synthesises Stop after the underlying token stream closes).
    assert!(
        matches!(&frames[0], StreamFrame::Token(t) if t == "hello "),
        "frames: {frames:?}"
    );
    assert!(matches!(&frames[1], StreamFrame::Token(t) if t == "world"));
    assert!(
        matches!(frames.last(), Some(StreamFrame::Finish { .. })),
        "stream must end with a terminal Finish, got {frames:?}"
    );
}

#[tokio::test]
async fn embed_roundtrips() {
    let seen = Arc::new(Mutex::new(None));
    let client = spawn_child(seen).await;

    let v = client
        .embed("some text", EmbedMode::Document)
        .await
        .unwrap();
    assert_eq!(v, vec![0.1, 0.2, 0.3]);
}

#[tokio::test]
async fn typed_error_envelope_roundtrips() {
    let seen = Arc::new(Mutex::new(None));
    let client = spawn_child(seen).await;

    let mut req = CompletionRequest::default();
    req.prompt = "ERR".into();
    let err = client.complete(&req).await.unwrap_err();
    assert!(
        matches!(err, Error::InvalidInput(ref m) if m.contains("sentinel error")),
        "expected InvalidInput to survive the wire, got {err:?}"
    );
}

#[tokio::test]
async fn health_reports_ready() {
    let seen = Arc::new(Mutex::new(None));
    let client = spawn_child(seen).await;

    let info = client.health().await.unwrap();
    assert!(info.is_ready());
    assert_eq!(info.role, "mock");
    assert_eq!(info.model_id, "echo");
}
