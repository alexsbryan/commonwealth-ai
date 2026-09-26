// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compute child's HTTP server: an axum router that exposes an
//! `Arc<dyn InferenceProvider>` over the native wire ([`crate::wire`]).
//!
//! The child (`child_main`) loads its model into a provider, flips the
//! `ready` flag, and serves this router on `127.0.0.1:0`. The daemon's
//! `ChildProvider` (increment 6) is the client.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header::CONTENT_TYPE, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, MethodRouter};
use axum::{Json, Router};
use futures::StreamExt;
use host_kit::shell::RouteBundle;
use sovereign_contracts::oicp::openai_types::ErrorResponse;
use sovereign_contracts::{CompletionRequest, Error, InferenceProvider, StreamFrame};
use sovereign_inference::served_kind::{self, KindRoute, KindServeError, ServedKind};
use tracing::{debug, warn};

use crate::wire::{
    self, EmbedBatchRequest, EmbedBatchResponse, EmbedMode, EmbedRequest, EmbedResponse,
    HealthInfo, WireError, NDJSON_CONTENT_TYPE, ROUTE_COMPLETE, ROUTE_COMPLETE_STREAM, ROUTE_EMBED,
    ROUTE_EMBED_BATCH, ROUTE_HEALTH,
};

/// Static identity of the child, reported by `/health`.
#[derive(Debug, Clone)]
pub struct ChildMeta {
    /// `"generate"` | `"embed"` | `"mock"`.
    pub role: String,
    /// The resident model id (or `""` for mock / pre-load).
    pub model_id: String,
}

/// Axum state: the provider being served + the readiness flag + identity.
#[derive(Clone)]
struct ChildServerState {
    provider: Arc<dyn InferenceProvider>,
    ready: Arc<AtomicBool>,
    meta: ChildMeta,
}

/// Build the child's router. `ready` starts `false` and is flipped `true`
/// by the child once its model is loaded — until then `/health` returns
/// 503 and the supervisor holds it in `Warming`.
pub fn router(
    provider: Arc<dyn InferenceProvider>,
    ready: Arc<AtomicBool>,
    meta: ChildMeta,
) -> Router {
    host_kit::shell::mount(vec![bundle(provider, ready, meta)])
}

/// [`router`]'s routes as the host kit's named bundle, which `child_main`
/// serves through the kit's shell.
pub fn bundle(
    provider: Arc<dyn InferenceProvider>,
    ready: Arc<AtomicBool>,
    meta: ChildMeta,
) -> RouteBundle {
    let state = ChildServerState {
        provider,
        ready,
        meta,
    };
    let bundle = RouteBundle::new("compute_child")
        .route(ROUTE_COMPLETE, post(handle_complete))
        .route(ROUTE_COMPLETE_STREAM, post(handle_complete_stream))
        .route(ROUTE_EMBED, post(handle_embed))
        .route(ROUTE_EMBED_BATCH, post(handle_embed_batch))
        .route(ROUTE_HEALTH, get(handle_health));
    // Each served kind answers on its own route path, from its registration,
    // so a child hosting a kind speaks the same wire as the public route.
    kind_routes(child_provider, wire_refusal)
        .into_iter()
        .fold(bundle, |bundle, (path, handler)| {
            bundle.route(path, handler)
        })
        .with_state(state)
}

