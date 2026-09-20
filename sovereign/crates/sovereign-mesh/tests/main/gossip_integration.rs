// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end gossip convergence test.
//!
//! Binds two real `sovereign_daemon::internal_router` instances on
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

use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;
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
            origins: Vec::new(),
            media_allow: Vec::new(),
            media_available: None,

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
    assert_eq!(state_a.inner.fabric.mesh.read().await.members.len(), 1);
    assert_eq!(state_b.inner.fabric.mesh.read().await.members.len(), 2);

    // Bootstrap A with B's address so A can find B during gossip —
    // this is what the join handshake's `adopt mesh snapshot` step
    // would normally deliver. Simulating it by hand keeps the test
    // focused on gossip and independent of the handshake code path.
    {
        let mut mesh = state_a.inner.fabric.mesh.write().await;
        mesh.members
            .insert(b_id, member_at(b_id, "B", 150, _addr_b));
    }
    assert_eq!(state_a.inner.fabric.mesh.read().await.members.len(), 2);

    // Drive one round on A. Inside run_one_round, A picks B (the
    // only non-self peer), POSTs to B's `/internal/gossip`, B
    // merges, and returns its updated view — which A merges in.
    // After this: both sides have the union of views.
    gossip::run_one_round(
        &*state_a.inner.fabric,
        state_a.inner.node.corpus_engine.as_ref(),
        &state_a,
        Duration::from_secs(60),
    )
    .await
    .expect("gossip round should succeed");

    // Both AppStates now contain both members.
    let a_after = state_a.inner.fabric.mesh.read().await;
    assert_eq!(a_after.members.len(), 2);
    assert!(a_after.members.contains_key(&a_id));
    assert!(a_after.members.contains_key(&b_id));

    let b_after = state_b.inner.fabric.mesh.read().await;
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
    state.clock_reader().publish(Arc::new(clock.clone()));

    // Round 1: ghost is lazy-init'd to now (grace window) — NOT decayed yet.
    gossip::run_one_round(
        &*state.inner.fabric,
        state.inner.node.corpus_engine.as_ref(),
        &*state,
        Duration::from_secs(60),
    )
    .await
    .expect("gossip round should not error even when peer unreachable");
    assert_eq!(
        state
            .inner
            .fabric
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
    gossip::run_one_round(
        &*state.inner.fabric,
        state.inner.node.corpus_engine.as_ref(),
        &*state,
        Duration::from_secs(60),
    )
    .await
    .expect("gossip round should not error even when peer unreachable");

    let after = state.inner.fabric.mesh.read().await;
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
    state
        .clock_reader()
        .publish(Arc::new(commonwealth_core::TestClock::new(1_000)));

    // We observed the peer locally at now (1_000) — a recent exchange — even
    // though its self-stamped last_seen is ancient (skewed clock).
    state.observe_peer_contact(peer, 1_000);

    gossip::run_one_round(
        &*state.inner.fabric,
        state.inner.node.corpus_engine.as_ref(),
        &*state,
        Duration::from_secs(60),
    )
    .await
    .expect("gossip round should not error");

    let after = state.inner.fabric.mesh.read().await;
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
    state_b
        .clock_reader()
        .publish(Arc::new(commonwealth_core::TestClock::new(1_000)));
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
    state_a.clock_reader().publish(Arc::new(clock_a.clone()));

    // Round 1 at t=1000: A reaches B and converges.
    gossip::run_one_round(
        &*state_a.inner.fabric,
        state_a.inner.node.corpus_engine.as_ref(),
        &*state_a,
        Duration::from_secs(60),
    )
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
            .fabric
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
    gossip::run_one_round(
        &*state_a.inner.fabric,
        state_a.inner.node.corpus_engine.as_ref(),
        &*state_a,
        Duration::from_secs(60),
    )
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
            .fabric
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
        let mut mesh = state_a.inner.fabric.mesh.write().await;
        mesh.members.insert(
            b_id,
            member_at(b_id, "B", 150, "127.0.0.1:2".parse().unwrap()),
        );
    }
    assert!(state_a.inner.fabric.mesh.read().await.members[&b_id].is_active());

    // B departs — pushes its self-tombstone to A.
    gossip::announce_departure(&*state_b.inner.fabric).await;

    let a = state_a.inner.fabric.mesh.read().await;
    let b_rec = a.members.get(&b_id).expect("A retains a record for B");
    assert!(
        b_rec.removed_at.is_some(),
        "A should have tombstoned B after B's departure"
    );
    assert!(!b_rec.is_active(), "B should be inactive (tombstoned) on A");
}

