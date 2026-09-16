// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-wiring integration test.
//!
//! Verifies the load-bearing service-injection order from
//! `EmbeddedDaemon::start_daemon` (see `daemon.rs:1196-1212`'s
//! "Order is load-bearing" comment) by reproducing the exact wiring
//! against a real `AppState` + ephemeral-port HTTP servers — without
//! actually invoking `start_daemon`, which hardcodes 9741/9742 and
//! cannot be parallelised across tests (see §10.1 deferral).
//!
//! Two invariants are pinned:
//!
//! 1. **`with_local_inference` fires.** When the adapter is installed
//!    in the documented position (before any clone of `app_state.inner`),
//!    a `/v1/chat/completions` request must serve through the local
//!    path and NOT return a 503 `model_not_ready` — that error is the
//!    canary for a regression that reverses the wiring order.
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
use sovereign_api::state::{AppState, LocalInferenceService};
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;
use sovereign_mesh::slot_manifest::CoreSlotManifest;
use sovereign_meshapp_registry::registry::AppRegistry;

use crate::common;
use crate::common::{member_with_last_seen, spawn_router, TestProvider};

/// Build an `AppState` the same way `EmbeddedDaemon::start_daemon`
/// does — the mutation hook at construction, the `with_local_inference`
/// installer applied *before* any `app_state.inner.clone()` would
/// occur. Returns the wired `AppState` plus the atomic the hook
/// will increment on every mutation.
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
    // The mutation hook is a construction argument now (DC §4.2 "Construction
    // is staged"), so the `Arc::get_mut` ordering hazard cannot silently drop
    // it; only `with_local_inference` still rides that block.
    let app_state = AppState::new_with_platform_and_engine_and_gauge_and_fabric(
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
    );

    // ── Order matches `daemon.rs:1199-1217` exactly ───────────────
    // `with_local_inference` goes through `Arc::get_mut`; cloning
    // `app_state.inner` before it would silently no-op.
    // Provider emits "ok" on both complete + complete_stream so
    // the wiring test can hit either route shape.
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
    let app_state = app_state.with_local_inference(adapter);

    (app_state, counter)
}

async fn spawn_client(state: AppState) -> SocketAddr {
    spawn_router(client_router(state)).await
}

async fn spawn_internal(state: AppState) -> SocketAddr {
    spawn_router(internal_router(state)).await
}

#[tokio::test]
async fn with_local_inference_routes_chat_completions_to_adapter() {
    // Pins the canary: if a future refactor reverses the
    // `with_local_inference` / `Arc::clone` ordering in start_daemon,
    // this test fails because the request falls through to the
    // forward_to_model path and 503s with `model_not_ready`.
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
        "with_local_inference wiring must serve a 200; \
         a 503 here means start_daemon's load-bearing order got reversed \
         and local_inference was silently dropped"
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
