// SPDX-License-Identifier: AGPL-3.0-or-later
//! The chat round trip is lossless (pb-svrn-dials-serve, phase-b-26).
//!
//! On the dialing path the svrn daemon builds a `CompletionRequest`, the
//! terminal arm renders it onto the OpenAI chat wire
//! (`RemoteApiProvider::build_request`), and serve rebuilds one from that wire
//! (`SovereignInferenceAdapter::build_completion_request`) before its engine
//! runs it. Two translators, one request: this test holds that the request
//! serve's engine receives is the one the daemon built, field by field, so a
//! field one side drops is a red line naming it rather than a turn that runs a
//! request nobody built (principles 6, 8).
//!
//! It runs serve's own route bundles behind the host kit's shell on a free
//! loopback port, over a provider that records what it is handed, and dials it
//! with the daemon's terminal arm. No model is loaded.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use sovereign_contracts::oicp::{InferenceRequirements, LatencyClass};
use sovereign_contracts::traits::InferenceProvider;
use sovereign_contracts::types::{
    CompletionRequest, CompletionResponse, Depth, FinishReason, ProviderCapabilities, SamplingMode,
    Speed, StreamFrame, ToolSchema, TurnAdmission,
};
use sovereign_inference::remote::SplitInferenceProvider;

/// Records the request the engine would run, and answers one token.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Option<CompletionRequest>>,
}

#[async_trait]
impl InferenceProvider for Recorder {
    async fn complete(
        &self,
        req: &CompletionRequest,
    ) -> sovereign_contracts::Result<CompletionResponse> {
        *self.seen.lock().unwrap() = Some(req.clone());
        Ok(CompletionResponse {
            text: "ok".into(),
            tokens_used: 1,
            prompt_tokens: 0,
            model_id: "recorder".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: Some(FinishReason::Stop),
            completion_tokens: Some(1),
        })
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
    ) -> sovereign_contracts::Result<
        Pin<Box<dyn Stream<Item = sovereign_contracts::Result<String>> + Send>>,
    > {
        *self.seen.lock().unwrap() = Some(req.clone());
        Ok(Box::pin(futures::stream::iter(vec![Ok("ok".to_string())])))
    }

    async fn complete_stream_with_finish(
        &self,
        req: &CompletionRequest,
    ) -> sovereign_contracts::Result<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        *self.seen.lock().unwrap() = Some(req.clone());
        Ok(Box::pin(futures::stream::iter(vec![
            StreamFrame::Token("ok".to_string()),
            StreamFrame::Finish {
                reason: FinishReason::Stop,
                usage: None,
            },
        ])))
    }

    async fn embed(&self, _text: &str) -> sovereign_contracts::Result<Vec<f32>> {
        unimplemented!("not on this path")
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 32_768,
            supports_structured_output: true,
            relative_speed: Speed::Slow,
            relative_reasoning: Depth::Moderate,
        }
    }

    /// A fast and a primary slot are loaded, so serve's slot pick has both
    /// tiers to choose from, as on a stock node.
    fn model_id_for(&self, speed: Speed) -> String {
        match speed {
            Speed::Fast => "fast-model".to_string(),
            Speed::Medium | Speed::Slow => "primary-model".to_string(),
        }
    }
}

async fn start_serve(recorder: Arc<Recorder>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a free loopback port");
    let addr = listener.local_addr().expect("bound address");
    let routes = sovereign_serve::bundles(recorder as Arc<dyn InferenceProvider>);
    tokio::spawn(host_kit::shell::serve(
        [listener],
        routes,
        futures::future::pending(),
    ));
    addr
}

/// serve's routes as the shell mounts them, but every request's connection
/// reads as `seen_from`: a caller on another host, without needing one.
async fn start_serve_seen_from(recorder: Arc<Recorder>, seen_from: SocketAddr) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a free loopback port");
    let addr = listener.local_addr().expect("bound address");
    let app = host_kit::shell::mount(sovereign_serve::bundles(
        recorder as Arc<dyn InferenceProvider>,
    ))
    .layer(axum::middleware::from_fn(
        move |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
            req.extensions_mut()
                .insert(axum::extract::ConnectInfo(seen_from));
            next.run(req).await
        },
    ));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    addr
}

