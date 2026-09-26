// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ready means projected (phase-b-5): the first read after `/v1/mesh/status`
//! first answers finds a row the journals hold, on every one of 20 starts.
//! The journal is large enough that projecting it takes measurable time, so
//! a listener that served before the projection would read absent.

use std::path::Path;
use std::time::{Duration, Instant};

use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{RailsDaemon, RailsNode};

const NAMESPACE: &str = "ready-proof";
const SENTINEL: &str = "sentinel";
const ROWS: usize = 1000;
const STARTS: usize = 20;

fn hermetic(listen: u16) -> Config {
    Config {
        name: "ready-node".to_string(),
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

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
}

/// Write the sentinel and `ROWS` filler rows, and drain them onto the journal.
async fn seed(dir: &Path) {
    let node = RailsNode::bind(dir.to_path_buf(), hermetic(free_port()))
        .await
        .expect("node binds");
    let daemon = RailsDaemon::start_from_disk(node).await.expect("starts");
    let origin = daemon.node.self_id;
    let set = |key: &str| {
        daemon
            .kv
            .store
            .set(NAMESPACE, key, bytes::Bytes::from(vec![b'x'; 256]), origin)
            .expect("set")
    };
    set(SENTINEL);
    for i in 0..ROWS {
        set(&format!("filler-{i:05}"));
    }
    let mut appended = 0;
    loop {
        let n = daemon.kv.pump_once().await.appended;
        if n == 0 {
            break;
        }
        appended += n;
    }
    assert_eq!(appended, ROWS + 1, "every seeded row reached the journal");
    daemon.node.endpoint.close().await;
}

/// Start `run` on `dir`, wait for the status path, then read the sentinel
/// once. Returns whether the read found it, and how long status took.
async fn first_read(dir: &Path) -> (bool, Duration) {
    let port = free_port();
    let node = RailsNode::bind(dir.to_path_buf(), hermetic(port))
        .await
        .expect("node binds");
    let daemon = RailsDaemon::start_from_disk(node).await.expect("starts");
    let started = Instant::now();
    // `run` is not `Send`, so it is polled alongside the probe, not spawned.
    tokio::select! {
        exit = daemon.run() => panic!("run returned before the probe: {exit:?}"),
        found = probe(port, started) => found,
    }
}

async fn probe(port: u16, started: Instant) -> (bool, Duration) {
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    loop {
        let answered = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if answered {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "status never answered"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let to_status = started.elapsed();
    let entry: serde_json::Value = client
        .get(format!(
            "{base}/v1/mesh/kv/entry?app_id={NAMESPACE}&key={SENTINEL}"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    (entry.get("value").is_some(), to_status)
}

#[test]
fn the_first_read_after_status_answers_finds_the_journals_rows() {
    let dir = tempfile::tempdir().unwrap();
    runtime().block_on(seed(dir.path()));
    for start in 0..STARTS {
        // A runtime per start: dropping it ends `run` and every task it spawned.
        let rt = runtime();
        let (found, to_status) = rt.block_on(first_read(dir.path()));
        rt.shutdown_timeout(Duration::from_secs(5));
        assert!(
            found,
            "start {start}: status answered after {to_status:?} but the sentinel read absent"
        );
    }
}