/// An offer the holder accepts is listed on a PEER's media rail one gossip
/// round later, and a withdrawal is gone one round after that.
///
/// The bar is the round, not the clock: `GET /v1/mesh/media` reads a peer's
/// merged capabilities, so the holder's self-stamp and the peer's merge are
/// the only two steps between `svrn mesh media offer` and a viewer seeing the
/// library. Room run 2 (2026-09-20) listed an offer in 8.39 s and then kept
/// listing it for 98.02 s after the withdrawal, with `gossip: reach ok` on
/// both sides every 10 s throughout — a delay neither side could be charged
/// with, because neither side said anything. This test holds the invariant
/// the two glassbox lines were added to measure.
#[tokio::test]
async fn an_offer_and_its_withdrawal_reach_a_peers_media_rail_in_one_round() {
    let mesh_id = MeshId::from_u128(42);
    let hash = [11u8; 32];
    let holder = NodeId::from_u128(100);
    let viewer = NodeId::from_u128(200);

    let mesh_holder = Mesh {
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
                holder,
                member_at(holder, "LittleMac", 100, "127.0.0.1:1".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_holder = AppState::new(holder, mesh_holder);
    let addr_holder = spawn_internal_router(state_holder.clone()).await;

    let mesh_viewer = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(holder, member_at(holder, "LittleMac", 100, addr_holder));
            m.insert(
                viewer,
                member_at(viewer, "BeefyMac", 150, "127.0.0.1:2".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_viewer = AppState::new(viewer, mesh_viewer);
    let addr_viewer = spawn_internal_router(state_viewer.clone()).await;
    {
        let mut mesh = state_holder.inner.fabric.mesh.write().await;
        mesh.members
            .insert(viewer, member_at(viewer, "BeefyMac", 150, addr_viewer));
    }

    // `svrn mesh media offer`: the live route now serves a media origin, and
    // the holder's own presence poll says the library is free.
    state_holder.update_local_media_available(Some(1.0)).await;
    state_holder.inner.fabric.dial_info.publish(Arc::new(|| {
        commonwealth_core::mesh::IrohDialInfo {
            relay_url: None,
            direct_addrs: Vec::new(),
            origins: vec![commonwealth_core::capabilities::OriginKind::Media],
            media_allow: vec!["BeefyMac".into()],
        }
    }));

    let round = |state: &AppState| {
        let state = state.clone();
        async move {
            gossip::run_one_round(
                &*state.inner.fabric,
                state.inner.node.corpus_engine.as_ref(),
                &state,
                Duration::from_secs(60),
            )
            .await
            .expect("gossip round should succeed")
        }
    };
    round(&state_holder).await;

    let rail = || async {
        let m = state_viewer.inner.fabric.mesh.read().await;
        commonwealth_media::offers(
            viewer,
            &commonwealth_media::roster_of(&m),
            &[],
            commonwealth_core::capabilities::OriginKind::Media,
        )
    };

    let rows = rail().await;
    assert_eq!(
        rows.len(),
        1,
        "one round after the offer, the viewer's rail must list the holder: {rows:?}"
    );
    assert_eq!(rows[0].peer, "LittleMac");
    assert_eq!(rows[0].offered_to, vec!["BeefyMac".to_string()]);
    assert_eq!(
        rows[0].media_available,
        Some(1.0),
        "the holder's reading travels with the offer it describes"
    );

    // `svrn mesh media withdraw`: the route stops serving the origin. The
    // wait is the gossip interval in miniature and it is load-bearing: LWW
    // compares `event_time()`, which is `last_seen` in whole SECONDS, so two
    // rounds inside one second are indistinguishable to a peer and the
    // second one is skipped as `LocalRecordNotOlder`. A real round is 10 s
    // (`DEFAULT_GOSSIP_INTERVAL`); this test only needs the second to turn.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    state_holder.inner.fabric.dial_info.publish(Arc::new(|| {
        commonwealth_core::mesh::IrohDialInfo {
            relay_url: None,
            direct_addrs: Vec::new(),
            origins: Vec::new(),
            media_allow: Vec::new(),
        }
    }));
    round(&state_holder).await;

    // Which side is being measured, said in the test as the two glassbox
    // lines say it in a run: the holder's own record first, then the peer's
    // copy. A failure here names the stamp; a failure below names the merge.
    {
        let m = state_holder.inner.fabric.mesh.read().await;
        assert!(
            m.members[&holder].capabilities.origins.is_empty(),
            "the holder's own record must stop offering in the round after the withdrawal: {:?}",
            m.members[&holder].capabilities.origins
        );
    }

    let rows = rail().await;
    assert!(
        rows.is_empty(),
        "one round after the withdrawal the viewer's rail must list nothing: {rows:?}"
    );
}

/// An offer already on the holder's record keeps reaching a peer's media rail
/// across rounds that have NO live dial info.
///
/// This is A45's measured failure in miniature. `svrn mesh media offer`
/// hot-reloads the offer route, the iroh endpoint is rebuilt, and for a while
/// `self_iroh_dialinfo()` answers `None` — 221 s of it in the room run. Every
/// round in that window replaced the holder's capabilities with `fresh_caps`,
/// whose `origins`/`media_allow` are empty by construction, and skipped the
/// only site that fills them; the holder published `origins=[]` and the peer's
/// LWW kept `[]` until dial info returned, 22 rounds later (little stamped
/// 03:27:04, peers flipped 03:30:45). Three rounds is enough: the first one
/// that blanks the triple loses the offer.
#[tokio::test]
async fn an_offer_on_record_survives_rounds_with_no_dial_info() {
    let mesh_id = MeshId::from_u128(44);
    let hash = [13u8; 32];
    let holder = NodeId::from_u128(100);
    let viewer = NodeId::from_u128(200);

    // The holder's OWN record already carries the offer — what an earlier
    // round's live read left behind, before the endpoint was rebuilt. Nothing
    // is published to the dial-info reader, so `self_iroh_dialinfo()` is
    // `None` for every round below.
    let mut holder_rec = member_at(holder, "LittleMac", 100, "127.0.0.1:1".parse().unwrap());
    holder_rec.capabilities.origins = vec![commonwealth_core::capabilities::OriginKind::Media];
    holder_rec.capabilities.media_allow = vec!["BeefyMac".into()];
    holder_rec.capabilities.media_available = Some(1.0);

    let mesh_holder = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(holder, holder_rec);
            m
        },
        peers: vec![],
    };
    let state_holder = AppState::new(holder, mesh_holder);
    let addr_holder = spawn_internal_router(state_holder.clone()).await;

    let mesh_viewer = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            // The viewer's copy offers nothing yet — it learns the offer from
            // the rounds below or not at all.
            m.insert(holder, member_at(holder, "LittleMac", 100, addr_holder));
            m.insert(
                viewer,
                member_at(viewer, "BeefyMac", 150, "127.0.0.1:2".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_viewer = AppState::new(viewer, mesh_viewer);
    let addr_viewer = spawn_internal_router(state_viewer.clone()).await;
    {
        let mut mesh = state_holder.inner.fabric.mesh.write().await;
        mesh.members
            .insert(viewer, member_at(viewer, "BeefyMac", 150, addr_viewer));
    }
    // The presence poll's answer rides the claims port, which knows nothing
    // about dial info; without it `fresh_caps` publishes `media_available:
    // None` and the rail's reading would be the thing that went missing.
    state_holder.update_local_media_available(Some(1.0)).await;

    for round in 0..3 {
        if round > 0 {
            // LWW compares `event_time()` in whole seconds, so two rounds
            // inside one second are indistinguishable to the peer and the
            // second is skipped as `LocalRecordNotOlder`.
            tokio::time::sleep(Duration::from_millis(1_100)).await;
        }
        gossip::run_one_round(
            &*state_holder.inner.fabric,
            state_holder.inner.node.corpus_engine.as_ref(),
            &state_holder,
            Duration::from_secs(60),
        )
        .await
        .expect("gossip round should succeed");

        // The holder's own record first, then the peer's copy — a failure
        // here names the stamp, a failure below names the merge.
        {
            let m = state_holder.inner.fabric.mesh.read().await;
            assert_eq!(
                m.members[&holder].capabilities.origins,
                vec![commonwealth_core::capabilities::OriginKind::Media],
                "round {round} with no dial info must publish the offer this \
                 node already held, not blank it: {:?}",
                m.members[&holder].capabilities
            );
        }

        let m = state_viewer.inner.fabric.mesh.read().await;
        let rows = commonwealth_media::offers(
            viewer,
            &commonwealth_media::roster_of(&m),
            &[],
            commonwealth_core::capabilities::OriginKind::Media,
        );
        assert_eq!(
            rows.len(),
            1,
            "round {round}: the viewer's rail must still list the holder: {rows:?}"
        );
        assert_eq!(rows[0].peer, "LittleMac");
        assert_eq!(rows[0].offered_to, vec!["BeefyMac".to_string()]);
        assert_eq!(
            rows[0].media_available,
            Some(1.0),
            "round {round}: the reading travels with the offer it describes"
        );
    }
}

/// The same one-round bar as above, with the contention a real node has and
/// the test above does not.
///
/// The test above awaits `run_one_round` alone: nothing else touches
/// `fabric.mesh` between the round's self-stamp (under the write lock) and
/// the snapshot it clones and sends (after the lock is released and after the
/// peer selection's `.await`s). A daemon has three writers in that gap — the
/// media-presence poll, the activity reporter, and an inbound
/// `/internal/gossip` from a peer whose copy of US still says we offer
/// nothing. Beefy's 22 silent rounds (room runs 5-6) are only explicable if
/// the bytes said `origins=[]` while the stamp said `[Media]`, so this is
/// that gap put under load. `offer_view::log_sent_snapshot` is the line that
/// speaks if it ever happens.
#[tokio::test]
async fn an_offer_survives_the_writers_that_contend_with_its_round() {
    let mesh_id = MeshId::from_u128(43);
    let hash = [12u8; 32];
    let holder = NodeId::from_u128(100);
    let viewer = NodeId::from_u128(200);

    let mesh_holder = Mesh {
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
                holder,
                member_at(holder, "LittleMac", 100, "127.0.0.1:1".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_holder = AppState::new(holder, mesh_holder);
    let addr_holder = spawn_internal_router(state_holder.clone()).await;

    let mesh_viewer = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            // The viewer's copy of the holder offers NOTHING — this is the
            // record an inbound round pushes back at the holder while the
            // holder's own round is mid-flight.
            m.insert(holder, member_at(holder, "LittleMac", 100, addr_holder));
            m.insert(
                viewer,
                member_at(viewer, "BeefyMac", 150, "127.0.0.1:2".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_viewer = AppState::new(viewer, mesh_viewer);
    let addr_viewer = spawn_internal_router(state_viewer.clone()).await;
    {
        let mut mesh = state_holder.inner.fabric.mesh.write().await;
        mesh.members
            .insert(viewer, member_at(viewer, "BeefyMac", 150, addr_viewer));
    }

    // `svrn mesh media offer`.
    state_holder.update_local_media_available(Some(1.0)).await;
    state_holder.inner.fabric.dial_info.publish(Arc::new(|| {
        commonwealth_core::mesh::IrohDialInfo {
            relay_url: None,
            direct_addrs: Vec::new(),
            origins: vec![commonwealth_core::capabilities::OriginKind::Media],
            media_allow: vec!["BeefyMac".into()],
        }
    }));

    // Writer 1 + 2: the media-presence poll and the activity reporter, both
    // hammering the holder's own claims for the duration of its round.
    let claims_writer = {
        let state = state_holder.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                state.update_local_media_available(Some(1.0)).await;
                state.update_local_availability(1.0).await;
                tokio::task::yield_now().await;
            }
        })
    };
    // Writer 3: a peer gossiping AT the holder — its round takes the holder's
    // mesh write lock inside `/internal/gossip` and pushes a record of the
    // holder that carries no offer.
    let inbound_writer = {
        let state = state_viewer.clone();
        tokio::spawn(async move {
            for _ in 0..3 {
                // Discarded deliberately (ARCH 6 named, not silent): this
                // round's own outcome is not the subject. It dials the
                // viewer's placeholder address and is EXPECTED to report the
                // peer unreachable; what it contributes is the mesh write
                // lock it takes on the holder through `/internal/gossip`,
                // which happens whether its own fan-out succeeds or not. A
                // failure here that mattered would show up as the holder's
                // assertion below going red.
                let _ = gossip::run_one_round(
                    &*state.inner.fabric,
                    state.inner.node.corpus_engine.as_ref(),
                    &state,
                    Duration::from_secs(60),
                )
                .await;
            }
        })
    };

    gossip::run_one_round(
        &*state_holder.inner.fabric,
        state_holder.inner.node.corpus_engine.as_ref(),
        &state_holder,
        Duration::from_secs(60),
    )
    .await
    .expect("gossip round should succeed");

    claims_writer.await.expect("claims writer must not panic");
    inbound_writer.await.expect("inbound writer must not panic");

    // The holder's own record is the first reading — a failure here names the
    // stamp or a writer that undid it, never the merge.
    {
        let m = state_holder.inner.fabric.mesh.read().await;
        assert_eq!(
            m.members[&holder].capabilities.origins,
            vec![commonwealth_core::capabilities::OriginKind::Media],
            "the holder's own record must still carry the offer after its round \
             raced the presence poll, the activity reporter and an inbound gossip"
        );
    }

    let rows = {
        let m = state_viewer.inner.fabric.mesh.read().await;
        commonwealth_media::offers(
            viewer,
            &commonwealth_media::roster_of(&m),
            &[],
            commonwealth_core::capabilities::OriginKind::Media,
        )
    };
    assert_eq!(
        rows.len(),
        1,
        "one round after the offer, a contended round must still put the \
         holder on the viewer's rail: {rows:?}"
    );
    assert_eq!(rows[0].peer, "LittleMac");
    assert_eq!(rows[0].media_available, Some(1.0));
}

/// A reading that changes with the offer UNTOUCHED reaches the peer's media
/// rail in one round.
///
/// Every test above moves `origins` and watches the reading ride along. Room
/// run 3 moved only the reading: little stamped `media_available=Some(0.0)`
/// at 04:58:23.354 with `origins=[Media]` unchanged, logged `reach ok` to
/// both peers on every one of the ~12 rounds that followed, and
/// `log_sent_snapshot` fired zero times on all three nodes — yet neither peer
/// ever showed the 0.0. `origins` is what `commonwealth_media::offers` keys
/// the ROW on, so a rail that lists the holder throughout hides a stale
/// reading inside a row that never appears or disappears. This is the case
/// neither the suite nor the demo had.
#[tokio::test]
async fn a_reading_that_changes_alone_reaches_a_peers_media_rail_in_one_round() {
    let mesh_id = MeshId::from_u128(45);
    let hash = [14u8; 32];
    let holder = NodeId::from_u128(100);
    let viewer = NodeId::from_u128(200);

    let mesh_holder = Mesh {
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
                holder,
                member_at(holder, "LittleMac", 100, "127.0.0.1:1".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_holder = AppState::new(holder, mesh_holder);
    let addr_holder = spawn_internal_router(state_holder.clone()).await;

    let mesh_viewer = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "T".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: {
            let mut m = HashMap::new();
            m.insert(holder, member_at(holder, "LittleMac", 100, addr_holder));
            m.insert(
                viewer,
                member_at(viewer, "BeefyMac", 150, "127.0.0.1:2".parse().unwrap()),
            );
            m
        },
        peers: vec![],
    };
    let state_viewer = AppState::new(viewer, mesh_viewer);
    let addr_viewer = spawn_internal_router(state_viewer.clone()).await;
    {
        let mut mesh = state_holder.inner.fabric.mesh.write().await;
        mesh.members
            .insert(viewer, member_at(viewer, "BeefyMac", 150, addr_viewer));
    }

    // The offer, and the dial info that carries it. Neither moves again for
    // the rest of this test — the reading is the only thing that changes.
    state_holder.inner.fabric.dial_info.publish(Arc::new(|| {
        commonwealth_core::mesh::IrohDialInfo {
            relay_url: None,
            direct_addrs: Vec::new(),
            origins: vec![commonwealth_core::capabilities::OriginKind::Media],
            media_allow: vec!["BeefyMac".into()],
        }
    }));
    state_holder.update_local_media_available(Some(1.0)).await;

    let round = |state: &AppState| {
        let state = state.clone();
        async move {
            gossip::run_one_round(
                &*state.inner.fabric,
                state.inner.node.corpus_engine.as_ref(),
                &state,
                Duration::from_secs(60),
            )
            .await
            .expect("gossip round should succeed")
        }
    };
    let rail = || async {
        let m = state_viewer.inner.fabric.mesh.read().await;
        commonwealth_media::offers(
            viewer,
            &commonwealth_media::roster_of(&m),
            &[],
            commonwealth_core::capabilities::OriginKind::Media,
        )
    };

    round(&state_holder).await;
    let rows = rail().await;
    assert_eq!(rows.len(), 1, "the offer must be on the rail first: {rows:?}");
    assert_eq!(rows[0].media_available, Some(1.0));

    // The origin is being watched now. `origins` and `media_allow` are
    // untouched, so the row stays — only the reading inside it moves.
    //
    // LWW compares `event_time()` in whole SECONDS, so two rounds inside one
    // second are indistinguishable to the peer and the second is skipped as
    // `LocalRecordNotOlder`.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    state_holder.update_local_media_available(Some(0.0)).await;
    round(&state_holder).await;

    // The holder's own record first, then the peer's copy — a failure here
    // names the stamp, a failure below names the merge.
    {
        let m = state_holder.inner.fabric.mesh.read().await;
        assert_eq!(
            m.members[&holder].capabilities.media_available,
            Some(0.0),
            "the holder must stamp the new reading: {:?}",
            m.members[&holder].capabilities
        );
        assert_eq!(
            m.members[&holder].capabilities.origins,
            vec![commonwealth_core::capabilities::OriginKind::Media],
            "the offer itself must not move"
        );
    }

    let rows = rail().await;
    assert_eq!(
        rows.len(),
        1,
        "the row must still be there — only the reading changed: {rows:?}"
    );
    assert_eq!(rows[0].peer, "LittleMac");
    assert_eq!(
        rows[0].media_available,
        Some(0.0),
        "one round after the reading changed alone, the viewer's rail must \
         read 0.0 — the case room run 3 could not attribute to a side"
    );
}
