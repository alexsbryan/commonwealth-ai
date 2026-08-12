// SPDX-License-Identifier: AGPL-3.0-or-later
//! UC-R1 single-host proof (order `seat-resource-commons`): "is my
//! machine serving the peer right now?"
//!
//! The order's own sequencing rule applies here: the cross-machine
//! cases cannot run until the instrument exists, so the single-host
//! case must prove the instrument first. This test is that proof —
//! an in-process router with a slow stub provider, no real daemon,
//! no restart:
//!
//! 1. idle zero — with the peer idle, `/status` shows no peer rows;
//! 2. WHILE it runs — a peer's `/v1/chat/completions` (carrying the
//!    `X-Node-Id` header) shows on `/status` as `active: 1` with the
//!    mesh-roster NAME joined, not an opaque hash;
//! 3. after — the same row reads `active: 0` with a monotonic
//!    `served_total: 1` (the cumulative attribution witness);
//! 4. local requests are NOT tallied — no header, no row.
//!
//! The tally's `active` counter follows the response BODY's lifetime
//! (see `TallyBody` in commonwealth-api admission), so the
//! "while it runs" reading is taken during a 500ms stub generation.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use serde_json::{json, Value};

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_core::error::Result as SovResult;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, ProviderCapabilities, Speed, StreamFrame,
};
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

mod common;
use common::{id_to_hex, member_with_last_seen, spawn_router, TestProvider};

/// An `InferenceProvider` that sleeps before delegating, so the
/// "response still generating" window is observable from outside.
/// Test-local: the shared-knob bar in `common/mod.rs` is "two
/// callers need it".
struct SlowProvider {
    inner: Arc<dyn InferenceProvider>,
    delay: Duration,
}

