// SPDX-License-Identifier: AGPL-3.0-or-later
//! A client's vendor fields on `/v1/chat/completions` reach a third-party
//! engine: thinking off, and the logprobs it asked for, which come back.
//!
//! Observed 2026-10-20 with DeepSeek as a node's remote engine: the body
//! `max_tokens: 1, logprobs: true, top_logprobs: 5, thinking: {type:
//! disabled}` answered `content: ""`, `reasoning_content: "We"`, no logprobs,
//! through the daemon, and `content: "B"` with its top logprobs sent straight
//! to DeepSeek. The wire type dropped all three fields where it parsed them.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use oicp_client::RemoteApiProvider;
use oicp_types::openai_types::ChatCompletionRequest;
use serde_json::{json, Value};
use sovereign_contracts::local_inference::LocalInferenceService;
use sovereign_contracts::traits::{InferenceProvider, ServingLocus};
use sovereign_contracts::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};
use sovereign_serving_host::inference_adapter::SovereignInferenceAdapter;
use sovereign_serving_host::slot_manifest::CoreSlotManifest;

/// The client body that failed, as the operator sent it.
fn the_failing_body() -> ChatCompletionRequest {
    serde_json::from_value(json!({
        "model": "commonwealth/primary",
        "messages": [{"role": "user", "content": "Answer with one letter, A or B: which comes second?"}],
        "max_tokens": 1,
        "logprobs": true,
        "top_logprobs": 5,
        "thinking": {"type": "disabled"},
    }))
    .expect("request shape")
}

/// A vendor as DeepSeek answered: with the letter and its top logprobs only
/// when told `thinking: disabled`; otherwise it reasons, spends the one token
/// on it, and reports no logprobs. It keeps the last body it was sent.
async fn mock_vendor() -> (String, Arc<Mutex<Option<Value>>>) {
    let seen = Arc::new(Mutex::new(None));
    let kept = seen.clone();
    let chat = move |axum::Json(body): axum::Json<Value>| {
        let thinking_off = body.get("thinking") == Some(&json!({"type": "disabled"}));
        let asked = body.get("logprobs") == Some(&json!(true));
        let top = body
            .get("top_logprobs")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        *kept.lock().unwrap() = Some(body);
        async move {
            let alternatives: Vec<Value> = [
                ("B", 0.9f64),
                ("A", 0.06),
                (" B", 0.02),
                ("The", 0.01),
                ("b", 0.01),
            ]
            .iter()
            .take(top)
            .map(|(t, p)| json!({"token": t, "logprob": p.ln()}))
            .collect();
            let choice = if thinking_off {
                let mut c = json!({"index": 0, "message": {"role": "assistant", "content": "B"}, "finish_reason": "length"});
                if asked {
                    c["logprobs"] = json!({"content": [{"token": "B", "logprob": 0.9f64.ln(), "top_logprobs": alternatives}]});
                }
                c
            } else {
                json!({"index": 0, "message": {"role": "assistant", "content": "", "reasoning_content": "We"}, "logprobs": null, "finish_reason": "length"})
            };
            axum::Json(json!({
                "choices": [choice],
                "usage": {"prompt_tokens": 20, "completion_tokens": 1, "total_tokens": 21},
                "model": "deepseek-flash",
            }))
        }
    };
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(chat));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1"), seen)
}

/// The vendor client behind `[engine] kind = "remote"`, at the locus the
/// engine pair reports for a vendor (`SplitInferenceProvider::engine`). A
/// mock on loopback would be read as this machine, so the locus is stated.
struct Vendor(RemoteApiProvider);

