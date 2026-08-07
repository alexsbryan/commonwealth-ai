// SPDX-License-Identifier: AGPL-3.0-or-later
//! Single-child routing over stub children (no real processes):
//! - the facade routes by `model_id` to the child, else to the in-process inner;
//! - a capturing embed child takes `/v1/embeddings` while serving, else inner;
//! - a non-serving child fails fast with `ComputeUnavailable`.
//!
//! (There is no N-replica pool — that path was removed after a live embed run
//! showed process replicas lose to in-process batching.)

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use sovereign_compute::child::{ChildLifecycle, ChildProvider, ChildRuntimeState};
use sovereign_compute::client::ComputeChildClient;
use sovereign_compute::manager::ComputeRoutedProvider;
use sovereign_compute::server::{router, ChildMeta};
use sovereign_contracts::{
    CompletionRequest, CompletionResponse, Depth, Error, InferenceProvider, ProviderCapabilities,
    Result, Speed,
};
use tokio::sync::watch;

/// A stub whose `complete` returns `marker` and `embed` returns `[embed_val]`.
struct StubProvider {
    marker: String,
    embed_val: f32,
}

#[async_trait]
impl InferenceProvider for StubProvider {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: self.marker.clone(),
            tokens_used: 1,
            prompt_tokens: 0,
            model_id: self.marker.clone(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: Some(1),
            ..Default::default()
        })
    }

    async fn complete_stream(
        &self,
        _request: &CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
        let m = self.marker.clone();
        Ok(Box::pin(futures::stream::iter(vec![Ok(m)])))
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![self.embed_val])
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 2048,
            supports_structured_output: true,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

async fn spawn_server(provider: Arc<dyn InferenceProvider>, role: &str) -> u16 {
    let ready = Arc::new(AtomicBool::new(true));
    let app = router(
        provider,
        ready,
        ChildMeta {
            role: role.to_string(),
            model_id: "stub".to_string(),
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    port
}

/// A serving `ChildProvider` pointing at `port`. The `watch::Sender` must be
/// kept alive (a dropped sender reads as "lost serving" to the fail-fast race).
fn serving_child(name: &str, port: u16) -> (Arc<ChildProvider>, watch::Sender<ChildRuntimeState>) {
    let mut st = ChildRuntimeState::starting();
    st.lifecycle = ChildLifecycle::Serving;
    st.port = Some(port);
    st.client = Some(ComputeChildClient::from_port(port).unwrap());
    let (tx, rx) = watch::channel(st);
    (Arc::new(ChildProvider::new(name.to_string(), rx)), tx)
}

#[tokio::test]
async fn facade_routes_by_model_id_else_inner() {
    let inner: Arc<dyn InferenceProvider> = Arc::new(StubProvider {
        marker: "INNER".into(),
        embed_val: 1.0,
    });
    let child_backend: Arc<dyn InferenceProvider> = Arc::new(StubProvider {
        marker: "CHILD".into(),
        embed_val: 2.0,
    });
    let port = spawn_server(child_backend, "generate").await;
    let (child, _tx) = serving_child("gen", port);
    let mut routes = HashMap::new();
    routes.insert("gen".to_string(), child);
    let facade = ComputeRoutedProvider::with_routes(inner, routes, None);

    let mut req = CompletionRequest::default();
    req.model_id = Some("gen".into());
    assert_eq!(facade.complete(&req).await.unwrap().text, "CHILD");

    let req_none = CompletionRequest::default();
    assert_eq!(facade.complete(&req_none).await.unwrap().text, "INNER");

    let mut req_unknown = CompletionRequest::default();
    req_unknown.model_id = Some("does-not-exist".into());
    assert_eq!(facade.complete(&req_unknown).await.unwrap().text, "INNER");
}

#[tokio::test]
async fn capturing_embed_child_takes_embeddings_else_inner() {
    let inner: Arc<dyn InferenceProvider> = Arc::new(StubProvider {
        marker: "INNER".into(),
        embed_val: 1.0,
    });
    let embed_backend: Arc<dyn InferenceProvider> = Arc::new(StubProvider {
        marker: "EMBED".into(),
        embed_val: 2.0,
    });
    let port = spawn_server(embed_backend, "embed").await;
    let (embed_child, tx) = serving_child("embed", port);
    let facade = ComputeRoutedProvider::with_routes(inner, HashMap::new(), Some(embed_child));

    // Serving → routes to the child (embed_val 2.0).
    assert_eq!(facade.embed("hi").await.unwrap(), vec![2.0]);

    // Transition the child to not-serving → falls back to the inner slot (1.0).
    tx.send(ChildRuntimeState::starting()).unwrap();
    assert_eq!(facade.embed("hi").await.unwrap(), vec![1.0]);
    drop(tx);
}

#[tokio::test]
async fn non_serving_child_fails_fast() {
    let (tx, rx) = watch::channel(ChildRuntimeState::starting());
    let child = ChildProvider::new("x".to_string(), rx);
    assert!(!child.is_serving());
    let err = child
        .complete(&CompletionRequest::default())
        .await
        .expect_err("a non-serving child must fail fast");
    assert!(
        matches!(err, Error::ComputeUnavailable { .. }),
        "expected ComputeUnavailable, got {err:?}"
    );
    drop(tx);
}
