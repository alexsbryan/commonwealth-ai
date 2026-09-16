// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire-level proof for the gossip-load-awareness fix
//! (`sovereign/docs/MESH_LOAD_AWARENESS.md`).
//!
//! Asserts the three load-bearing properties:
//!
//! 1. The in-flight gauge is created before the provider and handed to both:
//!    `AppState` reads the same atomic the router-side handle writes to, and
//!    there is no install step to clobber it (the one-shot
//!    `install_in_flight_publisher` this file used to exercise is gone — the
//!    property is now structural, ARCH 10).
//! 2. `AppState::current_local_in_flight` reads that same atomic. Bump on the
//!    router-side handle, observe through `AppState`.
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
//!    (`routes_inference.rs:171`) with no `InferenceRouter` in
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

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_state::MeshStore;
use sovereign_api::server::client_router;
use sovereign_api::state::{AppState, LocalInferenceService, ServingSeed};
use sovereign_core::in_flight::LocalInFlightGauge;
use sovereign_core::traits::InferenceProvider;
use sovereign_mesh::capabilities::build_local_capabilities;
use sovereign_mesh::inference_adapter::SovereignInferenceAdapter;
use sovereign_mesh::slot_manifest::CoreSlotManifest;
use sovereign_meshapp_registry::registry::AppRegistry;

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

/// An `AppState` holding `gauge` — the production shape, where the node
/// creates the gauge before the provider and gives the same handle to both.
fn app_state_with_gauge(id: NodeId, mesh: Mesh, gauge: LocalInFlightGauge) -> AppState {
    AppState::new_with_platform_and_engine_and_gauge(
        id,
        mesh,
        Arc::new(MeshStore::in_memory().unwrap()),
        Arc::new(AppRegistry::new()),
        None,
        Some(gauge),
    )
}

#[tokio::test]
async fn appstate_reads_the_gauge_it_was_constructed_with() {
    let gauge = LocalInFlightGauge::new();
    let state = app_state_with_gauge(NodeId::from_u128(1), empty_mesh(), gauge.clone());

    // Mutate through the gauge's provider-facing Arc; AppState must read the
    // same atomic.
    gauge.arc().store(3, Ordering::Relaxed);
    assert_eq!(state.current_local_in_flight(), Some(3));

    // The reload path reads the handle back to hand to the new router, so it
    // must be the gauge's own Arc — the property the old one-shot install
    // protected, now guaranteed by there being exactly one gauge.
    assert!(
        Arc::ptr_eq(
            &gauge.arc(),
            &state
                .in_flight_publisher()
                .expect("a node constructed with a gauge reports one")
        ),
        "in_flight_publisher must return the gauge's own Arc"
    );
}