/// Send `sent` through the daemon's terminal arm to serve, streamed as a chat
/// turn is, and return what serve's engine was handed.
async fn round_trip(sent: &CompletionRequest) -> CompletionRequest {
    let recorder = Arc::new(Recorder::default());
    let addr = start_serve(Arc::clone(&recorder)).await;
    dial(addr, &recorder, sent).await
}

async fn dial(
    addr: SocketAddr,
    recorder: &Recorder,
    sent: &CompletionRequest,
) -> CompletionRequest {
    let arm = SplitInferenceProvider::new(
        &format!("http://{addr}/v1"),
        "primary".to_string(),
        "unknown".to_string(),
        32_768,
        String::new(),
    );
    let mut stream = arm
        .complete_stream_with_finish(sent)
        .await
        .expect("serve answers the turn");
    while let Some(frame) = stream.next().await {
        if let StreamFrame::Error(e) = frame {
            panic!("serve reported an error frame: {e}");
        }
    }
    let seen = recorder.seen.lock().unwrap().clone();
    seen.expect("serve's engine was handed the request")
}

/// The terminal arm's chat model id: its pin for a Slow/Medium turn that
/// names no model (`RemoteApiProvider::build_request`).
const ARM_CHAT_ID: &str = "primary";

/// Every serialized field of `sent` that `received` does not carry unchanged,
/// by name, with both values. The transforms the wire owes are named, and
/// each is held to what the engine reads of it:
/// - a caller's envelope spends the hop a forward spends
///   (`decremented_for_forward`, oicp-types requirements.rs);
/// - with no envelope and no model named, the tier crosses as the arm's
///   pinned alias (Slow/Medium) or a synthesized envelope (Fast). The engine
///   reads an envelope only for `capability_hint` (embedded engine.rs,
///   `pick_slot`), which a synthesized one never carries, and the
///   alias resolves to the tier `preferred_speed` already names, which is
///   compared exactly.
fn differences(sent: &CompletionRequest, received: &CompletionRequest) -> Vec<String> {
    let mut expected = sent.clone();
    let mut out = Vec::new();
    match &sent.oicp {
        Some(o) => expected.oicp = Some(o.decremented_for_forward()),
        None => {
            if let Some(hint) = received
                .oicp
                .as_ref()
                .and_then(|o| o.capability_hint.as_ref())
            {
                out.push(format!(
                    "oicp: sent None, engine got a capability hint {hint:?}"
                ));
            }
            expected.oicp = received.oicp.clone();
        }
    }
    if sent.model_id.is_none() {
        match received.model_id.as_deref() {
            None | Some(ARM_CHAT_ID) => expected.model_id = received.model_id.clone(),
            Some(other) => out.push(format!("model_id: sent None, engine got {other:?}")),
        }
    }
    let want = serde_json::to_value(&expected).expect("request serializes");
    let got = serde_json::to_value(received).expect("request serializes");
    let (want, got) = (want.as_object().unwrap(), got.as_object().unwrap());
    let mut keys: Vec<&String> = want.keys().chain(got.keys()).collect();
    keys.sort();
    keys.dedup();
    out.extend(
        keys.into_iter()
            .filter(|k| want.get(*k) != got.get(*k))
            .map(|k| format!("{k}: sent {:?}, engine got {:?}", want.get(k), got.get(k))),
    );
    // Never serialized, by construction (`#[serde(skip)]`), so compared
    // apart: a continuation the daemon admitted must reach the queue that
    // decides whether to shed it.
    if sent.admission != received.admission {
        out.push(format!(
            "admission: sent {:?}, engine got {:?}",
            sent.admission, received.admission
        ));
    }
    out
}

/// Fields the chat wire does not yet carry. Each crosses as an extension
/// field, one behaviour fix per field (phase-b-27), and leaves this set when
/// it does. Held as an exact set, so a field that starts crossing, or a new
/// loss, turns this red.
const NO_CHAT_WIRE_FIELD: &[&str] = &[];

