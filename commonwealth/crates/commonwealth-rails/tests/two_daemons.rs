// SPDX-License-Identifier: AGPL-3.0-or-later
//! Two rails daemons, two real iroh endpoints, one gossip round.
//!
//! # Why the mesh is built rather than joined
//!
//! This daemon does not admit joiners — there is no `/internal/join` in it —
//! so a two-process test cannot found a mesh the way `sovereign-mesh`'s
//! `gossip_integration` does. What it CAN do, and what matters, is start both
//! from the same `Mesh` snapshot (which is exactly what a founder's
//! `JoinResponse` hands each of them) and prove that a round over the real
//! transport converges. Everything under test after that line is the same
//! code path a live join produces.
//!
//! # Hermetic on purpose
//!
//! `discovery = "none"` with no relay URLs builds the endpoints from iroh's
//! `Minimal` preset: no n0 relay, no n0 DNS, no packet leaves this machine.
//! The two endpoints reach each other on their own gossiped direct addresses,
//! which is the path a LAN pair takes anyway.

use std::time::Duration;

use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{gossip, RailsDaemon, RailsNode};

/// How long an endpoint is given to report at least one direct address. It is
/// a local bind, so this is a generous bound on a fast operation rather than
/// a guess at a network.
const ADDR_BUDGET: Duration = Duration::from_secs(20);

fn hermetic(name: &str, media_origin: Option<std::net::SocketAddr>) -> Config {
    Config {
        name: name.to_string(),
        // Never bound in this test: only `RailsDaemon::run` binds the API,
        // and these daemons are driven a round at a time.
        listen: 0xFFFF,
        relay: RelaySection {
            urls: Vec::new(),
            discovery: Some("none".to_string()),
        },
        media: MediaSection {
            origin: media_origin,
            allow: Vec::new(),
        },
        gossip_interval_secs: 1,
        offline_threshold_secs: 60,
    }
}

/// Wait until the endpoint has a direct address to gossip. Without one there
/// is nothing for the peer to dial and the round would fail for a reason that
/// has nothing to do with what is under test.
async fn wait_for_addrs(node: &RailsNode) -> Vec<std::net::SocketAddr> {
    let deadline = std::time::Instant::now() + ADDR_BUDGET;
    loop {
        let addrs: Vec<_> = node.endpoint.addr().ip_addrs().copied().collect();
        if !addrs.is_empty() {
            return addrs;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{} reported no direct address within {ADDR_BUDGET:?} — the endpoint never homed, \
             so nothing about gossip was measured",
            node.config.name
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn record(node: &RailsNode, addrs: Vec<std::net::SocketAddr>, offers: bool) -> MemberRecord {
    MemberRecord {
        node_id: node.self_id,
        name: node.config.name.clone(),
        invited_by: node.self_id,
        joined_at: 1,
        last_seen: 1,
        status: NodeStatus::Online,
        capabilities: gossip::minimal_capabilities(
            1,
            if offers { &[OriginKind::Media] } else { &[] },
        ),
        addresses: Vec::new(),
        node_pubkey: Some(node.pubkey()),
        relay_url: None,
        iroh_direct_addrs: addrs,
        dial_info_version: 0,
        dial_info_sig: None,
        removed_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_round_converges_two_daemons_and_carries_the_media_offer() {
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");

    // `a` declares a media origin; `b` declares none. Whether the offer
    // crosses is the second thing this test is for.
    let origin: std::net::SocketAddr = "127.0.0.1:8096".parse().unwrap();
    let a = RailsNode::bind(dir_a.path().to_path_buf(), hermetic("alpha", Some(origin)))
        .await
        .expect("alpha binds");
    let b = RailsNode::bind(dir_b.path().to_path_buf(), hermetic("beta", None))
        .await
        .expect("beta binds");
    assert_ne!(a.self_id, b.self_id, "two data dirs, two identities");

    let addrs_a = wait_for_addrs(&a).await;
    let addrs_b = wait_for_addrs(&b).await;

    // The snapshot a founder's JoinResponse would have handed both of them:
    // one mesh id, one invite hash, one secret, two members.
    let (mut mesh, _invite) =
        commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
    mesh.members.clear();
    let rec_a = record(&a, addrs_a, true);
    let rec_b = record(&b, addrs_b, false);
    mesh.members.insert(a.self_id, rec_a);
    mesh.members.insert(b.self_id, rec_b);

    let id_a = a.self_id;
    let id_b = b.self_id;
    let daemon_a = RailsDaemon::start(a, mesh.clone())
        .await
        .expect("alpha starts");
    let daemon_b = RailsDaemon::start(b, mesh).await.expect("beta starts");

    // Beta stamps itself first so alpha has something to observe changing.
    gossip::run_one_round(&daemon_b, 0).await;

    // Alpha's round: self-stamp, pick beta, dial by key, merge the reply.
    gossip::run_one_round(&daemon_a, 0).await;

    // Alpha reached beta: the contact map is the local-clock evidence, and
    // the whole decay pass is built on it.
    assert!(
        daemon_a.contacts.lock().await.contains_key(&id_b),
        "alpha never had contact with beta — the round did not complete"
    );

    // Beta learned alpha's stamp through the inbound half: its identity key,
    // its reachability, and its MEDIA OFFER. Before the offer rode gossip a
    // shim had to dial every peer and read the refusal to find a library.
    let seen_by_b = {
        let mesh = daemon_b.mesh.read().await;
        mesh.members
            .get(&id_a)
            .cloned()
            .expect("alpha is on beta's roster")
    };
    assert!(
        seen_by_b.last_seen > 1,
        "beta still holds alpha's pre-round last_seen — nothing merged"
    );
    assert_eq!(seen_by_b.status, NodeStatus::Online);
    assert_eq!(
        seen_by_b.capabilities.origins,
        vec![OriginKind::Media],
        "the media offer must cross the wire — it is what `mesh media` lists"
    );
    assert!(
        seen_by_b.dial_info_sig.is_some(),
        "alpha's reachability rides signed"
    );

    // And the catalogue reads it. Beta, which offers nothing itself, sees
    // exactly one offering member.
    let roster = daemon_b.roster().await;
    let offers = commonwealth_media::offers(id_b, &roster, &[]);
    assert_eq!(offers.len(), 1, "beta sees one library");
    assert_eq!(offers[0].peer, "alpha");

    // Alpha, which offers one, sees none: its own origin is already local.
    let roster_a = daemon_a.roster().await;
    assert!(
        commonwealth_media::offers(id_a, &roster_a, &[]).is_empty(),
        "a node never lists itself as a library to reach over the mesh"
    );

    // The mesh was persisted, so a restart resumes with this roster rather
    // than the one the join wrote. Read through the crate's OWN reader: a
    // second decoder here would be a second answer to what `mesh.json` is,
    // and it is exactly the question that was got wrong first (see
    // `identity`'s module docs on why the file is a `MeshWire`).
    let persisted = commonwealth_rails::identity::load_mesh(dir_b.path())
        .expect("beta persisted")
        .expect("a mesh on disk");
    assert_eq!(persisted.members.len(), 2);
    assert_ne!(
        persisted.mesh_secret, [0u8; 32],
        "a redacted secret on disk is a node that boots unable to prove membership"
    );
}
