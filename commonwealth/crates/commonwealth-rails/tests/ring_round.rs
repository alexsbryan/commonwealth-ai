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
        .append(act, daemon_a.rail.signer(), &roster, None)
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
