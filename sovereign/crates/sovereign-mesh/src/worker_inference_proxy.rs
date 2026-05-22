//! Pod-side TLS-pinned inference proxy.
//!
//! Spec: `sovereign/docs/PINNED_WORKER_AS_INFERENCE_PEER.md`.
//!
//! ## What it does
//!
//! When the worker daemon is configured with an [`InferenceProxyConfig`],
//! these routes are mounted alongside the four existing
//! `/internal/worker/*` routes:
//!
//! - `POST /v1/chat/completions` — streaming + non-streaming
//! - `POST /v1/embeddings`
//! - `GET  /v1/models`
//! - `GET  /oicp/v1/capabilities`
//!
//! Each call is forwarded to the child daemon's local
//! `http://127.0.0.1:9741` plain-HTTP listener and the response is
//! streamed back over the worker daemon's `:9742` TLS-pinned channel.
//! The Ed25519 bearer middleware on the worker router already gates
//! these — same auth path as `/internal/worker/upload`, no second
//! authentication layer.
//!
//! ## Streaming preservation
//!
//! `reqwest::Response::bytes_stream()` + `axum::body::Body::from_stream`
//! compose without buffering, so a 200-token SSE completion arrives at
//! the owner token-by-token, not as a single buffered blob at the end.
//! If the proxy ever stops streaming, UIs that drive off SSE go from
//! "smooth typing" to "10-second pause then giant dump" — the
//! single-buffer failure mode the spec calls out as the most likely
//! regression.
//!
//! ## Readiness gate
//!
//! Until the child daemon's first `/v1/models` probe succeeds, every
//! proxy call returns `503 Service Unavailable` with a one-line body
//! explaining why. Without this gate the owner would see ECONNREFUSED
//! during the ~90s model warmup — opaque and easy to misdiagnose as a
//! networking problem.
//!
//! ## What this file does NOT do
//!
//! - **No request rewriting.** The body and method pass through
//!   verbatim. The child daemon's HTTP surface is the authoritative
//!   chat-completions API; the proxy is a thin tunnel.
//! - **No second-layer auth.** The worker daemon's
//!   `require_worker_token` layer is sufficient — only the owner can
//!   reach these endpoints, and the child daemon trusts its parent.
//! - **No retry logic.** A 5xx from the child propagates to the owner
//!   unchanged; the mesh scheduler's fan-out retry is what makes
//!   pinned-pod failures non-fatal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header::HeaderName, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use reqwest::Method as ReqMethod;
use tokio_stream::StreamExt;

use crate::worker_http::WorkerState;

/// Headers we never copy from owner → child. Hop-by-hop and
/// auth-shaped headers; the child daemon doesn't authenticate inbound
/// traffic on its localhost port (everything is parent-trusted), so
/// forwarding `Authorization` is both pointless and slightly leaky if
/// the child ever decides to log headers.
const STRIPPED_REQUEST_HEADERS: &[&str] = &[
    "host",
    "authorization",
    "connection",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Headers we never copy from child → owner. Hop-by-hop only; the
/// auth-shaped ones are not produced by the child in practice but
/// excluding them is cheap insurance.
const STRIPPED_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "content-length",
    "transfer-encoding",
    "upgrade",
];

/// Configures the pod-side inference proxy. Carried on `WorkerState`
/// as `Option<Arc<…>>` — `None` means proxy disabled, the four
/// routes return 503 with a "not configured" hint. Production CLI
/// wiring sets this when the pod is launched with
/// `SOVEREIGN_WORKER_RUNNER=subprocess`; tests skip it entirely.
pub struct InferenceProxyConfig {
    /// Where the child daemon is listening. Typically
    /// `http://127.0.0.1:9741`. Stored without a trailing slash so
    /// path joins are unambiguous.
    pub child_base_url: String,
    /// Shared atomic the [`SubprocessRunner`] flips to `true` once
    /// the child daemon's `/v1/models` probe has returned 200.
    /// Cloned from the runner so both sides observe the same signal
    /// without polling state through a back channel.
    pub child_ready: Arc<AtomicBool>,
    /// reqwest client used for forwarding. A fresh client per request
    /// would work but a shared one lets connection-pooling kick in
    /// across an enrichment burst.
    pub client: reqwest::Client,
}

impl std::fmt::Debug for InferenceProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceProxyConfig")
            .field("child_base_url", &self.child_base_url)
            .field("child_ready", &self.child_ready.load(Ordering::Acquire))
            .finish()
    }
}

