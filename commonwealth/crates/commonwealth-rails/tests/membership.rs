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
        work_offer: Default::default(),
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
    poll_status(port, "the known-mesh list", |d| {
        d["meshes"].as_array().map(Vec::is_empty)
    })
    .await
}

fn active_mesh(doc: &Value) -> Option<String> {
    doc["meshes"]
        .as_array()?
        .iter()
        .find(|m| m["is_active"] == true)
        .and_then(|m| m["name"].as_str().map(str::to_string))
}

/// Every membership verb, on two cw-rails: rotate, join, preview, create,
/// switch, forget and leave, with the parked-membership guarantees the
/// daemon's `mesh_switch` and `join_parks_not_leaves` tests carried.
///
/// covers: FE-7, FE-10
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
    let founder_id = founder.node.self_id.to_string();

    let script = async {
        let old = poll_status(fport, "the founder's join_link", |d| {
            d["join_link"].as_str().map(str::to_string)
        })
        .await;
        assert!(
            old.contains(&founded.join_key),
            "a start from disk serves the invite key the founding wrote: {old}"
        );

        // relay candidates: each row shape-correct for the desktop's typed
        // reader, at most one recommended (the daemon's
        // `relay_candidates_endpoint_returns_classified_array`).
        let doc: Value = reqwest::get(format!("http://127.0.0.1:{fport}/v1/mesh/relay-candidates"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let rows = doc["candidates"]
            .as_array()
            .expect("candidates is an array");
        for c in rows {
            assert!(c["ip"].is_string() && c["kind"].is_string(), "{c}");
            assert!(
                c["url_fragment"].is_string() && c["recommended"].is_boolean(),
                "{c}"
            );
        }
        assert!(
            rows.iter().filter(|c| c["recommended"] == true).count() <= 1,
            "{doc}"
        );

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
        assert!(
            solo(nport).await,
            "a refused join leaves the node as it was"
        );

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

        // A mesh this node does not belong to is refused by name.
        let (code, body) = post(nport, "/v1/mesh/switch", json!({ "mesh": "nope" })).await;
        assert_eq!(code, 404, "{body}");
        assert!(body.to_string().contains("nope"), "{body}");

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
        // FE-7: the parked membership kept its roster, so the return is a
        // resume — the founder is on it with no handshake.
        poll_status(nport, "the founder on the resumed roster", |d| {
            d["members"]
                .as_array()?
                .iter()
                .any(|m| m["node_id"] == json!(founder_id))
                .then_some(())
        })
        .await;
        // A failed join from a populated mesh keeps it: refused for the
        // invite, never for being on a mesh, and nothing on disk moves.
        let (code, body) = post(nport, "/v1/mesh/join", json!({ "key_or_url": old })).await;
        assert_eq!(code, 403, "{body}");
        let doc = poll_status(nport, "the active mesh", |d| active_mesh(d)).await;
        assert_eq!(doc, "Lab", "the failed join kept the active mesh");
        assert_eq!(
            identity::load_mesh(node_dir.path()).unwrap().unwrap().id,
            founded.mesh.id,
            "and its state on disk"
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

        // FE-10: leaving removes the departed membership and only it — a
        // parked one survives, and resumes.
        let (code, body) = post(nport, "/v1/mesh/create", json!({ "name": "Third" })).await;
        assert_eq!(code, 200, "{body}");
        let (code, body) = post(nport, "/v1/mesh/leave", json!({})).await;
        assert_eq!(code, 200, "{body}");
        assert_eq!(
            identity::load_join_key(node_dir.path()).unwrap(),
            None,
            "the left mesh's invite key is gone, so the next mesh inherits no stale invite"
        );
        let doc = poll_status(nport, "the parked mesh alone", |d| {
            let names: Vec<&str> = d["meshes"]
                .as_array()?
                .iter()
                .filter_map(|m| m["name"].as_str())
                .collect();
            (names == ["Lab"]).then(|| d.clone())
        })
        .await;
        assert_eq!(active_mesh(&doc), None, "no pointer names the left mesh");
        let (code, body) = post(nport, "/v1/mesh/switch", json!({ "mesh": "Lab" })).await;
        assert_eq!(code, 200, "{body}");

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

/// A mesh resumed from disk whose invite key file is gone still starts, on
/// its mesh, and names the missing invite rather than serving a stale one.
/// Successor of the daemon's join_key_persistence
/// `resume_with_missing_join_key_secret_is_non_fatal`.
#[tokio::test(flavor = "multi_thread")]
async fn a_mesh_resumed_without_its_join_key_starts_and_names_the_absence() {
    let dir = tempfile::tempdir().unwrap();
    let founded = found::found(dir.path(), "Lab", "founder").expect("founds");
    std::fs::remove_file(identity::join_key_file(dir.path())).expect("the key file");
    let port = free_port();
    let daemon = RailsDaemon::start_from_disk(
        RailsNode::bind(dir.path().to_path_buf(), hermetic("founder", port))
            .await
            .expect("binds"),
    )
    .await
    .expect("a missing invite key is not a reason to refuse the start");
    assert!(!daemon.is_solo(), "it resumed its mesh");
    let script = async {
        let doc = poll_status(port, "the status", |d| Some(d.clone())).await;
        assert!(doc["join_link"].is_null(), "no stale invite: {doc}");
        assert!(
            doc["join_link_absent"].is_string(),
            "the absence is named: {doc}"
        );
        assert_eq!(
            identity::load_mesh(dir.path()).unwrap().unwrap().id,
            founded.mesh.id
        );
    };
    tokio::select! {
        exit = daemon.run() => panic!("stopped serving: {exit:?}"),
        () = script => {}
    }
}
