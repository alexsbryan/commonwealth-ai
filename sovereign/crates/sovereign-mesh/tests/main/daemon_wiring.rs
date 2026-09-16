// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-wiring integration test.
//!
//! Reproduces `EmbeddedDaemon::start_daemon`'s wiring against a real
//! `AppState` + ephemeral-port HTTP servers — without actually invoking
//! `start_daemon`, which hardcodes 9741/9742 and cannot be parallelised
//! across tests (see §10.1 deferral).
//!
//! Two invariants are pinned:
//!
//! 1. **The inference provider serves chat.** The provider is a
//!    construction argument now (`ServingSeed::local_inference`), so a
//!    `/v1/chat/completions` request must serve through the local path and
//!    NOT return a 503 `model_not_ready` — that error is the canary for a
//!    regression that drops the provider.
//! 2. **The mesh-mutation hook fires.** A `/internal/gossip` POST
//!    that adds a member must invoke the hook the node was constructed
//!    with (`FabricSeed::mesh_mutation_hook`); the `Arc::get_mut`
//!    silent-no-op that used to be its failure mode is gone, because the
//!    hook no longer arrives through an installer.
//!
//! The stub `InferenceProvider` returns a tiny canned response so
//! the test stays GPU-/model-/network-free per ARCH §12.4.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_api::server::{client_router, internal_router};
use sovereign_api::state::{AppState, LocalInferenceService, ServingSeed};
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;
use sovereign_mesh::slot_manifest::CoreSlotManifest;
use sovereign_meshapp_registry::registry::AppRegistry;

use crate::common;
use crate::common::{member_with_last_seen, spawn_router, TestProvider};

/// Build an `AppState` the same way `EmbeddedDaemon::start_daemon`
/// does — the mutation hook and the inference provider are both
/// constructor arguments now (DC §4.2 "Construction is staged, and
/// parts are total"). Returns the wired `AppState` plus the atomic the
/// hook will increment on every mutation.
fn build_wired_app_state() -> (AppState, Arc<AtomicUsize>) {
    let self_id = NodeId::from_u128(0x1111_1111_1111_1111);
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member_with_last_seen(self_id, "self", 100, "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(42),
        name: "wiring-test".into(),
        invite_key_hash: [7u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    let hook: sovereign_api::state::MeshMutationHook =
        Arc::new(move |_mesh: &Mesh, _self_id: NodeId| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });
    // The mutation hook and the inference provider are construction arguments
    // now (DC §4.2 "Construction is staged"), so the `Arc::get_mut` ordering
    // hazard that could silently drop either is gone.
    //
    // Provider emits "ok" on both complete + complete_stream so the wiring
    // test can hit either route shape.
    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("stub-primary")
            .with_complete_text("ok")
            .with_stream_chunks(vec!["ok".to_string()])
            .with_embed_marker(|_| vec![0.0; 8]),
    );
    let adapter: Arc<dyn LocalInferenceService> = Arc::new(SovereignInferenceAdapter::new(
        provider,
        Arc::new(CoreSlotManifest),
    ));
    let app_state = AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving(
        self_id,
        mesh,
        mesh_store,
        app_registry,
        None,
        None,
        sovereign_api::state::FabricSeed {
            mesh_mutation_hook: Some(hook),
            ..Default::default()
        },
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
    );

    (app_state, counter)
}

async fn spawn_client(state: AppState) -> SocketAddr {
    spawn_router(client_router(state)).await
}

async fn spawn_internal(state: AppState) -> SocketAddr {
    spawn_router(internal_router(state)).await
}

#[tokio::test]
async fn inference_provider_routes_chat_completions_to_adapter() {
    // Pins the canary: if a future refactor drops the provider from the
    // construction seed in start_daemon, this test fails because the request
    // falls through to the forward_to_model path and 503s with
    // `model_not_ready`.
    let (state, _counter) = build_wired_app_state();
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
         a 503 here means start_daemon dropped the provider from the \
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

#[tokio::test]
async fn mesh_mutation_hook_fires_on_gossip_delta() {
    // Pins the second half of the wiring: a real `/internal/gossip` POST that
    // adds a member must invoke the hook the node was constructed with. The
    // hook is a `FabricSeed` argument now, so there is no reorder that could
    // silently drop it.
    let (state, counter) = build_wired_app_state();
    let addr = spawn_internal(state.clone()).await;

    // Build a wire-shaped snapshot the receiver doesn't yet have.
    // `MeshWire` flattens `members` to a Vec for transport (the live
    // `Mesh` struct uses a HashMap keyed by NodeId, which can't round-
    // trip through JSON because object keys must be strings). Adding
    // one new member with a distinct NodeId guarantees `added > 0`
    // on merge, which is what makes the hook fire (it skips
    // last_seen-only refreshes).
    let self_id = state.inner.fabric.identity.current();
    let other_id = NodeId::from_u128(0x2222_2222_2222_2222);
    let other_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();

    let invite_key_hash: [u8; 32] = [7u8; 32];
    let payload = json!({
        "mesh": {
            "id": MeshId::from_u128(42),
            "name": "wiring-test",
            "join_key_hash": invite_key_hash.to_vec(),
            "members": [
                member_with_last_seen(self_id, "self", 200, "127.0.0.1:9742".parse().unwrap()),
                member_with_last_seen(other_id, "peer", 200, other_addr),
            ],
            "peers": Vec::<serde_json::Value>::new(),
        }
    });

    assert_eq!(counter.load(Ordering::Relaxed), 0, "no mutations yet");

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/gossip"))
        .json(&payload)
        .send()
        .await
        .expect("/internal/gossip must be reachable");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "gossip POST must succeed (status indicates mesh_id/join_key match)"
    );

    assert_eq!(
        counter.load(Ordering::Relaxed),
        1,
        "mutation hook must fire exactly once for one structural delta; \
         zero here means the construction hook was not wired"
    );
}
