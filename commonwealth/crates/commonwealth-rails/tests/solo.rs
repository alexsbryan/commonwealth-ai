// SPDX-License-Identifier: AGPL-3.0-or-later
//! Solo mode (five-programs-63): a data dir with no `mesh.json` starts, its
//! store serves, a local-only row survives a restart, and survives the join
//! that moves the node onto the meshed path.
//!
//! The pump is driven by hand (`project_all_on_disk` / `pump_once`, what
//! `RailsDaemon::run` does at start and every tick) so a restart is a drop and a fresh start, not
//! a timing race against the tick.

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{api, identity, RailsDaemon, RailsNode};

const PRIVATE: &str = "portfolio-private";

fn hermetic() -> Config {
    Config {
        name: "solo-node".to_string(),
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

async fn start(dir: &Path) -> Arc<RailsDaemon> {
    let node = RailsNode::bind(dir.to_path_buf(), hermetic())
        .await
        .expect("node binds");
    Arc::new(RailsDaemon::start_from_disk(node).await.expect("starts"))
}

/// The loopback API on an ephemeral port — the doors `run` serves.
async fn serve(daemon: Arc<RailsDaemon>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, api::router(daemon)).await.unwrap();
    });
    format!("http://{addr}")
}

async fn read_back(daemon: Arc<RailsDaemon>) -> Option<String> {
    daemon.kv.project_all_on_disk().await;
    let base = serve(daemon).await;
    let entry: serde_json::Value = reqwest::get(format!(
        "{base}/v1/mesh/kv/entry?app_id={PRIVATE}&key=holding"
    ))
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .await
    .unwrap();
    let value = entry.get("value")?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .unwrap();
    Some(String::from_utf8(bytes).unwrap())
}

#[tokio::test]
async fn a_meshless_node_serves_its_store_across_a_restart_and_a_join() {
    assert!(commonwealth_rail::is_local_only(PRIVATE));
    let dir = tempfile::tempdir().unwrap();

    // First start: no mesh.json. Solo, self-only, nothing written.
    let first = start(dir.path()).await;
    assert!(first.is_solo(), "no mesh.json must start solo");
    {
        let mesh = first.mesh.read().await;
        assert_eq!(mesh.members.len(), 1);
        assert!(mesh.members.contains_key(&first.node.self_id));
    }
    assert!(
        !identity::mesh_file(dir.path()).exists(),
        "the self-only roster must never reach mesh.json"
    );
    let self_id = first.node.self_id;
    let pubkey = first.node.pubkey();
    let base = serve(first.clone()).await;
    reqwest::Client::new()
        .post(format!("{base}/v1/mesh/kv/entry"))
        .json(&serde_json::json!({
            "app_id": PRIVATE,
            "key": "holding",
            "value": base64::engine::general_purpose::STANDARD.encode("mine"),
            "origin": self_id,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(first.kv.pump_once().await.appended, 1);
    first.node.endpoint.close().await;
    drop(first);

    // Restart, still solo: the row rehydrates from its own journal.
    let second = start(dir.path()).await;
    assert!(second.is_solo());
    assert_eq!(second.node.self_id, self_id, "identity is stable");
    assert_eq!(read_back(second.clone()).await.as_deref(), Some("mine"));
    second.node.endpoint.close().await;
    drop(second);

    // Join: a mesh.json holding this node, as `join_and_persist` leaves it.
    let (mesh, _join_key) = commonwealth_discovery::membership::init_mesh_with_identity(
        "Lab",
        "solo-node",
        Vec::new(),
        self_id,
        Some(pubkey),
        false,
    );
    identity::save_mesh(dir.path(), &mesh).unwrap();

    let third = start(dir.path()).await;
    assert!(!third.is_solo(), "a mesh.json must start meshed");
    assert_eq!(read_back(third.clone()).await.as_deref(), Some("mine"));
    third.node.endpoint.close().await;
}

/// Every `.rs` under `src/`, test code cut away: files named `tests.rs`,
/// anything under a `tests/` dir, and a file's text from its first
/// `#[cfg(test)]` on.
fn product_sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "tests") {
                product_sources(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|n| n != "tests.rs")
        {
            let text = std::fs::read_to_string(&path).unwrap();
            let product = text.split("#[cfg(test)]").next().unwrap_or("").to_string();
            out.push((path.display().to_string(), product));
        }
    }
}

/// The mount trace names every route the API serves (phase-b pb-shell).
///
/// The kit's trace prints each bundle's `routes()`. Here every route a
/// bundle names answers through the kit's `serve` (a router 404 has an
/// empty body; a handler's own 404 does not), and nothing in this crate can
/// serve a route the trace does not name: a route enters a bundle only
/// through `RouteBundle::route`, so the doors around it are a hand-built
/// `Router` and a bare `axum::serve`, and neither may appear in `src/`.
#[tokio::test]
async fn the_mount_trace_names_every_route_the_api_serves() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = start(dir.path()).await;
    let bundles = api::bundles(daemon.clone());
    let named: Vec<(&str, Vec<String>)> = bundles
        .iter()
        .map(|b| (b.name(), b.routes().to_vec()))
        .collect();
    assert_eq!(
        named.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        ["mesh", "membership", "kv", "ledger"]
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let forever = std::future::pending::<()>();
    tokio::spawn(host_kit::shell::serve([listener], bundles, forever));
    let client = reqwest::Client::new();
    for (bundle, routes) in &named {
        assert!(!routes.is_empty(), "{bundle} names no route");
        for route in routes {
            let path: String = route
                .split('/')
                .map(|seg| if seg.starts_with('{') { "x" } else { seg })
                .collect::<Vec<_>>()
                .join("/");
            let sent = client.get(format!("http://{addr}{path}")).send();
            // A long poll that holds the request open is a mounted route.
            let Ok(resp) = tokio::time::timeout(std::time::Duration::from_secs(2), sent).await
            else {
                continue;
            };
            let resp = resp.unwrap();
            let status = resp.status();
            let body = resp.bytes().await.unwrap();
            assert!(
                !(status == 404 && body.is_empty()),
                "{bundle} names {route}, and the router does not serve it"
            );
        }
    }
    daemon.node.endpoint.close().await;

    let mut sources = Vec::new();
    product_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    let outside: Vec<String> = sources
        .iter()
        .flat_map(|(path, text)| {
            text.lines()
                .enumerate()
                .filter(|(_, l)| l.contains("Router::new(") || l.contains("axum::serve("))
                .map(move |(i, l)| format!("{path}:{}: {}", i + 1, l.trim()))
        })
        .collect();
    assert!(
        outside.is_empty(),
        "a route mounted outside the bundle list — build it with \
         host_kit::shell::RouteBundle and serve it with host_kit::shell::serve:\n{}",
        outside.join("\n")
    );
}
