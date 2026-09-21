// SPDX-License-Identifier: AGPL-3.0-or-later
//! One writer for a member's self-stamped clock.
//!
//! Beside `gossip_integration`'s media-rail tests (whose `member_at` and
//! `spawn_internal_router` this module reuses) rather than inside it: that
//! file sits 43 lines under ARCH §3.1's 1200-line ceiling and this case does
//! not fit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use sovereign_daemon::state::AppState;
use sovereign_mesh::gossip;

use crate::gossip_integration::{member_at, spawn_internal_router};

/// A peer's reach must not write the holder's self-stamped clock.
///
/// Room run B (binaries `b20bacb00`) failed the film clause with
/// `listed_s: null` while run A on the same binaries listed in 6.3 s.
/// LittleMac stamped `origins=[Media]` at 07:41:01.913 and beefy read
/// `arm="local-record-not-older"` with `in_event_time == have_event_time`
/// (1789890061 on both sides) every round for 60 s, adopting only once the
/// holder's next second arrived. The tie was manufactured locally: beefy's own
/// reach wrote LittleMac's `last_seen` from beefy's clock in the same second
/// the holder stamped it, and `event_time()` is whole seconds. The comparison
/// is right and the writer was wrong.
///
/// One shared clock with no skew, so the reach and the stamp land in the same
/// second on every run — the race the room hit by accident, made
/// deterministic.
#[tokio::test]
async fn a_peers_reach_does_not_overwrite_the_holders_offer_clock() {
    let mesh_id = MeshId::from_u128(46);
    let hash = [15u8; 32];
    let holder = NodeId::from_u128(101);
    let viewer = NodeId::from_u128(201);

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

    // The viewer's own address is a dead port on purpose: the only gossip path
    // in this test is the viewer PULLING from the holder, which is the path
    // that carries the write under test.
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

    // ONE clock, no skew: both nodes read the same second, so the viewer's
    // reach and the holder's stamp below are simultaneous by construction.
    let clock = commonwealth_core::TestClock::new(1_000);
    state_holder.clock_reader().publish(Arc::new(clock.clone()));
    state_viewer.clock_reader().publish(Arc::new(clock.clone()));

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

    // t=1000: the viewer reaches the holder. Nothing has changed yet — this
    // round exists only to be the peer's observation of a live member.
    round(&state_viewer).await;

    // Still t=1000: the holder publishes an offer and stamps it on its own
    // record, in the same second as the reach above.
    state_holder.inner.fabric.dial_info.publish(Arc::new(|| {
        commonwealth_core::mesh::IrohDialInfo {
            relay_url: None,
            direct_addrs: Vec::new(),
            origins: vec![commonwealth_core::capabilities::OriginKind::Media],
            media_allow: vec!["BeefyMac".into()],
        }
    }));
    state_holder.update_local_media_available(Some(1.0)).await;
    round(&state_holder).await;
    {
        let m = state_holder.inner.fabric.mesh.read().await;
        assert_eq!(
            m.members[&holder].last_seen, 1_000,
            "the holder must stamp the offer in the very second the viewer \
             reached it — otherwise this test is not the race"
        );
        assert_eq!(
            m.members[&holder].capabilities.origins,
            vec![commonwealth_core::capabilities::OriginKind::Media],
            "the holder's own record must carry the offer"
        );
    }

    // The next round. The merge's own line is captured so a failure names the
    // arm and both event times instead of leaving them to a re-run.
    #[derive(Clone)]
    struct BufWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer")
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&self) -> BufWriter {
            self.clone()
        }
    }
    let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(BufWriter(Arc::clone(&buf)))
        .with_ansi(false)
        .finish();
    clock.advance(10);
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        round(&state_viewer).await;
    }
    let captured =
        String::from_utf8(buf.lock().expect("capture buffer").clone()).expect("utf-8 tracing");
    let merge_lines: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains("merge read this member's offer triple"))
        .collect();

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
        "the round after the offer was stamped must put the holder on the \
         viewer's rail. A peer that writes the holder's `last_seen` on its own \
         reach ties the LWW key against the holder's stamp of that second, and \
         the offer waits a whole round. Merge lines:\n{}",
        merge_lines.join("\n")
    );
    assert_eq!(rows[0].peer, "LittleMac");
    assert_eq!(rows[0].media_available, Some(1.0));
}
