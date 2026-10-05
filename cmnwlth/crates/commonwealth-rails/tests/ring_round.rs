// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails replicates a ring by itself (phase-b pb-rails-parity): an act
//! signed on one rails daemon reaches the other through cw-rails' own round
//! and its own `/internal/ring/sync` route — no inference daemon anywhere.
//!
//! Hermetic the way `two_daemons.rs` is: `discovery = "none"`, no relay, the
//! two endpoints reach each other on their own direct addresses, and both
//! start from the snapshot a founder's join would have handed them.

use std::time::Duration;

use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_rail::{RailAct, RingRail};
use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{gossip, ring_sync, RailsDaemon, RailsNode};

const ADDR_BUDGET: Duration = Duration::from_secs(20);
const RING: &str = "parity-proof";

fn hermetic(name: &str) -> Config {
    Config {
        name: name.to_string(),
        listen: 0xFFFF,
        relay: RelaySection {
            urls: Vec::new(),
            discovery: Some("none".to_string()),
        },
        media: MediaSection {
            origin: None,
            allow: Vec::new(),
        },
        gossip_interval_secs: 1,
        offline_threshold_secs: 60,
        work_offer: Default::default(),
        source: None,
    }
}

async fn wait_for_addrs(node: &RailsNode) -> Vec<std::net::SocketAddr> {
    let deadline = std::time::Instant::now() + ADDR_BUDGET;
    loop {
        let addrs: Vec<_> = node.endpoint.addr().ip_addrs().copied().collect();
        if !addrs.is_empty() {
            return addrs;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{} reported no direct address within {ADDR_BUDGET:?} — nothing about the round was measured",
            node.config.name
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn record(node: &RailsNode, addrs: Vec<std::net::SocketAddr>) -> MemberRecord {
    MemberRecord {
        node_id: node.self_id,
        name: node.config.name.clone(),
        invited_by: node.self_id,
        joined_at: 1,
        last_seen: 1,
        status: NodeStatus::Online,
        capabilities: gossip::minimal_capabilities(1, &[], None),
        addresses: Vec::new(),
        node_pubkey: Some(node.pubkey()),
        relay_url: None,
        iroh_direct_addrs: addrs,
        dial_info_version: 0,
        dial_info_sig: None,
        removed_at: None,
    }
}

/// How many ops `rail` holds for [`RING`], read off disk.
fn held(rail: &RingRail) -> usize {
    match rail.namespaces() {
        Ok(names) if names.iter().any(|n| n == RING) => rail
            .journal(RING)
            .expect("journal")
            .read()
            .expect("read")
            .0
            .len(),
        _ => 0,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_act_signed_on_one_rails_daemon_reaches_the_other_through_its_own_round() {
    let dir_a = tempfile::tempdir().expect("tempdir");
    let dir_b = tempfile::tempdir().expect("tempdir");
    let a = RailsNode::bind(dir_a.path().to_path_buf(), hermetic("alpha"))
        .await
        .expect("alpha binds");
    let b = RailsNode::bind(dir_b.path().to_path_buf(), hermetic("beta"))
        .await
        .expect("beta binds");
    let addrs_a = wait_for_addrs(&a).await;
    let addrs_b = wait_for_addrs(&b).await;

    let (mut mesh, _invite) =
        commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
    mesh.members.clear();
    mesh.members.insert(a.self_id, record(&a, addrs_a));
    mesh.members.insert(b.self_id, record(&b, addrs_b));
    let daemon_a = RailsDaemon::start(a, mesh.clone())
        .await
        .expect("alpha starts");
    let daemon_b = RailsDaemon::start(b, mesh).await.expect("beta starts");

    // Alpha signs one act onto a ring whose roster is the membership.
    let journal = daemon_a.rail.journal(RING).expect("alpha's journal");
    let roster = daemon_a
        .rail
        .roster(&journal)
        .await
        .expect("alpha's roster");
    let act = RailAct::from_json(serde_json::json!({ "op": "record", "payload": { "n": 1 } }))
        .expect("an act");
    journal
        .append(
            act,
            daemon_a.rail.signer(),
            &roster,
            None,
            &commonwealth_rail::Ed25519Verifier,
        )
        .expect("alpha signs");
    assert_eq!(held(&daemon_a.rail), 1, "control: alpha holds its act");
    assert_eq!(held(&daemon_b.rail), 0, "control: beta holds nothing yet");

    // Alpha's round: offer the ring to beta, whose route checks alpha's
    // stamped key against the ring's roster and ingests.
    let outcome = ring_sync::run_one_round(&daemon_a).await;
    assert_eq!(
        (outcome.peers_reached, outcome.ops_pushed),
        (1, 1),
        "alpha's round must reach beta and push its one act: {outcome:?}"
    );
    assert_eq!(
        held(&daemon_b.rail),
        1,
        "beta holds alpha's act, carried by cw-rails' round alone"
    );
}

/// How long a nudged round may take to carry one act to the peer: the
/// interval is sixty seconds, so an act inside this was carried by a nudge.
const NUDGE_BUDGET: Duration = Duration::from_secs(2);

/// Two daemons on one mesh, alpha's ring loop running at the sixty-second
/// interval, and alpha's loopback API served. Returns alpha, beta, alpha's
/// API address, and the loop's handle.
async fn two_with_alpha_looping(
    dirs: &(tempfile::TempDir, tempfile::TempDir),
) -> (
    std::sync::Arc<RailsDaemon>,
    RailsDaemon,
    std::net::SocketAddr,
    ring_sync::RingSyncHandle,
) {
    let a = RailsNode::bind(dirs.0.path().to_path_buf(), hermetic("alpha"))
        .await
        .expect("alpha binds");
    let b = RailsNode::bind(dirs.1.path().to_path_buf(), hermetic("beta"))
        .await
        .expect("beta binds");
    let addrs_a = wait_for_addrs(&a).await;
    let addrs_b = wait_for_addrs(&b).await;
    let (mut mesh, _invite) =
        commonwealth_discovery::membership::init_mesh("Lab", "founder", Vec::new());
    mesh.members.clear();
    mesh.members.insert(a.self_id, record(&a, addrs_a));
    mesh.members.insert(b.self_id, record(&b, addrs_b));
    let daemon_a = std::sync::Arc::new(
        RailsDaemon::start(a, mesh.clone())
            .await
            .expect("alpha starts"),
    );
    let daemon_b = RailsDaemon::start(b, mesh).await.expect("beta starts");
    let looping = ring_sync::spawn_ring_sync_loop(
        daemon_a.clone(),
        ring_sync::DEFAULT_RING_SYNC_INTERVAL,
        daemon_a.ring_nudge.clone(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let api = listener.local_addr().expect("its address");
    let router = commonwealth_rails::api::router(daemon_a.clone());
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    // The loop's boot round runs at once and carries nothing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    (daemon_a, daemon_b, api, looping)
}

/// `POST /v1/rail/append` on alpha's loopback door.
async fn append_over_http(api: std::net::SocketAddr) {
    let appended = reqwest::Client::new()
        .post(format!("http://{api}/v1/rail/append?namespace={RING}"))
        .json(&serde_json::json!({ "op": "record", "payload": { "kind": "doc-change" } }))
        .send()
        .await
        .expect("alpha's append door answers");
    let status = appended.status();
    assert!(
        status.is_success(),
        "{status}: {}",
        appended.text().await.unwrap_or_default()
    );
}

/// Wait until beta holds one act, inside [`NUDGE_BUDGET`] from `at`.
async fn beta_holds_it_within_the_budget(beta: &RailsDaemon, at: std::time::Instant, why: &str) {
    while held(&beta.rail) == 0 {
        assert!(
            at.elapsed() < NUDGE_BUDGET,
            "beta still holds nothing {}ms later — {why}, so the act waits for the \
             {}s tick",
            at.elapsed().as_millis(),
            ring_sync::DEFAULT_RING_SYNC_INTERVAL.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(held(&beta.rail), 1, "beta holds exactly the one act");
}

/// The daemon's `ring_append_nudges_sync`, on cw-rails: an act appended over
/// the loopback door wakes the round, so the peer holds it within two
/// seconds rather than at the sixty-second tick. Failing input: the append
/// door's `notify_one` removed (`ring_sync::append`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_act_appended_over_http_is_held_by_the_peer_within_two_seconds() {
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_alpha, beta, api, _loop) = two_with_alpha_looping(&dirs).await;
    assert_eq!(
        held(&beta.rail),
        0,
        "control: the boot round carried nothing"
    );
    append_over_http(api).await;
    beta_holds_it_within_the_budget(&beta, std::time::Instant::now(), "the append did not nudge")
        .await;
}

/// The daemon's `ring_return_syncs`, on cw-rails: a write made while a peer
/// was Offline is not offered to it (the round exchanges with Online members
/// only), and travels within two seconds of the gossip merge that brings
/// that peer back Online (`gossip::merge_waking_ring`). Failing input: the
/// merge's nudge removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_made_while_the_peer_was_offline_travels_when_it_returns() {
    let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (alpha, beta, api, _loop) = two_with_alpha_looping(&dirs).await;
    let beta_id = beta.node.self_id;
    alpha
        .mesh
        .write()
        .await
        .members
        .get_mut(&beta_id)
        .expect("beta on alpha's roster")
        .status = NodeStatus::Offline;
    append_over_http(api).await;
    tokio::time::sleep(NUDGE_BUDGET).await;
    assert_eq!(
        held(&beta.rail),
        0,
        "control: the round exchanges with Online members only, so the write \
         cannot have travelled while beta was Offline"
    );

    // Beta's own round stamps its row and gossips it to alpha, whose inbound
    // merge brings beta back Online.
    gossip::run_one_round(&beta, 1).await;
    let at = std::time::Instant::now();
    assert_eq!(
        alpha.mesh.read().await.members[&beta_id].status,
        NodeStatus::Online,
        "beta's round must bring it back Online on alpha, or this is not the case"
    );
    beta_holds_it_within_the_budget(&beta, at, "the return did not wake the ring round").await;
}
