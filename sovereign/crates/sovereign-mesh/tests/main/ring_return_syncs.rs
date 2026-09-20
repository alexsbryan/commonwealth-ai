// SPDX-License-Identifier: AGPL-3.0-or-later
//! A peer coming back from Offline carries the ring with it.
//!
//! The failing input is room run 2 of the 2026-09-20 session: the uplink cut
//! ran ~64 s against a 60 s offline threshold, so both sides decayed each
//! other; every ring round after the write skipped the only peer it had
//! (`status != Online`), and the room converged at 85 s — past the bar's 60 s
//! window — on the sync loop's next interval rather than on the return.
//!
//! Beside `ring_append_nudges_sync` rather than inside it: that file's case is
//! "a local write travels now", this one is "a write made while the peer was
//! gone travels when the peer comes back", and the two share no fixture.
//!
//! The interval stays at `DEFAULT_RING_SYNC_INTERVAL` for the same reason it
//! does there — shortening it would make this test pass whether or not the
//! return wakes anything.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{Person, RingRail, RingSigner, Roster};
use ed25519_dalek::SigningKey;
use sovereign_daemon::server::{client_router, internal_router};
use sovereign_daemon::state::AppState;
use sovereign_mesh::gossip;

use crate::common;

const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";
const NS: &str = "house-expenses";

/// The production offline threshold the room runs against.
const OFFLINE_THRESHOLD: Duration = Duration::from_secs(60);

/// What "on the return" is held to. One nudged round is sub-second, so two
/// seconds is ample for it and nowhere near the sixty the interval alone
/// would cost — the same budget `ring_append_nudges_sync` derives.
const BUDGET: Duration = Duration::from_secs(2);

/// One mesh id and invite hash on both sides, so the gossip auth guard in
/// `Mesh::merge_from` admits the round.
const MESH_ID: u128 = 52;
const INVITE_HASH: [u8; 32] = [23u8; 32];

fn mesh_with(members: Vec<commonwealth_core::mesh::MemberRecord>) -> Mesh {
    let mut map = HashMap::new();
    for m in members {
        map.insert(m.node_id, m);
    }
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(MESH_ID),
        name: "the room".into(),
        invite_key_hash: INVITE_HASH,
        invite_version: 0,
        require_encryption: false,
        members: map,
        peers: vec![],
    }
}

/// A daemon with ring storage under `dir`, signing as `key`, on a namespace
/// whose roster file names that key.
fn node(
    dir: &std::path::Path,
    key: &SigningKey,
    self_id: NodeId,
    mesh: Mesh,
) -> (AppState, Arc<RingRail>) {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    rail.journal(NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving_and_node(
        self_id,
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().expect("in-memory MeshStore")),
        Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new()),
        None,
        None,
        sovereign_daemon::state::fabric::FabricSeed {
            ring_rail: Some(Arc::clone(&rail)),
            ..Default::default()
        },
        Default::default(),
        sovereign_daemon::state::node::NodeSeed {
            client_token: Some(Arc::<str>::from(TOKEN)),
            ..Default::default()
        },
    );
    (state, rail)
}

/// How many ops `rail`'s `NS` journal holds on disk.
fn held(rail: &RingRail) -> usize {
    rail.journal(NS).unwrap().read().unwrap().0.len()
}

