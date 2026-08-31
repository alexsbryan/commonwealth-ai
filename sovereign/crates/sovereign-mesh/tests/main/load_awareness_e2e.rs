// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire-level proof for the gossip-load-awareness fix
//! (`sovereign/docs/MESH_LOAD_AWARENESS.md`).
//!
//! Asserts the three load-bearing properties:
//!
//! 1. `AppState::install_in_flight_publisher` is one-shot — a
//!    second install is silently ignored so the hot-reload path
//!    can't accidentally swap out the Arc that live
//!    `LocalTotalGuard`s reference.
//! 2. `AppState::current_local_in_flight` reads the same atomic
//!    that the MIP-side handle writes to. Bump on the MIP-side
//!    handle, observe through `AppState`.
//! 3. `build_local_capabilities` pulls
//!    `current_local_in_flight` into the gossiped
//!    `NodeCapabilities.current_in_flight` field — and survives a
//!    serde round-trip, which is what an actual peer would see.
//! 4. **Serving an inbound peer request moves that counter.** (1)–(3)
//!    prove the *pipe* — that whatever the atomic holds reaches a
//!    peer's scorer. They say nothing about whether the atomic holds
//!    the node's real load. Property 4 is the one that answers
//!    `SCHEDULER_QUALITY.md` F2's open caveat: the doc asserts the
//!    counter is a **total**, and the finding is that every writer
//!    (`peer_inference.rs::enter_local_total`, four call sites) sits
//!    in the *outbound* joiner path, while an inbound peer request is
//!    served at Priority 0 straight off `AppState::local_inference`
//!    (`routes_inference.rs:171`) with no `MeshInferenceProvider` in
//!    front of it. A node saturated by peer work would then advertise
//!    near-zero load, read as idle to every decider, and win more of
//!    it — priced by `Arm::OutboundOnlyLoad` at +126% mean latency on
//!    `household-evening-12` and +584% on `isolation`.
//!
//! These together prove the founder-side scoring CAN see what a
//! peer is gossiping. The scoring-side override (preferring the
//! gossiped value over the founder's local view) is unit-tested
//! in `peer_inference.rs::tests::gossiped_in_flight_overrides_self_observed`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use commonwealth_api::server::client_router;
use commonwealth_api::state::{AppState, LocalInferenceService};
use commonwealth_app::registry::AppRegistry;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::capabilities::build_local_capabilities;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;

use crate::common;
use crate::common::{member_with_last_seen, spawn_router, TestProvider};

fn empty_mesh() -> Mesh {
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(1),
        name: "test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: std::collections::HashMap::new(),
        peers: vec![],
    }
}

#[tokio::test]
async fn appstate_install_in_flight_publisher_is_one_shot() {
    let state = AppState::new(NodeId::from_u128(1), empty_mesh());
    // Before install, the reader sees None.
    assert_eq!(
        state.current_local_in_flight(),
        None,
        "no publisher installed → current_local_in_flight is None"
    );

    let first = Arc::new(AtomicU32::new(0));
    state.install_in_flight_publisher(Arc::clone(&first));

    // Mutate through `first`; AppState must read the same atomic.
    first.store(3, Ordering::Relaxed);
    assert_eq!(state.current_local_in_flight(), Some(3));

    // A second install must be silently ignored — the contract is
    // "first wins" so live LocalTotalGuards (which captured a clone
    // of `first`) keep writing to the Arc the gossip reader sees.
    let second = Arc::new(AtomicU32::new(99));
    state.install_in_flight_publisher(Arc::clone(&second));
    first.store(7, Ordering::Relaxed);
    assert_eq!(
        state.current_local_in_flight(),
        Some(7),
        "second install must NOT clobber — AppState must still read the first Arc"
    );
}