#[async_trait]
impl InferenceProvider for Vendor {
    async fn complete(
        &self,
        req: &CompletionRequest,
    ) -> sovereign_contracts::Result<CompletionResponse> {
        self.0.complete(req).await
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> sovereign_contracts::Result<
        Pin<Box<dyn Stream<Item = sovereign_contracts::Result<String>> + Send>>,
    > {
        unimplemented!("not on this path")
    }

    async fn embed(&self, _text: &str) -> sovereign_contracts::Result<Vec<f32>> {
        unimplemented!("not on this path")
    }

    fn serving_locus(&self) -> ServingLocus {
        ServingLocus::ForwardsToThirdParty
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 65_536,
            supports_structured_output: false,
            relative_speed: Speed::Slow,
            relative_reasoning: Depth::Deep,
        }
    }
}

/// The failing body, through the adapter, to a third-party engine: the
/// vendor is told thinking off in every spelling, asked for five logprobs,
/// and the client gets the letter with them.
#[tokio::test]
async fn a_third_party_engine_is_told_thinking_off_and_its_logprobs_come_back() {
    let (url, seen) = mock_vendor().await;
    let vendor = RemoteApiProvider::third_party(&url, None, "deepseek-flash", 65_536).unwrap();
    let adapter =
        SovereignInferenceAdapter::new(Arc::new(Vendor(vendor)), Arc::new(CoreSlotManifest));

    let reply = adapter
        .chat_completion(the_failing_body())
        .await
        .expect("the vendor answers");

    let sent = seen.lock().unwrap().clone().expect("the vendor was asked");
    assert_eq!(sent["thinking"], json!({"type": "disabled"}), "{sent}");
    assert_eq!(
        sent["chat_template_kwargs"],
        json!({"enable_thinking": false}),
        "{sent}"
    );
    assert_eq!(sent["think_budget"], json!(0), "{sent}");
    assert_eq!(sent["logprobs"], json!(true), "{sent}");
    assert_eq!(sent["top_logprobs"], json!(5), "{sent}");
    let choice = &reply.choices[0];
    assert_eq!(choice.message.content, "B");
    let top = choice
        .logprobs
        .as_ref()
        .and_then(|l| l.pointer("/content/0/top_logprobs"))
        .and_then(Value::as_array)
        .expect("the vendor's logprobs come back");
    assert_eq!(top.len(), 5);
    assert_eq!(top[0]["token"], "B");
}

/// What a local engine is handed for the same body: no budget and no
/// template switch from `thinking`, which llama-server ignores too. The ask
/// for logprobs is carried; an engine that reports none answers without.
#[tokio::test]
async fn a_local_engine_reads_no_thinking_from_a_vendor_field() {
    struct Captures(Mutex<Option<CompletionRequest>>);
    #[async_trait]
    impl InferenceProvider for Captures {
        async fn complete(
            &self,
            req: &CompletionRequest,
        ) -> sovereign_contracts::Result<CompletionResponse> {
            *self.0.lock().unwrap() = Some(req.clone());
            Ok(CompletionResponse {
                text: "B".into(),
                tokens_used: 21,
                prompt_tokens: 20,
                model_id: "local".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: Some(1),
                logprobs: None,
            })
        }
        async fn complete_stream(
            &self,
            _req: &CompletionRequest,
        ) -> sovereign_contracts::Result<
            Pin<Box<dyn Stream<Item = sovereign_contracts::Result<String>> + Send>>,
        > {
            unimplemented!("not on this path")
        }
        async fn embed(&self, _text: &str) -> sovereign_contracts::Result<Vec<f32>> {
            unimplemented!("not on this path")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 32_768,
                supports_structured_output: false,
                relative_speed: Speed::Slow,
                relative_reasoning: Depth::Moderate,
            }
        }
    }
    let local = Arc::new(Captures(Mutex::new(None)));
    let adapter = SovereignInferenceAdapter::new(local.clone(), Arc::new(CoreSlotManifest));

    let reply = adapter
        .chat_completion(the_failing_body())
        .await
        .expect("served");

    let req = local
        .0
        .lock()
        .unwrap()
        .clone()
        .expect("the engine was asked");
    assert_eq!(req.think_budget, None);
    assert_eq!(req.enable_thinking, None);
    assert_eq!(req.logprobs, Some(5));
    assert!(reply.choices[0].logprobs.is_none());
}
