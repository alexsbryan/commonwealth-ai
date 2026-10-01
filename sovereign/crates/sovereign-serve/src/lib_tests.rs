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

/// The mock, recording the admission each turn reached the provider with.
struct AdmissionSeen {
    inner: sovereign_compute::mock::MockProvider,
    seen: std::sync::Mutex<Vec<Option<String>>>,
}

impl AdmissionSeen {
    fn record(&self, request: &sovereign_contracts::CompletionRequest) {
        self.seen
            .lock()
            .unwrap()
            .push(request.admission.as_ref().map(|a| a.id().to_string()));
    }
}

#[async_trait::async_trait]
impl InferenceProvider for AdmissionSeen {
    async fn complete(
        &self,
        r: &sovereign_contracts::CompletionRequest,
    ) -> sovereign_contracts::Result<sovereign_contracts::CompletionResponse> {
        self.record(r);
        self.inner.complete(r).await
    }
    async fn complete_stream(
        &self,
        r: &sovereign_contracts::CompletionRequest,
    ) -> sovereign_contracts::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = sovereign_contracts::Result<String>> + Send>>,
    > {
        self.record(r);
        self.inner.complete_stream(r).await
    }
    async fn embed(&self, t: &str) -> sovereign_contracts::Result<Vec<f32>> {
        self.inner.embed(t).await
    }
    fn capabilities(&self) -> sovereign_contracts::ProviderCapabilities {
        self.inner.capabilities()
    }
}

/// serve's member client (`cwth/client/0`) is reached through cw-rails over
/// loopback, so the connection alone would call a peer "this host". The
/// verified key cw-rails stamps makes it a peer: its admission claim is
/// dropped, while a loopback caller's is kept.
#[tokio::test]
async fn a_turn_cw_rails_forwarded_keeps_no_admission_claim() {
    let stub = Arc::new(AdmissionSeen {
        inner: sovereign_compute::mock::MockProvider {
            tokens: 1,
            delay: std::time::Duration::ZERO,
        },
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let base = serving(Arc::clone(&stub) as Arc<dyn InferenceProvider>).await;
    let client = reqwest::Client::new();
    for via_mesh in [false, true] {
        let mut req = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&serde_json::json!({
                "messages": [{"role": "user", "content": "ping"}],
                "turn_admission": "turn-claimed",
            }));
        if via_mesh {
            req = req.header(kernel_types::member::MESH_PUBKEY_HEADER, "ab".repeat(32));
        }
        let resp = req.send().await.expect("answered");
        assert!(resp.status().is_success(), "refused: {}", resp.status());
    }
    let seen = stub.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![Some("turn-claimed".to_string()), None],
        "a loopback caller keeps its admission; a member cw-rails forwarded does not"
    );
}

/// What cw-rails forwards a member to on `cwth/client/0` is the member
/// client alone: the OpenAI face answers, and the reload, which is guarded
/// only by the caller being on loopback (as cw-rails' forward is), is not
/// mounted there.
#[tokio::test]
async fn the_member_client_mounts_the_openai_face_and_no_reload() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    tokio::spawn(host_kit::shell::serve(
        [listener],
        vec![openai_face(Arc::new(
            sovereign_compute::mock::MockProvider {
                tokens: 1,
                delay: std::time::Duration::ZERO,
            },
        ))],
        std::future::pending(),
    ));
    let client = reqwest::Client::new();
    let models = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .expect("answered");
    assert_eq!(models.status(), 200, "the member client answers /v1/models");
    let reload = client
        .post(format!(
            "{base}{}",
            sovereign_contracts::engine_state::RELOAD_PATH
        ))
        .send()
        .await
        .expect("answered");
    assert_eq!(
        reload.status(),
        404,
        "a member must not reach serve's reload through cw-rails"
    );
}

/// Every route a standalone serve mounts: `assemble`'s, over the mock engine,
/// plus the bundle `rails_mesh::join` adds, built as join builds it.
async fn standalone_routes() -> (Vec<RouteBundle>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = SetupConfig::path_in(dir.path());
    std::fs::write(
        &config_path,
        "[engine]\nkind = \"mock\"\n\n[models]\nprimary = \"/nonexistent/mock.gguf\"\nembed = \"/nonexistent/mock-embed.gguf\"\n",
    )
    .expect("write config");
    let ServeAssembly {
        mut routes,
        servable,
        rails_base,
        ..
    } = assemble(dir.path(), &config_path).await.expect("assembled");
    routes.push(crate::rpc_warm::bundle(
        servable,
        crate::rails_mesh::RailsRoster::new(rails_base),
        Arc::new(crate::MeshRpcShardWarmer::new()),
    ));
    (routes, dir)
}

/// The census (pb-serve-package-guard): every `/internal/*` route a standalone
/// serve mounts refuses a non-loopback caller with the loopback guard's 403,
/// whatever `--listen` names. The paths are read from the mounted bundles, so
/// an internal route added without `host_kit::shell::guard::loopback_only` is
/// named here the moment it is mounted.
///
/// The guard also turns away no legitimate caller: the same router bound on
/// every interface still runs rpc-warm for a loopback caller, as cw-rails'
/// forward is. One test, because `assemble` registers the mock engine and the
/// registry refuses a second registration in one process (`cargo test` runs
/// every test of the lifted closure in one process).
#[tokio::test]
async fn every_internal_route_serve_mounts_refuses_a_non_loopback_caller() {
    use tower::ServiceExt;
    let (routes, _dir) = standalone_routes().await;
    let internal: Vec<String> = routes
        .iter()
        .flat_map(|b| b.routes())
        .filter(|p| p.starts_with("/internal/"))
        .cloned()
        .collect();
    for expected in [
        crate::rails_mesh::RPC_WARM_PATH,
        crate::guest_route::GUEST_ROUTE_PATH,
    ] {
        assert!(
            internal.iter().any(|p| p == expected),
            "the census does not see {expected}: {internal:?}"
        );
    }
    let app = host_kit::shell::mount(routes);
    let stranger: SocketAddr = "203.0.113.7:40000".parse().unwrap();
    let mut unguarded = Vec::new();
    for path in &internal {
        let uri = path
            .split('/')
            .map(|s| if s.starts_with('{') { "x" } else { s })
            .collect::<Vec<_>>()
            .join("/");
        for method in [axum::http::Method::GET, axum::http::Method::POST] {
            let mut request = axum::http::Request::builder()
                .method(method.clone())
                .uri(&uri)
                .body(axum::body::Body::empty())
                .expect("request");
            request
                .extensions_mut()
                .insert(axum::extract::ConnectInfo(stranger));
            let response = app.clone().oneshot(request).await.expect("infallible");
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap_or_default();
            let body = String::from_utf8_lossy(&body);
            if status != axum::http::StatusCode::FORBIDDEN || !body.contains("local-only") {
                unguarded.push(format!("{method} {path} -> {status} {body}"));
            }
        }
    }
    assert!(
        unguarded.is_empty(),
        "internal routes answer a non-loopback caller without \
         host_kit::shell::guard::loopback_only: {unguarded:?}"
    );

    // `{}` is refused by the handler, past the guard, as a malformed body.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, service).await });
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}{}",
            crate::rails_mesh::RPC_WARM_PATH
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("answered");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(
        status != 403 && body.contains("malformed rpc-warm body"),
        "a loopback caller did not reach the rpc-warm handler: {status} {body}"
    );
}