/// How a host renders a kind route's refusal on its own wire: the status,
/// the message, and the OpenAI error type.
pub type KindRefusal = fn(StatusCode, String, &'static str) -> Response;

/// Where a host's kind route finds the provider it serves against, or the
/// host's own sentence for why it has none.
pub type KindProvider<S> = fn(&S, &ServedKind) -> Result<Arc<dyn InferenceProvider>, String>;

/// `(path, handler)` for every registered kind with a route: the ONE kind
/// mount, read from the registry (phase-b-16). Every host that serves kinds
/// (the daemon, the compute child, `serve`) mounts through it, so a refusal
/// maps to one status and one event wherever it happens: a bad body is 400,
/// a backend failure is 503 with a warn. Only the envelope is the host's
/// (`refuse`), because the child's client reads a [`WireError`] and a public
/// route answers OpenAI's error shape. A kind whose route is a named absence
/// mounts nothing, and says why at debug.
pub fn kind_routes<S>(
    provider: KindProvider<S>,
    refuse: KindRefusal,
) -> Vec<(&'static str, MethodRouter<S>)>
where
    S: Clone + Send + Sync + 'static,
{
    served_kind::served_kinds()
        .into_iter()
        .filter_map(|kind| match kind.route {
            KindRoute::Served { path, serve } => {
                debug!(target: "served_kind", kind = kind.role, path, "mounting served kind route");
                let handler = post(
                    move |State(st): State<S>, Json(body): Json<serde_json::Value>| async move {
                        let backend = match provider(&st, &kind) {
                            Ok(backend) => backend,
                            Err(why) => {
                                return refuse(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    why,
                                    "no_local_inference_backend",
                                )
                            }
                        };
                        match serve(backend, body).await {
                            Ok(value) => {
                                debug!(target: "served_kind", kind = kind.role, "served kind request answered");
                                Json(value).into_response()
                            }
                            Err(KindServeError::BadRequest(message)) => {
                                refuse(StatusCode::BAD_REQUEST, message, "invalid_request_error")
                            }
                            Err(KindServeError::Backend(message)) => {
                                warn!(target: "served_kind", kind = kind.role, error = %message, "served kind request failed");
                                refuse(StatusCode::SERVICE_UNAVAILABLE, message, "backend_error")
                            }
                        }
                    },
                );
                Some((path, handler))
            }
            KindRoute::Absent { reason } => {
                debug!(target: "served_kind", kind = kind.role, reason, "served kind has no route");
                None
            }
        })
        .collect()
}

/// A kind route's refusal in OpenAI's error shape, for a public route.
pub fn openai_refusal(status: StatusCode, message: String, error_type: &'static str) -> Response {
    (
        status,
        Json(serde_json::to_value(ErrorResponse::new(message, error_type)).unwrap_or_default()),
    )
        .into_response()
}

/// A kind route's refusal on the native wire, which the child's client
/// decodes into a typed [`Error`].
fn wire_refusal(status: StatusCode, message: String, _error_type: &'static str) -> Response {
    let err = if status == StatusCode::BAD_REQUEST {
        Error::InvalidInput(message)
    } else {
        Error::Inference(message)
    };
    (status, Json(WireError::from_error(&err))).into_response()
}

/// The child serves every kind against the one provider it loaded.
fn child_provider(
    st: &ChildServerState,
    _kind: &ServedKind,
) -> Result<Arc<dyn InferenceProvider>, String> {
    Ok(Arc::clone(&st.provider))
}

/// Map a contract [`Error`] to an HTTP status + wire envelope.
fn err_response(err: &Error) -> Response {
    let status = match err {
        Error::InvalidInput(_) => StatusCode::BAD_REQUEST,
        Error::ModelNotLoaded(_) | Error::ComputeUnavailable { .. } => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        Error::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        // 499 = client closed request (nginx convention) — a cancelled
        // generation, distinct from a 500.
        Error::Cancelled => StatusCode::from_u16(499).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(WireError::from_error(err))).into_response()
}

async fn handle_complete(
    State(st): State<ChildServerState>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    match st.provider.complete(&req).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => err_response(&e),
    }
}

async fn handle_complete_stream(
    State(st): State<ChildServerState>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    let stream = match st.provider.complete_stream_with_finish(&req).await {
        Ok(s) => s,
        Err(e) => return err_response(&e),
    };
    // Each frame → one NDJSON line. A frame that somehow fails to encode
    // becomes a terminal Error frame rather than silently truncating.
    let byte_stream = stream.map(|frame| {
        let mut line = wire::encode_frame(&frame).unwrap_or_else(|e| {
            serde_json::to_string(&StreamFrame::Error(format!("frame encode failed: {e}")))
                .unwrap_or_else(|_| "{\"Error\":\"frame encode failed\"}".to_string())
        });
        line.push('\n');
        Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(line))
    });
    (
        [(CONTENT_TYPE, NDJSON_CONTENT_TYPE)],
        Body::from_stream(byte_stream),
    )
        .into_response()
}

