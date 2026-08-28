// SPDX-License-Identifier: AGPL-3.0-or-later
//! A granted model resolves to the LENDING node, and the turn stays here.
//!
//! Live bar 3.3 (2026-08-28) failed because `svrn chat ask` repointed the
//! whole CLI at the lender, sending the CONVERSATION there — where
//! `/v1/conversations` is in no `Scope` and is not served on the guest
//! listener at all. The fix is that the guest's OWN daemon holds the link and
//! only the completion crosses.
//!
//! These drive a real `MeshInferenceProvider` against a real HTTP lender, and
//! pin the two things that must differ from the peer route. Both are live
//! defects if `provider_for_peer` is copied:
//!
//! 1. the REAL model id on the wire — a placeholder resolves to nobody AND
//!    cannot satisfy the lender's scope check, which matches on the name;
//! 2. NO `X-Node-Id` — a guest is not a node, and stamping one runs the
//!    lender's PEER admission on a non-peer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State as AxumState;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use sovereign_core::guest_link::{save_in, GuestLink};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::CompletionRequest;
use sovereign_mesh::daemon::PeerInferenceEndpoint;
use sovereign_mesh::guest_lender::StoredGuestLink;
use sovereign_mesh::peer_inference::{MeshInferenceProvider, PeerEndpointSource};

mod common;
use common::spawn_router;

const GRANTED: &str = "lender-only-model";
const TOKEN: &str = "aa11bb22cc33dd44ee55ff66aa77bb88cc99dd00ee11ff22aa33bb44cc55dd66";

#[derive(Debug)]
struct NoPeers;

#[async_trait]
impl PeerEndpointSource for NoPeers {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        Vec::new()
    }
}

/// What the lender observed about the request it served.
#[derive(Default)]
struct Seen {
    stamped_node_id: AtomicBool,
    named_the_real_model: AtomicBool,
    presented_the_bearer: AtomicBool,
}

async fn models(
    headers: HeaderMap,
    AxumState(seen): AxumState<Arc<Seen>>,
) -> Json<serde_json::Value> {
    if headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOKEN}"))
    {
        seen.presented_the_bearer.store(true, Ordering::SeqCst);
    }
    Json(serde_json::json!({ "data": [{ "id": GRANTED }] }))
}

async fn completions(
    headers: HeaderMap,
    AxumState(seen): AxumState<Arc<Seen>>,
    body: String,
) -> Json<serde_json::Value> {
    if headers.contains_key("x-node-id") {
        seen.stamped_node_id.store(true, Ordering::SeqCst);
    }
    if body.contains(GRANTED) {
        seen.named_the_real_model.store(true, Ordering::SeqCst);
    }
    Json(serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": GRANTED,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "served by the lender"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    }))
}

/// A lender serving exactly what a guest grant scopes.
async fn spawn_lender(seen: Arc<Seen>) -> String {
    let app = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(completions))
        .with_state(seen);
    format!("http://{}", spawn_router(app).await)
}

/// Local knows nothing, so anything served proves it came from the lender.
fn local_without_the_model() -> Arc<dyn InferenceProvider> {
    Arc::new(common::TestProvider::new().with_model_id("something-else"))
}

fn provider_with_link(root: &std::path::Path) -> MeshInferenceProvider {
    let p = MeshInferenceProvider::with_peer_source(
        local_without_the_model(),
        Arc::new(NoPeers) as Arc<dyn PeerEndpointSource>,
    );
    p.set_guest_source(Arc::new(StoredGuestLink::new(root.to_path_buf())));
    p
}

fn store_link(root: &std::path::Path, url: &str) {
    save_in(
        root,
        &GuestLink {
            token: TOKEN.into(),
            url: url.to_string(),
            dial: None,
            expires_at: sovereign_core::guest_link::now_secs() + 3_600,
            summary: Some(GRANTED.into()),
        },
    )
    .unwrap();
}