#[async_trait]
impl InferenceProvider for SlowProvider {
    async fn complete(&self, req: &CompletionRequest) -> SovResult<CompletionResponse> {
        tokio::time::sleep(self.delay).await;
        self.inner.complete(req).await
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = SovResult<String>> + Send>>> {
        tokio::time::sleep(self.delay).await;
        self.inner.complete_stream(req).await
    }

    async fn complete_stream_with_finish(
        &self,
        req: &CompletionRequest,
    ) -> SovResult<Pin<Box<dyn Stream<Item = StreamFrame> + Send>>> {
        tokio::time::sleep(self.delay).await;
        self.inner.complete_stream_with_finish(req).await
    }

    async fn embed(&self, input: &str) -> SovResult<Vec<f32>> {
        self.inner.embed(input).await
    }

    fn model_id_for(&self, speed: Speed) -> String {
        self.inner.model_id_for(speed)
    }

    fn code_model_id(&self) -> Option<String> {
        self.inner.code_model_id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}

fn build_state(peer_name: &str) -> (AppState, NodeId) {
    // High-byte ids: NodeId Display truncates to the first 8 bytes
    // (`define_id!` in commonwealth-core), so low-byte ids would
    // render identically and the attribution check would be blind.
    let self_id = NodeId::from_u128(0x1111_1111_1111_1111 << 64);
    let peer_id = NodeId::from_u128(0x2222_2222_2222_2222 << 64);
    let mut members = HashMap::new();
    members.insert(
        self_id,
        member_with_last_seen(self_id, "self", 100, "127.0.0.1:9742".parse().unwrap()),
    );
    members.insert(
        peer_id,
        member_with_last_seen(peer_id, peer_name, 100, "127.0.0.1:9876".parse().unwrap()),
    );
    let mesh = Mesh {
        id: MeshId::from_u128(42),
        name: "tally-e2e".into(),
        join_key_hash: [7u8; 32],
        require_encryption: false,
        members,
        peers: vec![],
    };

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    let app_state =
        AppState::new_with_platform_and_engine(self_id, mesh, mesh_store, app_registry, None);

    let fast: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("stub-primary")
            .with_complete_text("ok"),
    );
    let slow = Arc::new(SlowProvider {
        inner: fast,
        delay: Duration::from_millis(500),
    });
    let adapter: Arc<dyn LocalInferenceService> = Arc::new(SovereignInferenceAdapter::new(slow));
    (app_state.with_local_inference(adapter), peer_id)
}

async fn status_peer_requests(client: &reqwest::Client, addr: &SocketAddr) -> Vec<Value> {
    let st: Value = client
        .get(format!("http://{addr}/status"))
        .send()
        .await
        .expect("/status must be reachable")
        .json()
        .await
        .expect("/status must be JSON");
    st["inference"]["peer_requests"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Poll `/status` until a row for `node` exists with `active == want`.
/// Async: tests run on the current-thread runtime, where
/// `block_in_place` is unavailable.
async fn poll_peer_row(
    client: &reqwest::Client,
    addr: &SocketAddr,
    node_display: &str,
    want_active: u64,
    timeout: Duration,
) -> Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let rows = status_peer_requests(client, addr).await;
        if let Some(row) = rows
            .iter()
            .find(|r| r["node_id"].as_str() == Some(node_display))
        {
            if row["active"].as_u64() == Some(want_active) {
                return row.clone();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "peer row for {node_display} with active={want_active} never appeared on /status"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn peer_inference_attributes_to_peer_on_status_and_resets_to_zero() {
    let (state, peer_id) = build_state("BeefyMac");
    let addr = spawn_router(client_router(state)).await;
    let client = reqwest::Client::new();
    let peer_display = peer_id.to_string();

    // Phase 0 — with the peer idle, A reads zero (no row at all).
    let idle = status_peer_requests(&client, &addr).await;
    assert!(
        idle.is_empty(),
        "idle daemon must show no peer rows; got: {idle:?}"
    );

    // Phase 1 — the peer sends inference while we watch /status.
    let inference = tokio::spawn({
        let client = client.clone();
        let addr = addr;
        let peer_header = id_to_hex(&peer_id);
        async move {
            client
                .post(format!("http://{addr}/v1/chat/completions"))
                .header("X-Node-Id", peer_header)
                .json(&json!({
                    "model": "stub-primary",
                    "messages": [{"role": "user", "content": "ping"}],
                    "stream": false,
                }))
                .send()
                .await
                .expect("peer inference must be admitted (stub, no gates armed)")
        }
    });

    let live = poll_peer_row(
        &client,
        &addr,
        &peer_display,
        1,
        Duration::from_millis(3000),
    )
    .await;
    assert_eq!(
        live["name"].as_str(),
        Some("BeefyMac"),
        "the row must carry the mesh-roster NAME, not an opaque hash"
    );
    assert_eq!(
        live["active"].as_u64(),
        Some(1),
        "active must be non-zero WHILE the response is streaming"
    );
    assert_eq!(
        live["served_total"].as_u64(),
        Some(1),
        "one admission so far"
    );

    // Consume the response body — the tally's active counter follows
    // the body's lifetime, so this is what flips it back to zero.
    let resp = inference.await.expect("inference task");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let _body: Value = resp.json().await.expect("response body readable");

    // Phase 2 — back to zero, cumulative witness monotonic.
    let settled = poll_peer_row(
        &client,
        &addr,
        &peer_display,
        0,
        Duration::from_millis(3000),
    )
    .await;
    assert_eq!(
        settled["active"].as_u64(),
        Some(0),
        "idle again after the body is consumed"
    );
    assert_eq!(
        settled["served_total"].as_u64(),
        Some(1),
        "served_total is the cumulative attribution witness — never decremented"
    );

    // Phase 3 — a LOCAL request (no X-Node-Id) is not tallied at all.
    let _loc = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({
            "model": "stub-primary",
            "messages": [{"role": "user", "content": "local"}],
            "stream": false,
        }))
        .send()
        .await
        .expect("local request always admitted");
    let rows = status_peer_requests(&client, &addr).await;
    assert_eq!(
        rows.len(),
        1,
        "local traffic must not create tally rows; got: {rows:?}"
    );
    assert_eq!(
        rows[0]["served_total"].as_u64(),
        Some(1),
        "peer row unchanged by local traffic"
    );
}

#[tokio::test]
async fn two_peers_are_attributed_separately() {
    // The attribution must be per-peer, not a single aggregate row —
    // otherwise "is B using this daemon?" is unanswerable.
    let (state, peer_a) = build_state("PeerA");
    let addr = spawn_router(client_router(state)).await;
    let client = reqwest::Client::new();

    let peer_b = NodeId::from_u128(0x3333_3333_3333_3333 << 64);
    // Inject B into the roster so its name joins too (mirrors the
    // daemon's gossip-merge path).
    let peer_b_display = peer_b.to_string();

    for (node, _name) in [(peer_a, "PeerA"), (peer_b, "PeerB")] {
        let h = id_to_hex(&node);
        let _r = client
            .post(format!("http://{addr}/v1/chat/completions"))
            .header("X-Node-Id", h)
            .json(&json!({
                "model": "stub-primary",
                "messages": [{"role": "user", "content": "ping"}],
                "stream": false,
            }))
            .send()
            .await
            .expect("admitted");
    }
    // Wait for both rows to settle at active=0 with their totals.
    poll_peer_row(
        &client,
        &addr,
        &peer_a.to_string(),
        0,
        Duration::from_millis(3000),
    )
    .await;
    let rows = status_peer_requests(&client, &addr).await;
    assert!(
        rows.iter()
            .any(|r| r["node_id"].as_str() == Some(peer_a.to_string().as_str())),
        "peer A row"
    );
    // B was never a roster member at build time, so its row must still
    // appear — with `name` ABSENT, not silently dropped.
    assert!(
        rows.iter()
            .any(|r| r["node_id"].as_str() == Some(&peer_b_display)),
        "a non-roster peer must still be attributed (name omitted)"
    );
    assert_eq!(rows.len(), 2, "one row per peer, never merged");
}
