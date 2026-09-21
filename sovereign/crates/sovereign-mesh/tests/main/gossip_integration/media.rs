// SPDX-License-Identifier: AGPL-3.0-or-later
//! Media-rail offers and readings reaching a peer through gossip.
//!
//! Split out of `gossip_integration.rs`, which the media work pushed into
//! the 800-1200 approach band (ARCH §3.1). `use super::*` keeps the
//! fixtures shared.

use super::*;

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
    assert_eq!(
        rows.len(),
        1,
        "the offer must be on the rail first: {rows:?}"
    );
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
