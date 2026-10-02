// SPDX-License-Identifier: AGPL-3.0-or-later
//! cw-rails heals its own endpoint (phase-b pb-rails-parity): the watchdog
//! over a rails daemon's endpoint sees a peer path it held go away and
//! rebuilds the endpoint in-process, and the daemon then serves on the fresh
//! one — the self-heal the inference daemon runs over its own endpoint.
//!
//! Hermetic the way `ring_round.rs` is: `discovery = "none"`, no relay, so
//! the peer-path term is the only one that can move (iroh_watchdog.rs:24-41;
//! relay-home and the self-discovery probe are off for a relay-less node).

use std::time::Duration;

use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::iroh_watchdog::WatchdogConfig;
use commonwealth_rails::{gossip, self_heal, RailsDaemon, RailsNode};

const ADDR_BUDGET: Duration = Duration::from_secs(20);
/// iroh 1.0.2 keeps a remote's path `Open` after its last connection closes
/// (only the connection's own path state is closed); the record goes when
/// the remote's actor has idled for `ACTOR_MAX_IDLE_TIMEOUT` (60 s,
/// socket/remote_map/remote_state.rs:73), and the peer-path term reads that
/// as the loss. So a loss is visible about a minute after the peer goes.
const HEAL_BUDGET: Duration = Duration::from_secs(120);

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
            "{} reported no direct address within {ADDR_BUDGET:?} — nothing about the self-heal was measured",
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

/// Poll alpha's watchdog status until `done` holds, or fail naming `what`.
async fn until(
    daemon: &RailsDaemon,
    what: &str,
    done: impl Fn(&commonwealth_rails::iroh_watchdog::ReachabilityStatus) -> bool,
) -> commonwealth_rails::iroh_watchdog::ReachabilityStatus {
    let deadline = std::time::Instant::now() + HEAL_BUDGET;
    loop {
        let status = daemon
            .self_reachability()
            .await
            .expect("the watchdog was spawned, so its status is present");
        if done(&status) {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what} within {HEAL_BUDGET:?}; last status: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rails_daemon_rebuilds_its_endpoint_after_a_peer_path_it_held_is_lost() {
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
    let daemon_a = std::sync::Arc::new(
        RailsDaemon::start(a, mesh.clone())
            .await
            .expect("alpha starts"),
    );
    let daemon_b = RailsDaemon::start(b, mesh).await.expect("beta starts");
    assert!(
        daemon_a.self_reachability().await.is_none(),
        "control: no watchdog, no status"
    );

    // Fast enough to watch: one-second-scale detection, no cooldown.
    let cfg = WatchdogConfig {
        health_poll: Duration::from_millis(200),
        unhealthy_grace: Duration::from_millis(300),
        rebuild_cooldown: Duration::ZERO,
        peer_path_bad_streak: 2,
        self_probe: false,
        relays_expected: false,
        ..WatchdogConfig::default()
    };
    let first = daemon_a.endpoint();
    let _watchdog = self_heal::spawn_watchdog(daemon_a.clone(), cfg);

    // Alpha holds a path to beta: its gossip round dials beta by key.
    gossip::run_one_round(&daemon_a, 1).await;
    until(
        &daemon_a,
        "alpha's watchdog saw an active path to beta",
        |s| s.peer_paths_active >= 1,
    )
    .await;

    // Beta goes away. The path alpha held is lost, and nothing else can move
    // the watchdog on a relay-less node.
    daemon_b.endpoint().close().await;
    drop(daemon_b);
    let healed = until(&daemon_a, "alpha's watchdog rebuilt its endpoint", |s| {
        s.rebuilds >= 1
    })
    .await;
    assert_eq!(
        healed.last_recovery.as_ref().map(|r| r.action.as_str()),
        Some("endpoint_rebuild"),
        "the recovery on record is the rebuild: {healed:?}"
    );

    // The daemon serves on the fresh endpoint, under the same key, and the
    // one it replaced is closed.
    let now = daemon_a.endpoint();
    assert!(first.is_closed(), "the wedged endpoint is closed");
    assert!(!now.is_closed(), "the fresh endpoint is open");
    assert_eq!(now.id(), first.id(), "a rebuild keeps the node key");
}
