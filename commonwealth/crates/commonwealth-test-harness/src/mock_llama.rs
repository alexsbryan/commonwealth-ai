// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

/// A mock llama-server that responds with canned completions.
/// Tracks request count for verification.
pub struct MockLlamaServer {
    addr: SocketAddr,
    request_count: Arc<AtomicU64>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockLlamaServer {
    /// Start a mock llama-server on a random port.
    /// Returns the server with its address.
    pub async fn start() -> Self {
        let request_count = Arc::new(AtomicU64::new(0));
        let count_clone = Arc::clone(&request_count);

        let app = Router::new()
            .route("/health", get(|| async { StatusCode::OK }))
            .route(
                "/v1/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let count = Arc::clone(&count_clone);
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);

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