#[tokio::test]
async fn build_local_capabilities_publishes_in_flight_through_appstate() {
    let state = AppState::new(NodeId::from_u128(2), empty_mesh());
    let publisher = Arc::new(AtomicU32::new(0));
    state.install_in_flight_publisher(Arc::clone(&publisher));

    // Bump the publisher — simulates a `LocalTotalGuard` being
    // alive on the MIP side.
    publisher.store(5, Ordering::Relaxed);

    let caps = build_local_capabilities(
        None, // no CorpusEngine — irrelevant for this assertion
        100,  // reported_at
        1.0,  // inference_availability
        None, // embed_model
        Some(&state),
    )
    .await;

    assert_eq!(
        caps.current_in_flight,
        Some(5),
        "gossip payload must reflect the live MIP-side publisher value"
    );

    // Drain back to zero and rebuild — the next gossip tick must
    // see the drop, not a stale snapshot.
    publisher.store(0, Ordering::Relaxed);
    let caps_after = build_local_capabilities(None, 101, 1.0, None, Some(&state)).await;
    assert_eq!(
        caps_after.current_in_flight,
        Some(0),
        "post-drain gossip must publish 0, not the prior 5"
    );
}

#[tokio::test]
async fn capabilities_payload_survives_serde_roundtrip() {
    // Sanity check on the wire shape: this is the JSON a real
    // peer would parse on receiving our gossip. The test exercises
    // the same code paths a remote founder uses to learn this
    // node's in-flight count.
    let state = AppState::new(NodeId::from_u128(3), empty_mesh());
    let publisher = Arc::new(AtomicU32::new(0));
    state.install_in_flight_publisher(Arc::clone(&publisher));
    publisher.store(11, Ordering::Relaxed);

    let caps = build_local_capabilities(None, 200, 1.0, None, Some(&state)).await;
    let json = serde_json::to_string(&caps).expect("serialize");
    assert!(
        json.contains("\"current_in_flight\":11"),
        "JSON must carry the field: {json}"
    );

    let back: commonwealth_core::capabilities::NodeCapabilities =
        serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.current_in_flight, Some(11));
}

#[tokio::test]
async fn no_publisher_yields_none_in_gossip_payload() {
    // Storage-only nodes and test harnesses that don't wire a MIP
    // must produce the legacy "no signal" shape: `current_in_flight:
    // None`. Older peers without the field deserialize that as
    // None too, so scoring falls back to the founder's local view.
    let state = AppState::new(NodeId::from_u128(4), empty_mesh());
    let caps = build_local_capabilities(None, 300, 1.0, None, Some(&state)).await;
    assert_eq!(
        caps.current_in_flight, None,
        "no publisher → None in gossip (legacy-compatible)"
    );
    let json = serde_json::to_string(&caps).expect("serialize");
    assert!(
        !json.contains("current_in_flight"),
        "None must be skipped on the wire for byte-economy: {json}"
    );
}

