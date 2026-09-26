// SPDX-License-Identifier: AGPL-3.0-or-later
//! A mesh grown by cw-rails alone: one founds it, a second joins by the
//! invite the first serves, and the joiner appears on the founder's roster.
//! No inference daemon exists anywhere in this test (phase-b pb-membership).
//!
//! Hermetic like `two_daemons.rs`: `discovery = "none"` with no relay URLs, so
//! the invite's dial string carries direct addresses only and no packet leaves
//! this machine.

use std::time::{Duration, Instant};

use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{found, join, RailsDaemon, RailsNode};

const BUDGET: Duration = Duration::from_secs(60);

fn hermetic(name: &str, listen: u16) -> Config {
    Config {
        name: name.to_string(),
        listen,
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Poll the founder's status until `pick` finds something, or fail naming
/// what never appeared.
async fn poll_status<T>(
    port: u16,
    what: &str,
    pick: impl Fn(&serde_json::Value) -> Option<T>,
) -> T {
    let client = reqwest::Client::new();
    let started = Instant::now();
    loop {
        if let Ok(r) = client
            .get(format!("http://127.0.0.1:{port}/v1/mesh/status"))
            .send()
            .await
        {
            if let Ok(doc) = r.json::<serde_json::Value>().await {
                if let Some(v) = pick(&doc) {
                    return v;
                }
                assert!(
                    started.elapsed() < BUDGET,
                    "{what} never appeared on the founder's status: {doc}"
                );
            }
        }
        assert!(
            started.elapsed() < BUDGET,
            "the founder's status never answered"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cw_rails_founds_a_mesh_and_a_second_joins_it_by_invite() {
    let founder_dir = tempfile::tempdir().unwrap();
    let joiner_dir = tempfile::tempdir().unwrap();

    let founded = found::found(founder_dir.path(), "Lab", "founder").expect("an empty root founds");
    let port = free_port();
    let node = RailsNode::bind(founder_dir.path().to_path_buf(), hermetic("founder", port))
        .await
        .expect("the founder binds");
    let daemon = RailsDaemon::start_from_disk(node)
        .await
        .expect("the founder starts");
    assert!(!daemon.solo, "a founded root starts meshed");
    // `run` is not `Send`, so it is driven here beside the joiner rather
    // than spawned (the same shape as `ready.rs`).
    let joiner_side = async {
        // The invite is served, with the daemon's field name, once the endpoint
        // has a direct address to put in it.
        let invite = poll_status(port, "join_link", |doc| {
            doc["join_link"].as_str().map(str::to_string)
        })
        .await;
        assert!(
            invite.contains("iroh="),
            "a cw-rails mesh is encrypted: {invite}"
        );

        let joiner = RailsNode::bind(
            joiner_dir.path().to_path_buf(),
            hermetic("joiner", free_port()),
        )
        .await
        .expect("the joiner binds");
        let joined = join::join_and_persist(&joiner, &invite, joiner_dir.path())
            .await
            .expect("the founder admits the joiner");
        assert_eq!(
            joined.mesh.id, founded.mesh.id,
            "the joiner holds the founder's mesh"
        );
        assert_eq!(joined.mesh.members.len(), 2);

        let id = joined.self_id.to_string();
        let row = poll_status(port, "the joiner's roster row", |doc| {
            doc["members"]
                .as_array()?
                .iter()
                .find(|m| m["node_id"].as_str() == Some(id.as_str()))
                .cloned()
        })
        .await;
        assert_eq!(row["name"], "joiner");
        assert_eq!(row["is_self"], false);

        joiner.endpoint.close().await;
    };
    tokio::select! {
        exit = daemon.run() => panic!("the founder stopped serving: {exit:?}"),
        () = joiner_side => {}
    }
}
