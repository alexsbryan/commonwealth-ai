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
//!
//! These together prove the founder-side scoring CAN see what a
//! peer is gossiping. The scoring-side override (preferring the
//! gossiped value over the founder's local view) is unit-tested
//! in `peer_inference.rs::tests::gossiped_in_flight_overrides_self_observed`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use sovereign_mesh::capabilities::build_local_capabilities;

fn empty_mesh() -> Mesh {
    Mesh {
        id: MeshId::from_u128(1),
        name: "test".into(),
        join_key_hash: [0u8; 32],
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
        100, // reported_at
        1.0, // inference_availability
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
    let caps_after = build_local_capabilities(
        None,
        101,
        1.0,
        None,
        Some(&state),
    )
    .await;
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
        caps.current_in_flight,
        None,
        "no publisher → None in gossip (legacy-compatible)"
    );
    let json = serde_json::to_string(&caps).expect("serialize");
    assert!(
        !json.contains("current_in_flight"),
        "None must be skipped on the wire for byte-economy: {json}"
    );
}
