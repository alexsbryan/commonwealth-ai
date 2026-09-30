// SPDX-License-Identifier: AGPL-3.0-or-later

/// Without `llama_cpp` in the allowlist, the log route serve installs
/// before loading delivers nothing and a failed load is a bare error. It
/// must be listed at BOTH postures, and reach `debug` when the operator
/// asks for verbose ggml output (moved with the knob from svrn's filter,
/// pb-serve-distributes).
#[test]
fn serve_filter_carries_llama_cpp_target() {
    let quiet = super::tracing_filter_for(false);
    let verbose = super::tracing_filter_for(true);
    assert!(
        quiet.contains("llama_cpp=info"),
        "llama_cpp must be allowlisted even when quiet: {quiet}"
    );
    assert!(
        verbose.contains("llama_cpp=debug"),
        "GGML_RPC_DEBUG / SOVEREIGN_LLAMA_LOGS=1 must lift llama_cpp to debug: {verbose}"
    );
    tracing_subscriber::EnvFilter::builder()
        .parse(&quiet)
        .expect("serve's default filter must parse");
    let rendered = tracing_subscriber::EnvFilter::builder()
        .parse(&verbose)
        .expect("verbose serve filter must parse")
        .to_string();
    assert!(
        rendered.contains("llama_cpp=debug"),
        "llama_cpp=debug did not survive EnvFilter parsing: {rendered}"
    );
}

use super::*;

#[test]
fn parse_takes_the_data_dir_and_the_listener() {
    let args: Vec<String> = ["--data-dir", "/srv/serve", "--listen", "127.0.0.1:8080"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let parsed = ServeArgs::parse(&args).expect("valid");
    assert_eq!(parsed.data_dir, PathBuf::from("/srv/serve"));
    assert_eq!(parsed.listen, "127.0.0.1:8080".parse().unwrap());
}

#[test]
fn with_no_listener_named_it_listens_where_svrn_dials() {
    let parsed = ServeArgs::parse(&[]).expect("valid");
    assert_eq!(parsed.listen, "127.0.0.1:9748".parse().unwrap());
}

/// A provider with a FIM-capable edit slot that records the request it
/// was asked to decode.
struct EditSlotStub {
    seen: std::sync::Mutex<Option<sovereign_contracts::CompletionRequest>>,
}

#[async_trait::async_trait]
impl InferenceProvider for EditSlotStub {
    async fn complete(
        &self,
        _r: &sovereign_contracts::CompletionRequest,
    ) -> sovereign_contracts::Result<sovereign_contracts::CompletionResponse> {
        unimplemented!("stream-only stub")
    }
    async fn complete_stream(
        &self,
        _r: &sovereign_contracts::CompletionRequest,
    ) -> sovereign_contracts::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = sovereign_contracts::Result<String>> + Send>>,
    > {
        unimplemented!("with_finish-only stub")
    }
    async fn complete_stream_with_finish(
        &self,
        request: &sovereign_contracts::CompletionRequest,
    ) -> sovereign_contracts::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = sovereign_contracts::StreamFrame> + Send>>,
    > {
        *self.seen.lock().unwrap() = Some(request.clone());
        Ok(Box::pin(futures::stream::iter(vec![
            sovereign_contracts::StreamFrame::Token("return a + b;".to_string()),
            sovereign_contracts::StreamFrame::Finish {
                reason: sovereign_contracts::FinishReason::Stop,
                usage: None,
            },
        ])))
    }
    async fn embed(&self, _t: &str) -> sovereign_contracts::Result<Vec<f32>> {
        unimplemented!()
    }
    fn capabilities(&self) -> sovereign_contracts::ProviderCapabilities {
        sovereign_contracts::ProviderCapabilities {
            max_context_tokens: 4096,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: sovereign_contracts::Depth::Shallow,
        }
    }
    fn edit_slot_info(&self) -> Option<sovereign_contracts::EditSlotInfo> {
        Some(sovereign_contracts::EditSlotInfo {
            slot: "edit".into(),
            model_id: "coder".into(),
            aliased_to_fast: false,
            degraded: false,
            next_edit: None,
            fim: Some(sovereign_contracts::FimLane {
                style: sovereign_contracts::FimStyle::QwenCoder,
                max_tokens: 48,
                temperature: 0.2,
                max_prefix_chars: 4096,
                max_suffix_chars: 4096,
            }),
        })
    }
}

async fn serving(provider: Arc<dyn InferenceProvider>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(host_kit::shell::serve(
        [listener],
        bundles(provider),
        std::future::pending(),
    ));
    base
}

#[tokio::test]
async fn serve_decodes_the_next_edit_lanes_raw_prompt_verbatim() {
    let stub = Arc::new(EditSlotStub {
        seen: std::sync::Mutex::new(None),
    });
    let base = serving(Arc::clone(&stub) as Arc<dyn InferenceProvider>).await;
    let raw = "<|editable_region_start|>fn add(a, b) {}<|editable_region_end|>";
    let body: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/v1/completions"))
        .json(&serde_json::json!({ "raw_prompt": raw, "max_tokens": 16 }))
        .send()
        .await
        .expect("answered")
        .json()
        .await
        .expect("a text_completion");
    assert_eq!(body["choices"][0]["text"], "return a + b;", "{body}");
    let seen = stub.seen.lock().unwrap().clone().expect("decoded");
    assert_eq!(seen.prompt, raw, "the raw prompt was not decoded verbatim");
    assert_eq!(seen.model_id.as_deref(), Some("coder"));
}

#[tokio::test]
async fn serve_without_an_edit_slot_refuses_fim_by_name() {
    let base = serving(Arc::new(sovereign_compute::mock::MockProvider {
        tokens: 1,
        delay: std::time::Duration::ZERO,
    }))
    .await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/completions"))
        .json(&serde_json::json!({ "prefix": "fn main() {" }))
        .send()
        .await
        .expect("answered");
    assert_eq!(resp.status(), 503);
    assert!(resp
        .text()
        .await
        .unwrap_or_default()
        .contains("fim_unavailable"));
}

#[test]
fn parse_refuses_an_unknown_argument_by_name() {
    let err = ServeArgs::parse(&["--mesh".to_string()]).expect_err("refused");
    assert!(err.contains("--mesh"), "got: {err}");
}

#[test]
fn the_weight_verbs_route_before_the_server_arguments() {
    // Unrouted, `warm-cache` would reach ServeArgs::parse and exit 2.
    for verb in WEIGHT_VERBS {
        assert_eq!(run(&[verb.to_string(), "--help".into()]), 0, "{verb}");
    }
}
