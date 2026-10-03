// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-wiring integration test.
//!
//! Reproduces `EmbeddedDaemon::start`'s wiring against a real `AppState` +
//! an ephemeral-port HTTP server — without invoking `start`, which binds the
//! configured ports and cannot be parallelised across tests (see §10.1
//! deferral).
//!
//! **The inference provider serves chat.** The provider is a construction
//! argument (`ServingSeed::local_inference`), so a `/v1/chat/completions`
//! request must serve through the local path and NOT return a 503
//! `model_not_ready` — that error is the canary for a regression that drops
//! the provider. (The mesh-mutation hook this file also pinned left with the
//! daemon's roster and gossip, pb-mesh-exit-transport.)
//!
//! The stub `InferenceProvider` returns a tiny canned response so
//! the test stays GPU-/model-/network-free per ARCH §12.4.
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::json;

use kernel_types::NodeId;
use sovereign_contracts::traits::InferenceProvider;
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::{AppState, LocalInferenceService, NodeSeed, ServingSeed};

use crate::common::ledger_double::RecordingLedger;
use crate::common::service_double::ProviderService;
use crate::common::{spawn_router, TestProvider};

/// Build an `AppState` the way `EmbeddedDaemon::start` does — the inference
/// provider is a constructor argument (DC §4.2 "Construction is staged, and
/// parts are total").
fn build_wired_app_state() -> AppState {
    let self_id = NodeId::from_u128(0x1111_1111_1111_1111);
    // Provider emits "ok" on both complete + complete_stream so the wiring
    // test can hit either route shape.
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("stub-primary")
            .with_complete_text("ok")
            .with_stream_chunks(vec!["ok".to_string()])
            .with_embed_marker(|_| vec![0.0; 8]),
    );
    let adapter: Arc<dyn LocalInferenceService> = ProviderService::new(provider);
    AppState::new_with_seeds(
        self_id,
        None,
        None,
        Default::default(),
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
        NodeSeed::default(),
        Arc::new(RecordingLedger::new(self_id)).seed(),
    )
}

async fn spawn_client(state: AppState) -> SocketAddr {
    spawn_router(client_router(state)).await
}

#[tokio::test]
async fn inference_provider_routes_chat_completions_to_adapter() {
    // Pins the canary: if a future refactor drops the provider from the
    // construction seed in `start`, this test fails because the request
    // falls through to the forward_to_model path and 503s with
    // `model_not_ready`.
    let state = build_wired_app_state();
    let addr = spawn_client(state).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({
            "model": "stub-primary",
            "messages": [
                {"role": "user", "content": "ping"}
            ],
            "stream": false,
        }))
        .send()
        .await
        .expect("/v1/chat/completions must be reachable");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "inference-provider wiring must serve a 200; \
         a 503 here means start dropped the provider from the \
         construction seed and local_inference was absent"
    );

    // Body sanity: we got back the stub provider's output, not a
    // forward_to_model fallthrough payload.
    let body: serde_json::Value = resp.json().await.unwrap();
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    assert!(
        !content.is_empty(),
        "local_inference path must return a non-empty completion body; got: {body}"
    );
}
