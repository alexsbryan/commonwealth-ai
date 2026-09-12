// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

/// Tunable faults for [`MockLlamaServer`], flippable mid-scenario via the
/// shared handle. Lets a test express a slow / saturated peer
/// (`per_request_delay`, which holds requests in-flight) or a peer that
/// rejects everything (`fail_all`, so routing falls away from it).
#[derive(Clone, Debug, Default)]
pub struct LlamaKnobs {
    /// Sleep before responding — models a slow or saturated peer. Holds the
    /// request in-flight, which is how a test drives an admission ceiling.
    pub per_request_delay: Duration,
    /// When true, every request gets `503` instead of a completion.
    pub fail_all: bool,
}

/// Shared, mutable knob handle. Read per request.
pub type SharedKnobs = Arc<RwLock<LlamaKnobs>>;

/// A mock llama-server that responds with canned completions.
/// Tracks request count for verification.
pub struct MockLlamaServer {
    addr: SocketAddr,
    request_count: Arc<AtomicU64>,
    knobs: SharedKnobs,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockLlamaServer {
    /// Start a mock llama-server on a random port with default (no-fault) knobs.
    pub async fn start() -> Self {
        Self::start_with_knobs(Arc::new(RwLock::new(LlamaKnobs::default()))).await
    }

    /// Start a mock llama-server whose behaviour is driven by `knobs`. Keep a
    /// clone of the handle (or use [`MockLlamaServer::knobs`]) to flip faults
    /// mid-scenario.
    pub async fn start_with_knobs(knobs: SharedKnobs) -> Self {
        let request_count = Arc::new(AtomicU64::new(0));
        let count_clone = Arc::clone(&request_count);
        let knobs_clone = Arc::clone(&knobs);

        let app = Router::new()
            .route("/health", get(|| async { StatusCode::OK }))
            .route(
                "/v1/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let count = Arc::clone(&count_clone);
                    let knobs = Arc::clone(&knobs_clone);
                    async move {
                        let k = knobs
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        if !k.per_request_delay.is_zero() {
                            tokio::time::sleep(k.per_request_delay).await;
                        }
                        count.fetch_add(1, Ordering::SeqCst);
                        if k.fail_all {
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                "mock llama-server: injected failure",
                            )
                                .into_response();
                        }

                        let model = body
                            .get("model")
                            .and_then(|m| m.as_str())
                            .unwrap_or("mock-model");

                        Json(json!({
                            "id": format!("chatcmpl-mock-{}", count.load(Ordering::SeqCst)),
                            "object": "chat.completion",
                            "created": 1700000000u64,
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {
                                    "role": "assistant",
                                    "content": "This is a mock response from the simulated llama-server."
                                },
                                "finish_reason": "stop"
                            }],
                            "usage": {
                                "prompt_tokens": 10,
                                "completion_tokens": 15,
                                "total_tokens": 25
                            }
                        }))
                        .into_response()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tokio::select! {
                result = axum::serve(listener, app) => {
                    if let Err(e) = result {
                        tracing::warn!("mock llama-server error: {e}");
                    }
                }
                _ = shutdown_rx => {}
            }
        });

        Self {
            addr,
            request_count,
            knobs,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// Get the address of the mock server.
    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    /// Get the address as a string (host:port).
    pub fn address_string(&self) -> String {
        self.addr.to_string()
    }

    /// Get the number of requests received.
    pub fn request_count(&self) -> u64 {
        self.request_count.load(Ordering::SeqCst)
    }

    /// Shared knob handle — mutate to change behaviour mid-scenario.
    pub fn knobs(&self) -> SharedKnobs {
        Arc::clone(&self.knobs)
    }

    /// Shutdown the mock server.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for MockLlamaServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