fn assert_lossless(
    case: &str,
    sent: &CompletionRequest,
    received: &CompletionRequest,
    known: &[&str],
) {
    let lost = differences(sent, received);
    let mut names: Vec<&str> = lost
        .iter()
        .map(|l| l.split(':').next().unwrap_or(""))
        .collect();
    names.sort_unstable();
    assert!(
        names == known,
        "{case}: the chat round trip changed {} field(s) the engine reads (known without a \
         wire field: {known:?}):\n  {}",
        lost.len(),
        lost.join("\n  ")
    );
}

/// Every field the engine reads set away from its default, on the tools
/// shape (tools pin the primary, so `preferred_speed` is `Slow` here and the
/// fast case below covers `Fast`). `prompt_shape` stays unset: a raw prompt
/// leaves the chat wire for serve's `/v1/completions`
/// (`serve_loopback::wants_raw_completion`), which is not this round trip.
fn golden() -> CompletionRequest {
    let mut req = CompletionRequest::new("Which river runs through Vienna?");
    req.system_message = Some("Answer in one word.".to_string());
    req.preferred_speed = Speed::Slow;
    req.max_tokens = Some(77);
    req.temperature = Some(0.33);
    req.structured_output = Some(serde_json::json!({"type": "object"}));
    req.think_budget = Some(123);
    req.stable_prefix_len = Some(5);
    req.top_k = Some(17);
    req.top_p = Some(0.61);
    req.oicp = Some(
        InferenceRequirements::new()
            .with_latency_class(LatencyClass::Normal)
            .with_context_tokens(4096),
    );
    req.tools = Some(vec![ToolSchema {
        name: "lookup".to_string(),
        description: Some("Look a river up.".to_string()),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }]);
    req.tool_choice = Some(serde_json::json!("auto"));
    req.model_id = Some("golden-model".to_string());
    req.enable_thinking = Some(true);
    req.sampling_mode = Some(SamplingMode::Code);
    req.assistant_prefix = Some("The river is".to_string());
    req.cmd_prefix = Some("apply_patch".to_string());
    req.url_allowlist = Some(vec!["https://example.org/danube".to_string()]);
    req.evidence_id_allowlist = Some(vec!["ev-T1-0001".to_string()]);
    req.lark_grammar = Some("start: \"Danube\"".to_string());
    req.admission = Some(TurnAdmission::new("turn-golden"));
    req
}

#[tokio::test]
async fn every_field_the_engine_reads_survives_the_chat_round_trip() {
    let sent = golden();
    let received = round_trip(&sent).await;
    assert_lossless("golden", &sent, &received, NO_CHAT_WIRE_FIELD);
}

/// The request the pre-registered first-token bar measures
/// (sovereign-daemon tests/main/serve_latency_bars.rs): the shape a plain
/// chat turn takes, with no model named and no envelope.
#[tokio::test]
async fn the_latency_bars_request_survives_the_chat_round_trip() {
    let mut sent = CompletionRequest::new("Name one river in Europe.");
    sent.max_tokens = Some(8);
    sent.enable_thinking = Some(false);
    let received = round_trip(&sent).await;
    assert_lossless("latency bar", &sent, &received, &[]);
}

/// A fast-tier turn with no model named: the slot shadow crosses as a latency
/// class and must come back as the same tier.
#[tokio::test]
async fn a_fast_turn_survives_the_chat_round_trip() {
    let mut sent = CompletionRequest::new("Say hi.");
    sent.preferred_speed = Speed::Fast;
    sent.max_tokens = Some(16);
    let received = round_trip(&sent).await;
    assert_lossless("fast", &sent, &received, &[]);
}

/// The admission id is honoured only from a caller on serve's own host
/// (phase-b-27): the same admitted turn, arriving from another host, reaches
/// the engine as fresh load.
#[tokio::test]
async fn an_admitted_turn_from_another_host_reaches_the_engine_as_fresh_load() {
    let mut sent = CompletionRequest::new("Say hi.");
    sent.admission = Some(TurnAdmission::new("turn-remote"));
    let recorder = Arc::new(Recorder::default());
    let addr = start_serve_seen_from(
        Arc::clone(&recorder),
        "10.0.0.2:40000".parse().expect("an address"),
    )
    .await;
    let received = dial(addr, &recorder, &sent).await;
    assert_eq!(
        received.admission, None,
        "a caller off this host must not park its call in serve's queue"
    );
}