#[tokio::test]
async fn build_local_capabilities_publishes_in_flight_through_appstate() {
    let gauge = LocalInFlightGauge::new();
    let state = app_state_with_gauge(NodeId::from_u128(2), empty_mesh(), gauge.clone());

    // Bump the gauge — simulates a `LocalTotalGuard` being alive on the
    // router side.
    gauge.set(5);

    let caps = build_local_capabilities(
        None, // no CorpusEngine — irrelevant for this assertion
        100,  // reported_at
        &state,
    )
    .await;

    assert_eq!(
        caps.current_in_flight,
        Some(5),
        "gossip payload must reflect the live router-side publisher value"
    );

    // Drain back to zero and rebuild — the next gossip tick must
    // see the drop, not a stale snapshot.
    gauge.set(0);
    let caps_after = build_local_capabilities(None, 101, &state).await;
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
    let gauge = LocalInFlightGauge::new();
    let state = app_state_with_gauge(NodeId::from_u128(3), empty_mesh(), gauge.clone());
    gauge.set(11);

    let caps = build_local_capabilities(None, 200, &state).await;
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
    // Storage-only nodes and test harnesses that don't wire a router
    // must produce the legacy "no signal" shape: `current_in_flight:
    // None`. Older peers without the field deserialize that as
    // None too, so scoring falls back to the founder's local view.
    let state = AppState::new(NodeId::from_u128(4), empty_mesh());
    let caps = build_local_capabilities(None, 300, &state).await;
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

/// The storage half of the `SelfClaims` port on the real `AppState`
/// implementation: Fabric hands the measured usage back, the node remembers it,
/// and the remaining budget it answers clamps the published free storage. The
/// trait's own tests use a fake; this is the positive control for the wiring.
#[tokio::test]
async fn self_claims_publishes_storage_remaining_from_the_budget() {
    let state = AppState::new(NodeId::from_u128(5), empty_mesh());
    let ten_gib = 10 * 1_073_741_824_u64;
    state
        .set_storage_budget_bytes(Some(ten_gib))
        .expect("10 GiB is a legal budget");

    // No engine → the measured usage is 0, so the whole budget remains.
    let claims = sovereign_core::self_claims::SelfClaims::claims(&state).await;
    assert_eq!(
        claims.storage_remaining,
        Some(ten_gib),
        "an unset engine usage must leave the whole budget remaining"
    );

    // And the builder clamps the published free storage to that remaining
    // budget — the behaviour the port replaced a direct `AppState` read for.
    let caps = build_local_capabilities(None, 500, &state).await;
    assert!(
        caps.hardware.free_storage_gb <= 10,
        "budget remaining of 10 GiB must clamp published free_storage_gb, got {}",
        caps.hardware.free_storage_gb
    );
}

/// Property 4 — the inbound half of the load signal, on the **desktop**
/// topology, where it is currently a known gap.
///
/// `local_inference` here is `SovereignInferenceAdapter(engine)` with no
/// `InferenceRouter` in the stack. That is exactly what the desktop
/// installs (`sovereign-desktop/src-tauri/src/state.rs:952` hands the mesh
/// `raw_inference`), and it is deliberate — the comment at `state.rs:941-953`
/// says a peer POSTing to `:9741` must be served "without re-entering the
/// mesh-routing wrapper and ping-ponging the request back out".
///
/// The consequence, which this test pins so it cannot regress silently: every
/// writer of the published counter is an `enter_local_total` inside the router
/// (`peer_inference.rs:1888`), so with no router in the path, peer-served work
/// moves nothing. On desktop it is worse than a stale number — the bootstrap
/// mints a gauge only when it builds a router, so a router-less node holds
/// none and `current_in_flight` is omitted from gossip entirely
/// (`Option::is_none` + `skip_serializing_if`,
/// `commonwealth-core/src/capabilities.rs:87-88`) and a founder scoring that
/// node falls back to its own dispatch count, reading a pinned machine as idle.
///
/// This is asserted as **current behaviour, not desired behaviour**. The CLI
/// daemon puts the router in the inbound path and does move the counter; the two
/// surfaces disagree, and reconciling them is open work (SCHEDULER_QUALITY.md
/// F2). When that lands, this test should flip to `>= 1` rather than be deleted
/// — the sampling harness is the part worth keeping.
///
/// The counter is sampled *during* generation via the provider hook, because
/// reading it after the response returns cannot distinguish "never
/// incremented" from "incremented and correctly released". The gauge below is
/// the test's probe — a stand-in for the router-side handle a serving node
/// would hold — not a claim that the desktop topology has one.
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

    // The gauge gossip publishes. Created before the node and shared with the
    // probe closure below so the provider can read it mid-serve.
    let gauge = LocalInFlightGauge::new();
    let publisher = gauge.arc();
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
    let adapter: Arc<dyn LocalInferenceService> = Arc::new(SovereignInferenceAdapter::new(
        provider,
        Arc::new(CoreSlotManifest),
    ));

    let mesh_store = Arc::new(MeshStore::in_memory().unwrap());
    let app_registry = Arc::new(AppRegistry::new());
    // The gauge and the inference provider are both construction arguments now,
    // so there is no `Arc::get_mut` installer whose ordering could silently
    // drop the provider (DC §4.2 "Construction is staged, and parts are
    // total").
    let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving(
        self_id,
        mesh,
        mesh_store,
        app_registry,
        None,
        Some(gauge),
        sovereign_api::state::FabricSeed::default(),
        ServingSeed {
            local_inference: Some(adapter),
            ..Default::default()
        },
    );

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
         with no InferenceRouter in the inbound path there is no \
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