async fn handle_embed(
    State(st): State<ChildServerState>,
    Json(req): Json<EmbedRequest>,
) -> Response {
    let result = match req.mode {
        EmbedMode::Document => st.provider.embed(&req.input).await,
        EmbedMode::Query => st.provider.embed_query(&req.input).await,
    };
    match result {
        Ok(embedding) => Json(EmbedResponse { embedding }).into_response(),
        Err(e) => err_response(&e),
    }
}

async fn handle_embed_batch(
    State(st): State<ChildServerState>,
    Json(req): Json<EmbedBatchRequest>,
) -> Response {
    match st.provider.embed_batch(&req.inputs).await {
        Ok(embeddings) => Json(EmbedBatchResponse { embeddings }).into_response(),
        Err(e) => err_response(&e),
    }
}

async fn handle_health(State(st): State<ChildServerState>) -> Response {
    let ready = st.ready.load(Ordering::Relaxed);
    let info = HealthInfo {
        state: if ready { "ready" } else { "loading" }.to_string(),
        role: st.meta.role.clone(),
        model_id: st.meta.model_id.clone(),
    };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(info)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;

    use async_trait::async_trait;
    use futures::Stream;
    use sovereign_contracts::{CompletionResponse, Depth, ProviderCapabilities, Result, Speed};

    /// A reranker whose backend has gone away.
    struct FailingReranker;

    #[async_trait]
    impl InferenceProvider for FailingReranker {
        async fn complete(&self, _: &CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::NotImplemented("rerank only".into()))
        }
        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("rerank only".into()))
        }
        async fn embed(&self, _: &str) -> Result<Vec<f32>> {
            Err(Error::NotImplemented("rerank only".into()))
        }
        async fn rerank_batch(&self, _: &str, _: &[String]) -> Result<Vec<f32>> {
            Err(Error::Inference("the device is gone".into()))
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 0,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    /// The child's kind route goes through the one kind mount: a backend
    /// failure is 503 with a warn under `served_kind`, as on the daemon, and
    /// the body is still the native envelope the child's client decodes.
    /// Before the one mount the child answered 500 and logged nothing.
    #[test]
    fn a_kind_backend_failure_on_the_child_is_503_and_logged() {
        let ((status, body), logs) = crate::logged_under("served_kind=warn", || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async {
                let app = router(
                    Arc::new(FailingReranker),
                    Arc::new(AtomicBool::new(true)),
                    ChildMeta {
                        role: "rerank".into(),
                        model_id: String::new(),
                    },
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind");
                let addr = listener.local_addr().expect("addr");
                tokio::spawn(async move { axum::serve(listener, app).await });
                let resp = reqwest::Client::new()
                    .post(format!("http://{addr}/v1/rerank"))
                    .json(&serde_json::json!({"model": "", "query": "q", "documents": ["d"]}))
                    .send()
                    .await
                    .expect("the child answers");
                let status = resp.status().as_u16();
                (status, resp.json::<WireError>().await.expect("a WireError body"))
            })
        });
        assert_eq!(status, 503, "a backend failure is 503 on every host");
        match body.into_error() {
            Error::Inference(m) => assert!(m.contains("the device is gone"), "got: {m}"),
            other => panic!("expected an Inference error, got {other:?}"),
        }
        assert!(
            logs.contains("served kind request failed"),
            "the failure must reach the log: {logs:?}"
        );
    }
}