/// THE test. A model only the LENDER has is served, through the link.
#[tokio::test]
async fn a_granted_model_is_served_by_the_lender_with_the_real_name_and_no_node_stamp() {
    let seen = Arc::new(Seen::default());
    let lender = spawn_lender(Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    store_link(dir.path(), &lender);

    let resp = provider_with_link(dir.path())
        .complete(&CompletionRequest::new("hello").with_model_id(GRANTED))
        .await
        .expect("the lender serves a granted model");

    assert!(
        resp.text.contains("served by the lender"),
        "got {:?}",
        resp.text
    );
    assert_eq!(resp.model_id, GRANTED, "attribution names what was served");
    assert!(
        seen.presented_the_bearer.load(Ordering::SeqCst),
        "the grant token is the credential and must be presented"
    );
    assert!(
        seen.named_the_real_model.load(Ordering::SeqCst),
        "the REAL model id must be on the wire — a placeholder cannot satisfy \
         the lender's scope check, which matches on the name"
    );
    assert!(
        !seen.stamped_node_id.load(Ordering::SeqCst),
        "a guest is not a node: stamping X-Node-Id runs the lender's PEER \
         admission on a non-peer and mis-attributes it in their tally"
    );
}

/// The listing and the routing must agree. `locate_named_model` routes these
/// ids, so `/v1/models` has to carry them — omitting a model the daemon will
/// happily serve is the same §10.6 lie the peer listing was fixed for, in the
/// other direction. This is the source that feeds it.
#[tokio::test]
async fn the_granted_ids_are_advertised_for_the_models_listing() {
    use sovereign_mesh::guest_lender::GuestLenderSource;

    let seen = Arc::new(Seen::default());
    let lender = spawn_lender(Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    store_link(dir.path(), &lender);

    let src = StoredGuestLink::new(dir.path().to_path_buf());
    let (who, ids) = src
        .granted_models()
        .await
        .expect("a live link advertises what it buys");
    assert_eq!(ids, vec![GRANTED.to_string()]);
    assert_eq!(
        who, lender,
        "the holder is the LENDER, never a peer name — `advertised_by` must \
         not claim a mesh relationship that does not exist"
    );
}

/// No link, nothing advertised. The listing must not grow a phantom row on a
/// node that never accepted one.
#[tokio::test]
async fn a_node_without_a_link_advertises_nothing_extra() {
    use sovereign_mesh::guest_lender::GuestLenderSource;
    let dir = tempfile::tempdir().unwrap();
    assert!(StoredGuestLink::new(dir.path().to_path_buf())
        .granted_models()
        .await
        .is_none());
}

/// With no link stored, the same request resolves to nothing rather than
/// being quietly served by the local slot under a different model.
#[tokio::test]
async fn without_a_link_a_lender_only_model_is_not_substituted_locally() {
    let dir = tempfile::tempdir().unwrap();
    let out = provider_with_link(dir.path())
        .complete(&CompletionRequest::new("hello").with_model_id(GRANTED))
        .await;
    assert!(
        out.is_err(),
        "a model nothing advertises must fail loud, not fall back to \
         this node's own model (§18.3): got {out:?}"
    );
}

/// An expired link is not a route. The window is the guest's own half of the
/// contract, checked before any network call.
#[tokio::test]
async fn an_expired_link_does_not_route() {
    let seen = Arc::new(Seen::default());
    let lender = spawn_lender(Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    save_in(
        dir.path(),
        &GuestLink {
            token: TOKEN.into(),
            url: lender,
            dial: None,
            expires_at: 1,
            summary: None,
        },
    )
    .unwrap();

    assert!(provider_with_link(dir.path())
        .complete(&CompletionRequest::new("hello").with_model_id(GRANTED))
        .await
        .is_err());
    assert!(
        !seen.presented_the_bearer.load(Ordering::SeqCst),
        "an expired link must not even be dialled"
    );
}