/// Property 4 — the inbound half of the load signal, on the **desktop**
/// topology, where it is currently a known gap.
///
/// `local_inference` here is `SovereignInferenceAdapter(engine)` with no
/// `MeshInferenceProvider` in the stack. That is exactly what the desktop
/// installs (`sovereign-desktop/src-tauri/src/state.rs:952` hands the mesh
/// `raw_inference`), and it is deliberate — the comment at `state.rs:941-953`
/// says a peer POSTing to `:9741` must be served "without re-entering the
/// mesh-routing wrapper and ping-ponging the request back out".
///
/// The consequence, which this test pins so it cannot regress silently: every
/// writer of the published counter is an `enter_local_total` inside the MIP
/// (`peer_inference.rs:1888`), so with no MIP in the path, peer-served work
/// moves nothing. On desktop it is worse than a stale number — nothing calls
/// `install_in_flight_publisher` at all, so `current_in_flight` is omitted
/// from gossip entirely (`Option::is_none` + `skip_serializing_if`,
/// `commonwealth-core/src/capabilities.rs:87-88`) and a founder scoring that
/// node falls back to its own dispatch count, reading a pinned machine as idle.
///
/// This is asserted as **current behaviour, not desired behaviour**. The CLI
/// daemon puts the MIP in the inbound path and does move the counter; the two
/// surfaces disagree, and reconciling them is open work (SCHEDULER_QUALITY.md
/// F2). When that lands, this test should flip to `>= 1` rather than be deleted
/// — the sampling harness is the part worth keeping.
///
/// The counter is sampled *during* generation via the provider hook, because
/// reading it after the response returns cannot distinguish "never
/// incremented" from "incremented and correctly released".
#[tokio::test]
async fn desktop_topology_serving_a_peer_request_does_not_publish_in_flight() {
    let self_id = NodeId::from_u128(0x5EF_u128);
    let mut members = std::collections::HashMap::new();
    members.insert(
        self_id,
        member_with_last_seen(self_id, "self", 100, "127.0.0.1:9742".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(77),
        name: "inbound-load-test".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };

    // The atomic gossip publishes. Shared with the probe closure
    // below so the provider can read it mid-serve.
    let publisher = Arc::new(AtomicU32::new(0));
    // `u32::MAX` is the "hook never fired" sentinel — it separates
    // "the counter did not move" from "the request never reached
    // local_inference at all", which would otherwise both read as a
    // failure with no way to tell them apart.
    let observed = Arc::new(AtomicU32::new(u32::MAX));
    let probe_publisher = Arc::clone(&publisher);
    let probe_observed = Arc::clone(&observed);

    let provider: Arc<dyn InferenceProvider> = Arc::new(
        TestProvider::new()
            .with_model_id("stub-primary")
            .with_complete_text("ok")
            .with_on_complete(move || {
                probe_observed.store(probe_publisher.load(Ordering::Relaxed), Ordering::Relaxed);
            }),
    );
    let adapter: Arc<dyn LocalInferenceService> =
        Arc::new(SovereignInferenceAdapter::new(provider));

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    // `with_local_inference` goes through `Arc::get_mut` and must run
    // before anything clones `inner` (see `injection_order.rs`);
    // `install_in_flight_publisher` is a OnceLock and has no such
    // ordering constraint, so it follows.
    let state =
        AppState::new_with_platform_and_engine(self_id, mesh, mesh_store, app_registry, None)
            .with_local_inference(adapter);
    state.install_in_flight_publisher(Arc::clone(&publisher));

    let addr = spawn_router(client_router(state)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        // 32 hex chars, big-endian u128 — `headers::parse_x_node_id`'s
        // shape. A different id than `self_id`: this is peer traffic.
        .header("X-Node-Id", format!("{:032x}", 0xBEEF_u128))
        .json(&serde_json::json!({
            "model": "stub-primary",
            "messages": [{"role": "user", "content": "ping"}],
            "stream": false,
        }))
        .send()
        .await
        .expect("/v1/chat/completions must be reachable");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "setup sanity: the peer request must actually be served locally"
    );

    let during = observed.load(Ordering::Relaxed);
    assert_ne!(
        during,
        u32::MAX,
        "setup sanity: the provider hook never fired, so this request \
         did not reach local_inference and the assertion below would be \
         measuring nothing"
    );
    assert_eq!(
        during, 0,
        "CURRENT behaviour, pinned so the gap cannot close or widen silently: \
         with no MeshInferenceProvider in the inbound path there is no \
         `enter_local_total` to bump, so peer-served work is invisible to \
         gossip. If this now reads {during}, the desktop topology gained a \
         load-publishing path — that is the fix SCHEDULER_QUALITY.md F2 wants, \
         so flip this assertion to `>= 1` and update the doc comment above."
    );

    // Whatever the count, the guard accounting must balance — a leak here
    // would make a node advertise permanent load and never be chosen again.
    assert_eq!(
        publisher.load(Ordering::Relaxed),
        0,
        "the in-flight guard must drop once the response is returned"
    );
}