/// **A write made while the peer was Offline reaches it on the return, not at
/// the next sixty-second tick.**
///
/// Watched RED on 8251f8b61 (and by deleting the one
/// `fabric.ring_write_nudge().notify_one()` in `gossip.rs`'s offline→online
/// block): the peer comes back Online, the gossip round succeeds, and B still
/// holds nothing for the whole budget because the only remaining wake-up is
/// the interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_made_while_the_peer_was_offline_travels_when_it_returns() {
    let (ka, kb) = (
        SigningKey::from_bytes(&[11u8; 32]),
        SigningKey::from_bytes(&[12u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(301), NodeId::from_u128(302));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // B first: A's mesh has to name B's real address once it is reachable.
    let (b_state, b_rail) = node(
        db.path(),
        &kb,
        b_id,
        mesh_with(vec![common::member(
            b_id,
            "beefy",
            "127.0.0.1:2".parse().unwrap(),
        )]),
    );
    let b_addr = common::spawn_router(internal_router(b_state)).await;

    // B starts at a dead port in A's view: the cut, expressed as the only
    // thing a cut actually is here — nothing of B's answers.
    let (a_state, a_rail) = node(
        da.path(),
        &ka,
        a_id,
        mesh_with(vec![
            common::member(a_id, "halo", "127.0.0.1:1".parse().unwrap()),
            common::member(b_id, "beefy", "127.0.0.1:1".parse().unwrap()),
        ]),
    );
    let a_client = common::spawn_router(client_router(a_state.clone())).await;

    // One clock for the decay arithmetic, so "the cut outlasted the threshold"
    // is a fact of the test rather than a wait.
    let clock = commonwealth_core::TestClock::new(1_000);
    a_state.clock_reader().publish(Arc::new(clock.clone()));

    let _sync = sovereign_mesh::ring_sync::spawn_ring_sync_loop(
        a_state.inner.fabric.clone(),
        sovereign_mesh::ring_sync::DEFAULT_RING_SYNC_INTERVAL,
        a_state.ring_write_nudge(),
    );
    // The loop runs one round at start. Let it finish against the dead port,
    // so nothing below can be carried by the boot round.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(held(&b_rail), 0, "control: the boot round carried nothing");

    let round = |state: &AppState| {
        let state = state.clone();
        async move {
            gossip::run_one_round(
                &*state.inner.fabric,
                state.inner.node.corpus_engine.as_ref(),
                &state,
                OFFLINE_THRESHOLD,
            )
            .await
            .expect("gossip round should succeed")
        }
    };

    // t=1000: the round that gives B a local-contact stamp to go stale from.
    round(&a_state).await;
    // t=1061: one second past the threshold — the cut outlasted it, as room
    // run 2's did.
    clock.advance(OFFLINE_THRESHOLD.as_secs() + 1);
    round(&a_state).await;
    {
        let m = a_state.inner.fabric.mesh.read().await;
        assert_eq!(
            m.members[&b_id].status,
            commonwealth_core::mesh::NodeStatus::Offline,
            "the cut must outlast the offline threshold or this test is not \
             the failing input"
        );
    }

    // The local write, made while the peer is gone.
    let http = reqwest::Client::new();
    let appended = http
        .post(format!("http://{a_client}/v1/rail/append?namespace={NS}"))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({
            "op": "record",
            "payload": { "kind": "expense", "amount": 1 },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        appended.status(),
        reqwest::StatusCode::OK,
        "{}",
        appended.text().await.unwrap()
    );
    assert_eq!(held(&a_rail), 1, "the writer holds its own op");

    // The append's own nudge ran a round, and that round had no Online member
    // to exchange with. Control: without it, the assertion below would not be
    // about the return at all.
    tokio::time::sleep(BUDGET).await;
    assert_eq!(
        held(&b_rail),
        0,
        "control: a ring round exchanges only with Online members, so the \
         write cannot have travelled while B was Offline"
    );

    // The return: B answers again.
    {
        let mut m = a_state.inner.fabric.mesh.write().await;
        m.members.get_mut(&b_id).unwrap().addresses = vec![b_addr];
    }
    round(&a_state).await;
    let at = Instant::now();
    {
        let m = a_state.inner.fabric.mesh.read().await;
        assert_eq!(
            m.members[&b_id].status,
            commonwealth_core::mesh::NodeStatus::Online,
            "the round that reaches B must put it back Online"
        );
    }

    while held(&b_rail) == 0 {
        assert!(
            at.elapsed() < BUDGET,
            "B came back Online {}ms ago and still holds nothing — the return \
             did not wake the ring sync, so everything written during the cut \
             waits for the {}s tick",
            at.elapsed().as_millis(),
            sovereign_mesh::ring_sync::DEFAULT_RING_SYNC_INTERVAL.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        held(&b_rail),
        1,
        "the returning peer holds exactly the op written during the cut"
    );
}
