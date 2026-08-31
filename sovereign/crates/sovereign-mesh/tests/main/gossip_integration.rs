// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end gossip convergence test.
//!
//! Binds two real `commonwealth_api::internal_router` instances on
//! ephemeral localhost ports (skipping `EmbeddedDaemon`'s hardcoded
//! 9742), seeds each with a distinct `AppState` on the same mesh,
//! and drives `sovereign_mesh::gossip::run_one_round` between them.
//!
//! Proves the bug reported in the vast-knitting-seal plan: "Peer A
//! stays at 1/1 while Peer B shows 2/2" is caused solely by the
//! absence of gossip — give the loop real HTTP endpoints to talk
//! to, and both sides converge in a single round.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use sovereign_mesh::gossip;

fn member_at(id: NodeId, name: &str, last_seen: u64, addr: SocketAddr) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 0,
                cpu_cores: 0,
                total_storage_gb: 0,
                free_storage_gb: 0,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: last_seen,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec![addr],
    }
}

/// Bind `internal_router(state)` on `127.0.0.1:0`, return the bound
/// address and keep the server running for the test's lifetime.
/// The JoinHandle is intentionally leaked — it lives as long as the
/// test process, which is bounded by tokio::test's drop.
async fn spawn_internal_router(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = internal_router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    // Give tokio a tick to start accepting.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

#[tokio::test]
async fn two_peers_converge_via_one_gossip_round() {
    // Shared mesh identity — both sides agree on id + hash so the
    // auth guard in `Mesh::merge_from` doesn't reject.
    let mesh_id = MeshId::from_u128(42);
    let hash = [11u8; 32];

    let a_id = NodeId::from_u128(100);
    let b_id = NodeId::from_u128(200);

    // Two independent AppStates (representing founder + late-joining
    // peer), each bound to an ephemeral port.
    let mesh_a = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "Test".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            // Peer A starts knowing only about themselves — the
            // "founder before handshake" case.
            m.insert(
                a_id,
                member_at(a_id, "A", 100, "127.0.0.1:1111".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_a = AppState::new(a_id, mesh_a);
    let addr_a = spawn_internal_router(state_a.clone()).await;

    let mesh_b = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "Test".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            // Peer B already knows about both themselves and A —
            // the shape you'd see right after a successful
            // `/internal/join` handshake.
            m.insert(a_id, member_at(a_id, "A", 100, addr_a));
            m.insert(
                b_id,
                member_at(b_id, "B", 150, "127.0.0.1:2222".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_b = AppState::new(b_id, mesh_b);
    let _addr_b = spawn_internal_router(state_b.clone()).await;

    // Sanity check: before gossip, A has 1 member, B has 2.
    assert_eq!(state_a.inner.mesh.read().await.members.len(), 1);
    assert_eq!(state_b.inner.mesh.read().await.members.len(), 2);

    // Bootstrap A with B's address so A can find B during gossip —
    // this is what the join handshake's `adopt mesh snapshot` step
    // would normally deliver. Simulating it by hand keeps the test
    // focused on gossip and independent of the handshake code path.
    {
        let mut mesh = state_a.inner.mesh.write().await;
        mesh.members
            .insert(b_id, member_at(b_id, "B", 150, _addr_b));
    }
    assert_eq!(state_a.inner.mesh.read().await.members.len(), 2);

    // Drive one round on A. Inside run_one_round, A picks B (the
    // only non-self peer), POSTs to B's `/internal/gossip`, B
    // merges, and returns its updated view — which A merges in.
    // After this: both sides have the union of views.
    gossip::run_one_round(&state_a, Duration::from_secs(60))
        .await
        .expect("gossip round should succeed");

    // Both AppStates now contain both members.
    let a_after = state_a.inner.mesh.read().await;
    assert_eq!(a_after.members.len(), 2);
    assert!(a_after.members.contains_key(&a_id));
    assert!(a_after.members.contains_key(&b_id));

    let b_after = state_b.inner.mesh.read().await;
    assert_eq!(b_after.members.len(), 2);
    assert!(b_after.members.contains_key(&a_id));
    assert!(b_after.members.contains_key(&b_id));

    // A's self record was touched to "now" — regardless of what
    // the initial last_seen was, it should be greater than it was
    // before the round. (We seeded A's self.last_seen at 100; real
    // time is a large unix timestamp, so after the round it must
    // be strictly greater.)
    assert!(
        a_after.members.get(&a_id).unwrap().last_seen > 100,
        "self last_seen should have been bumped to now()"
    );
}

#[tokio::test]
async fn gossip_decays_peer_after_local_contact_goes_stale() {
    // New model: offline-decay measures LOCAL-observation staleness, not the
    // peer's gossiped `last_seen`. A ghost with no HTTP server is never
    // re-observed; it gets one grace window (lazy-init to now), then decays
    // once OUR clock advances past the threshold without re-observing it.
    let me = NodeId::from_u128(1);
    let ghost = NodeId::from_u128(2);

    let mut members = HashMap::new();
    members.insert(
        me,
        member_at(me, "Me", 1_000, "127.0.0.1:9000".parse().unwrap()),
    );
    members.insert(
        ghost,
        member_at(ghost, "Ghost", 1_000, "127.0.0.1:9001".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "Test".into(),
        invite_key_hash: [1u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let state = Arc::new(AppState::new(me, mesh));
    let clock = commonwealth_core::TestClock::new(1_000);
    state.install_clock(Arc::new(clock.clone()));

    // Round 1: ghost is lazy-init'd to now (grace window) — NOT decayed yet.
    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("gossip round should not error even when peer unreachable");
    assert_eq!(
        state
            .inner
            .mesh
            .read()
            .await
            .members
            .get(&ghost)
            .unwrap()
            .status,
        NodeStatus::Online,
        "a freshly-seen ghost gets a grace window before decay"
    );

    // Advance OUR clock past the threshold; the ghost has no server so it is
    // never re-observed → its local-contact stamp goes stale → decay.
    clock.advance(120);
    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("gossip round should not error even when peer unreachable");

    let after = state.inner.mesh.read().await;
    assert_eq!(
        after.members.get(&ghost).unwrap().status,
        NodeStatus::Offline,
        "ghost should decay once local contact is older than the threshold"
    );
    // Own record stays Online — self is exempt from decay and refreshes each round.
    assert_eq!(after.members.get(&me).unwrap().status, NodeStatus::Online);
}

#[tokio::test]
async fn gossip_skewed_last_seen_does_not_false_decay() {
    // Regression for the "~9 min flap": a peer whose gossiped `last_seen` is
    // wildly skewed (here, ≈epoch — a clock far behind ours) must NOT decay as
    // long as we observed it locally within the threshold. Under the old
    // `now - last_seen` decay it flipped Offline immediately; under
    // local-observation decay it stays Online.
    let me = NodeId::from_u128(1);
    let peer = NodeId::from_u128(2);

    let mut members = HashMap::new();
    members.insert(
        me,
        member_at(me, "Me", 1_000, "127.0.0.1:9000".parse().unwrap()),
    );
    members.insert(
        peer,
        member_at(peer, "Skewed", 1, "127.0.0.1:9001".parse().unwrap()),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "Test".into(),
        invite_key_hash: [1u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let state = Arc::new(AppState::new(me, mesh));
    state.install_clock(Arc::new(commonwealth_core::TestClock::new(1_000)));

    // We observed the peer locally at now (1_000) — a recent exchange — even
    // though its self-stamped last_seen is ancient (skewed clock).
    state.observe_peer_contact(peer, 1_000);

    gossip::run_one_round(&state, Duration::from_secs(60))
        .await
        .expect("gossip round should not error");

    let after = state.inner.mesh.read().await;
    assert_eq!(
        after.members.get(&peer).unwrap().status,
        NodeStatus::Online,
        "a skewed last_seen must not flap an observed peer Offline"
    );
}

#[tokio::test]
async fn answering_peer_whose_record_is_frozen_must_not_decay() {
    // REGRESSION (2026-07-29, observed live). A peer that ANSWERS every
    // gossip round must never decay Offline, even when its own record
    // stops advancing.
    //
    // The bug: liveness was stamped only from `MergeReport.observed`,
    // which by contract holds the peers whose record ADVANCED in the
    // merge. A peer answering us every round with an unchanged record
    // was therefore never stamped, and decayed on schedule while
    // `gossip: reach ok` kept logging success.
    //
    // Production shape this reproduces: BeefyMac replied to four
    // consecutive rounds at 46/55/63/69 ms, its self `last_seen` frozen
    // (its own outbound loop was wedged, its inbound handler was fine),
    // and we marked it Offline at staleness_secs=67 — emptying the
    // eligible-worker set and costing the distributed 122B its shard.
    //
    // Modelled here by freezing B's clock while A's advances: B keeps
    // serving, but every reply carries the same `last_seen`, so nothing
    // ever lands in `observed`.
    let mesh_id = MeshId::from_u128(77);
    let hash = [5u8; 32];
    let a_id = NodeId::from_u128(1);
    let b_id = NodeId::from_u128(2);

    // B: a healthy peer with a REAL router, on a clock that never moves.
    let mesh_b = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "Test".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(
                b_id,
                member_at(b_id, "B", 1_000, "127.0.0.1:2222".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_b = Arc::new(AppState::new(b_id, mesh_b));
    state_b.install_clock(Arc::new(commonwealth_core::TestClock::new(1_000)));
    let addr_b = spawn_internal_router((*state_b).clone()).await;

    let mesh_a = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "Test".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(
                a_id,
                member_at(a_id, "A", 1_000, "127.0.0.1:1111".parse().unwrap()),
            );
            m.insert(b_id, member_at(b_id, "B", 1_000, addr_b));
            m
        },
        peers: vec![],
    };
    let state_a = Arc::new(AppState::new(a_id, mesh_a));
    let clock_a = commonwealth_core::TestClock::new(1_000);
    state_a.install_clock(Arc::new(clock_a.clone()));

    // Round 1 at t=1000: A reaches B and converges.
    gossip::run_one_round(&state_a, Duration::from_secs(60))
        .await
        .expect("round 1 should succeed");
    assert_eq!(
        state_a.peer_contact_or_init(b_id, 0),
        1_000,
        "round 1 must stamp local contact at t=1000"
    );
    assert_eq!(
        state_a
            .inner
            .mesh
            .read()
            .await
            .members
            .get(&b_id)
            .unwrap()
            .status,
        NodeStatus::Online,
        "B must be Online after a successful round"
    );

    // Advance ONLY A's clock well past the 60s threshold. B still
    // serves, but its frozen clock means its record never advances,
    // so B cannot appear in `observed` on any later merge.
    clock_a.advance(120);

    // Round 2 at t=1120: A reaches B successfully again. THE ROUND-TRIP
    // ITSELF is the liveness evidence — it must stamp B.
    gossip::run_one_round(&state_a, Duration::from_secs(60))
        .await
        .expect("round 2 should succeed");
    // THE ASSERTION WITH TEETH. Status alone is NOT it: on a successful
    // reach the round unconditionally forces `status = Online` (see the
    // `peer back Online` fix-up in run_one_round), which masks the defect
    // for any peer selected that round. The damage is done through
    // `last_contact`, which that fix-up does NOT touch — it ages forever
    // while we keep reaching the peer, so the decay pass at the TOP of
    // every subsequent round re-marks the peer Offline, and it only gets
    // flipped back if the FANOUT=2 selection happens to include it. That
    // Offline window is what empties the eligible-worker set.
    //
    // So assert the quantity decay actually reads.
    assert_eq!(
        state_a.peer_contact_or_init(b_id, 0),
        1_120,
        "a completed round-trip MUST stamp local contact — otherwise \
         last_contact ages while the peer answers every round, and the \
         decay pass flaps it Offline on schedule (observed live 2026-07-29: \
         reach ok at 46/55/63/69 ms in the four rounds before \
         `peer marked Offline … staleness_secs=67`)"
    );
    // And with contact fresh, decay must not fire at all.
    assert_eq!(
        state_a
            .inner
            .mesh
            .read()
            .await
            .members
            .get(&b_id)
            .unwrap()
            .status,
        NodeStatus::Online,
        "a peer that answered this very round must be Online"
    );
}

#[tokio::test]
async fn departure_tombstones_self_on_peers() {
    // B calls announce_departure → it pushes its own tombstoned record to A's
    // /internal/gossip → A removes B mesh-wide (event-time LWW), instead of
    // keeping B as a live ghost.
    let mesh_id = MeshId::from_u128(42);
    let hash = [11u8; 32];
    let a_id = NodeId::from_u128(100);
    let b_id = NodeId::from_u128(200);

    // A starts knowing only itself; its server must be up so B can reach it.
    let mesh_a = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(
                a_id,
                member_at(a_id, "A", 100, "127.0.0.1:1".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_a = AppState::new(a_id, mesh_a);
    let addr_a = spawn_internal_router(state_a.clone()).await;

    // B knows A (at A's real addr) + itself.
    let mesh_b = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(a_id, member_at(a_id, "A", 100, addr_a));
            m.insert(
                b_id,
                member_at(b_id, "B", 150, "127.0.0.1:2".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_b = AppState::new(b_id, mesh_b);

    // A learns B (so it has a record to tombstone).
    {
        let mut mesh = state_a.inner.mesh.write().await;
        mesh.members.insert(
            b_id,
            member_at(b_id, "B", 150, "127.0.0.1:2".parse().unwrap()),
        );
    }
    assert!(state_a.inner.mesh.read().await.members[&b_id].is_active());

    // B departs — pushes its self-tombstone to A.
    gossip::announce_departure(&state_b).await;

    let a = state_a.inner.mesh.read().await;
    let b_rec = a.members.get(&b_id).expect("A retains a record for B");
    assert!(
        b_rec.removed_at.is_some(),
        "A should have tombstoned B after B's departure"
    );
    assert!(!b_rec.is_active(), "B should be inactive (tombstoned) on A");
}
