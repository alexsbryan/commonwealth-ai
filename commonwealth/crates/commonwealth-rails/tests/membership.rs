// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every membership verb, through cw-rails' own doors on running nodes
//! (phase-b pb-rails-membership). A founder rotates its invite; a solo node is
//! refused on the old invite and admitted on the new one, then creates a
//! second mesh, switches back, forgets the second, and leaves, and the
//! founder's roster drops it. No inference daemon exists anywhere here.
//!
//! Hermetic like `found_and_join.rs`: `discovery = "none"`, no relay URLs.

use std::time::{Duration, Instant};

use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::{found, identity, RailsDaemon, RailsNode};
use serde_json::{json, Value};

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

/// GET a node's status until `pick` finds something, or fail naming it.
async fn poll_status<T>(port: u16, what: &str, pick: impl Fn(&Value) -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Ok(r) = reqwest::get(format!("http://127.0.0.1:{port}/v1/mesh/status")).await {
            if let Ok(doc) = r.json::<Value>().await {
                if let Some(v) = pick(&doc) {
                    return v;
                }
                assert!(started.elapsed() < BUDGET, "{what} never appeared: {doc}");
            }
        }
        assert!(
            started.elapsed() < BUDGET,
            "status on :{port} never answered"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn post(port: u16, path: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {path} got no answer: {e}"));
    let code = r.status().as_u16();
    (code, r.json().await.unwrap_or(Value::Null))
}

async fn solo(port: u16) -> bool {
    poll_status(port, "the known-mesh list", |d| d["meshes"].as_array().map(Vec::is_empty)).await
}

fn active_mesh(doc: &Value) -> Option<String> {
    doc["meshes"]
        .as_array()?
        .iter()
        .find(|m| m["is_active"] == true)
        .and_then(|m| m["name"].as_str().map(str::to_string))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_membership_verb_answers_through_cw_rails_alone() {
    let founder_dir = tempfile::tempdir().unwrap();
    let node_dir = tempfile::tempdir().unwrap();
    let founded = found::found(founder_dir.path(), "Lab", "founder").expect("founds");
    let (fport, nport) = (free_port(), free_port());
    let founder = RailsDaemon::start_from_disk(
        RailsNode::bind(founder_dir.path().to_path_buf(), hermetic("founder", fport))
            .await
            .expect("the founder binds"),
    )
    .await
    .expect("the founder starts");
    let node = RailsDaemon::start_from_disk(
        RailsNode::bind(node_dir.path().to_path_buf(), hermetic("member", nport))
            .await
            .expect("the node binds"),
    )
    .await
    .expect("the node starts");
    assert!(node.is_solo(), "no mesh.json starts solo");
    // `run` takes the daemon, so from here on the status doors say whether
    // it is solo: no known mesh at all.
    let node_id = node.node.self_id.to_string();

    let script = async {
        let old = poll_status(fport, "the founder's join_link", |d| {
            d["join_link"].as_str().map(str::to_string)
        })
        .await;

        // rotate: a new key, a 24-hour expiry, and the old key opens nothing.
        let (code, rotated) = post(fport, "/v1/mesh/rotate", json!({})).await;
        assert_eq!(code, 200, "{rotated}");
        let new = rotated["join_link"].as_str().expect("a link").to_string();
        assert_ne!(rotated["join_key"], json!(founded.join_key));
        let expires = rotated["expires_at"]
            .as_u64()
            .expect("an encrypted mesh's invite expires");
        let now = commonwealth_core::clock::unix_now_secs();
        assert!(
            expires > now + 23 * 3600 && expires <= now + 24 * 3600,
            "{expires}"
        );

        let (code, refused) = post(nport, "/v1/mesh/join", json!({ "key_or_url": old })).await;
        assert_eq!(
            code, 403,
            "the rotated-out invite must be refused: {refused}"
        );
        assert!(solo(nport).await, "a refused join leaves the node as it was");

        // preview reads the invite without joining.
        let (code, preview) = post(nport, "/v1/mesh/join/preview", json!({ "link": new })).await;
        assert_eq!(
            (code, &preview["encrypted"]),
            (200, &json!(true)),
            "{preview}"
        );
        assert!(solo(nport).await);

        // join: admitted on the new invite, by the running endpoint.
        let (code, joined) = post(nport, "/v1/mesh/join", json!({ "key_or_url": new })).await;
        assert_eq!(code, 200, "{joined}");
        assert_eq!(joined["node_id"], json!(node_id));
        assert!(!solo(nport).await);
        poll_status(fport, "the member on the founder's roster", |d| {
            d["members"]
                .as_array()?
                .iter()
                .find(|m| m["node_id"] == json!(node_id))
                .cloned()
        })
        .await;

        // create: the joined mesh is parked, the new one active.
        let (code, created) = post(nport, "/v1/mesh/create", json!({ "name": "Second" })).await;
        assert_eq!(code, 200, "{created}");
        assert!(
            created["join_link"]
                .as_str()
                .is_some_and(|l| l.contains("iroh=")),
            "{created}"
        );
        let doc = poll_status(nport, "two known meshes", |d| {
            (d["meshes"].as_array()?.len() == 2).then(|| d.clone())
        })
        .await;
        assert_eq!(active_mesh(&doc).as_deref(), Some("Second"));

        // switch back, by name; switching to the active one is refused.
        let (code, body) = post(nport, "/v1/mesh/switch", json!({ "mesh": "lab" })).await;
        assert_eq!(code, 200, "{body}");
        let (code, body) = post(nport, "/v1/mesh/switch", json!({ "mesh": "Lab" })).await;
        assert_eq!(code, 409, "{body}");
        assert_eq!(
            identity::load_mesh(node_dir.path()).unwrap().unwrap().id,
            founded.mesh.id,
            "a restart comes up on the mesh the switch chose"
        );

        // forget: the parked one goes; the active one and a stranger are refused.
        let (code, body) = post(nport, "/v1/mesh/forget", json!({ "mesh": "Lab" })).await;
        assert_eq!(code, 409, "{body}");
        let (code, body) = post(nport, "/v1/mesh/forget", json!({ "mesh": "nope" })).await;
        assert_eq!(code, 404, "{body}");
        let (code, body) = post(nport, "/v1/mesh/forget", json!({ "mesh": "Second" })).await;
        assert_eq!(code, 200, "{body}");
        let doc = poll_status(nport, "one known mesh", |d| {
            (d["meshes"].as_array()?.len() == 1).then(|| d.clone())
        })
        .await;
        assert_eq!(active_mesh(&doc).as_deref(), Some("Lab"));

        // leave: the founder's roster drops the member, and the node runs solo.
        let (code, body) = post(nport, "/v1/mesh/leave", json!({})).await;
        assert_eq!(code, 200, "{body}");
        assert!(solo(nport).await);
        assert!(
            identity::load_mesh(node_dir.path()).unwrap().is_none(),
            "a restart is solo"
        );
        poll_status(fport, "the member's tombstone on the founder", |d| {
            let gone = !d["members"]
                .as_array()?
                .iter()
                .any(|m| m["node_id"] == json!(node_id));
            gone.then_some(())
        })
        .await;
        let (code, body) = post(nport, "/v1/mesh/leave", json!({})).await;
        assert_eq!(code, 409, "{body}");
    };
    tokio::select! {
        exit = founder.run() => panic!("the founder stopped serving: {exit:?}"),
        exit = node.run() => panic!("the node stopped serving: {exit:?}"),
        () = script => {}
    }
}