impl InferenceProxyConfig {
    /// Convenience constructor that builds a localhost-only reqwest
    /// client with sensible defaults (no proxy, modest connect
    /// timeout, generous overall request timeout to match
    /// long-running enrichment calls).
    pub fn for_local_child(child_base_url: impl Into<String>, child_ready: Arc<AtomicBool>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(1800))
            .no_proxy()
            .build()
            .expect("reqwest builds with localhost-only config");
        Self {
            child_base_url: child_base_url
                .into()
                .trim_end_matches('/')
                .to_string(),
            child_ready,
            client,
        }
    }
}

/// Mount the four proxy routes on a `Router<Arc<WorkerState>>`. The
/// caller is responsible for layering the same auth middleware
/// already in use by `worker_router` — we don't add it here so the
/// merge inside `worker_router` doesn't double-layer.
pub fn inference_proxy_routes() -> Router<Arc<WorkerState>> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions_proxy))
        .route("/v1/models", get(models_proxy))
        .route("/v1/embeddings", post(embeddings_proxy))
        .route("/oicp/v1/capabilities", get(oicp_capabilities_proxy))
}

async fn chat_completions_proxy(
    State(state): State<Arc<WorkerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(state, Method::POST, "/v1/chat/completions", headers, Some(body)).await
}

async fn embeddings_proxy(
    State(state): State<Arc<WorkerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(state, Method::POST, "/v1/embeddings", headers, Some(body)).await
}

async fn models_proxy(
    State(state): State<Arc<WorkerState>>,
    headers: HeaderMap,
) -> Response {
    forward(state, Method::GET, "/v1/models", headers, None).await
}

async fn oicp_capabilities_proxy(
    State(state): State<Arc<WorkerState>>,
    headers: HeaderMap,
) -> Response {
    forward(state, Method::GET, "/oicp/v1/capabilities", headers, None).await
}

async fn forward(
    state: Arc<WorkerState>,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Option<Bytes>,
) -> Response {
    // Entry log + start-time capture. A request that takes 4 min to
    // complete and a request that's hung for 4 min look identical from
    // the owner side; pairing this with the exit log below lets an
    // operator distinguish them with a single `vastai logs | grep
    // proxy:`. The 2026-05-16 instrumentation audit called this out as
    // a blind spot worth closing before SEP-on-Vast.
    let started = std::time::Instant::now();
    let body_bytes = body.as_ref().map(|b| b.len()).unwrap_or(0);
    tracing::info!(
        method = %method,
        path,
        request_bytes = body_bytes,
        "proxy: forwarding to child"
    );

    let Some(proxy) = state.inference_proxy.clone() else {
        tracing::warn!(
            path,
            "proxy: 503 — inference proxy not configured on this pod"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "inference proxy not configured on this pod",
        )
            .into_response();
    };

    if !proxy.child_ready.load(Ordering::Acquire) {
        // Owner-side: same shape as a transient peer failure, so the
        // scheduler's fan-out retry policy treats it as "try later".
        // `Retry-After` is a hint to clients that aren't part of the
        // mesh scheduler (e.g. a curl session during pod warmup).
        tracing::info!(
            path,
            duration_ms = started.elapsed().as_millis() as u64,
            "proxy: 503 — child not ready (model still warming up)"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", "10")],
            "child daemon not ready (model warming up)",
        )
            .into_response();
    }

    let url = format!("{}{}", proxy.child_base_url, path);
    let req_method = match ReqMethod::from_bytes(method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid method: {e}"),
            )
                .into_response();
        }
    };
    let mut req = proxy.client.request(req_method, &url);
    for (name, value) in headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if STRIPPED_REQUEST_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        // reqwest accepts http::HeaderName/HeaderValue directly via
        // the underlying conversion — pass through verbatim.
        req = req.header(name.clone(), value.clone());
    }
    if let Some(b) = body {
        req = req.body(b);
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path,
                duration_ms = started.elapsed().as_millis() as u64,
                "proxy: 502 — child daemon request failed (likely SEGV / port closed)"
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("upstream child daemon unreachable: {e}"),
            )
                .into_response();
        }
    };

    let status = match StatusCode::from_u16(response.status().as_u16()) {
        Ok(s) => s,
        Err(_) => StatusCode::BAD_GATEWAY,
    };
    tracing::info!(
        path,
        status = status.as_u16(),
        ttfb_ms = started.elapsed().as_millis() as u64,
        "proxy: headers received from child — streaming body"
    );

    let mut owner_headers = HeaderMap::new();
    for (name, value) in response.headers().iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if STRIPPED_RESPONSE_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            owner_headers.insert(n, v);
        }
    }

    // Stream the body. `bytes_stream` yields `Result<Bytes, reqwest::Error>`;
    // axum's `Body::from_stream` wants `Result<Bytes, BoxError>`.
    let stream = response.bytes_stream().map(|chunk| {
        chunk.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    });
    let body = Body::from_stream(stream);

    let mut resp = Response::builder().status(status);
    if let Some(h) = resp.headers_mut() {
        h.extend(owner_headers);
    }
    resp.body(body)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "worker_inference_proxy: response build failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "response build failed",
            )
                .into_response()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_http::{
        worker_router, EmitCompletedFn, JobManifest, WorkerRunner,
    };
    use crate::worker_pod::{mint_bootstrap, BootstrapInputs};
    use axum::body::Body as AxumBody;
    use axum::http::{header::AUTHORIZATION, Request};
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tower::ServiceExt;

    fn fixed_owner_key() -> SigningKey {
        SigningKey::from_bytes(&[71u8; 32])
    }

    struct InertRunner;
    impl WorkerRunner for InertRunner {
        fn dispatch(&self, _: JobManifest, _: EmitCompletedFn) {}
    }

    /// Run a tiny axum server on 127.0.0.1 that mimics the child
    /// daemon's `/v1/*` and `/oicp/v1/capabilities` surface. Returns
    /// the bound address and a shutdown sender.
    async fn spawn_fake_child() -> (SocketAddr, oneshot::Sender<()>) {
        async fn models() -> &'static str {
            r#"{"data":[{"id":"qwen3.5-2b","object":"model"}]}"#
        }
        async fn chat() -> &'static str {
            r#"{"choices":[{"message":{"content":"hi"}}]}"#
        }
        async fn capabilities() -> &'static str {
            r#"{"version":"0.3","provider":{},"models":[]}"#
        }

        let app = Router::new()
            .route("/v1/models", get(models))
            .route("/v1/chat/completions", post(chat))
            .route("/oicp/v1/capabilities", get(capabilities));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .ok();
        });
        (addr, tx)
    }

    async fn build_state_with_proxy(
        ready: bool,
        seed: u8,
    ) -> (
        Arc<WorkerState>,
        String,
        oneshot::Sender<()>,
        Arc<AtomicBool>,
    ) {
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: "proxy-test".into(),
            owner_signing: &fixed_owner_key(),
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 600,
            seed_override: Some([seed; 32]),
        })
        .unwrap();
        let token = blob.worker_token.clone();
        let (addr, shutdown) = spawn_fake_child().await;
        let child_ready = Arc::new(AtomicBool::new(ready));
        let proxy = Arc::new(InferenceProxyConfig::for_local_child(
            format!("http://{addr}"),
            child_ready.clone(),
        ));
        let mut state = WorkerState::from_blob(blob, Arc::new(InertRunner)).unwrap();
        state.inference_proxy = Some(proxy);
        (Arc::new(state), token, shutdown, child_ready)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_returns_503_when_child_not_ready() {
        let (state, token, _shutdown, _ready) = build_state_with_proxy(false, 41).await;
        let app = worker_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(AxumBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_forwards_models_to_child() {
        let (state, token, _shutdown, _ready) = build_state_with_proxy(true, 42).await;
        let app = worker_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(AxumBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
        assert!(
            std::str::from_utf8(&body).unwrap().contains("qwen3.5-2b"),
            "proxied body must echo the child's response"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_forwards_chat_completions() {
        let (state, token, _shutdown, _ready) = build_state_with_proxy(true, 43).await;
        let app = worker_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(AxumBody::from(r#"{"model":"x","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("hi"), "got {text:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_serves_oicp_manifest() {
        let (state, token, _shutdown, _ready) = build_state_with_proxy(true, 44).await;
        let app = worker_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/oicp/v1/capabilities")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(AxumBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_rejects_unauthenticated_requests() {
        let (state, _token, _shutdown, _ready) = build_state_with_proxy(true, 45).await;
        let app = worker_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .body(AxumBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn proxy_disabled_when_config_absent() {
        let (blob, _) = mint_bootstrap(BootstrapInputs {
            job_id: "no-proxy".into(),
            owner_signing: &fixed_owner_key(),
            expected_uploads: BTreeMap::new(),
            ttl_seconds: 600,
            seed_override: Some([33u8; 32]),
        })
        .unwrap();
        let token = blob.worker_token.clone();
        let state = WorkerState::from_blob(blob, Arc::new(InertRunner)).unwrap();
        // proxy left as None.
        let state = Arc::new(state);
        let app = worker_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(AxumBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // When no proxy is configured the routes are not mounted —
        // axum 404s an unknown path before auth runs.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
