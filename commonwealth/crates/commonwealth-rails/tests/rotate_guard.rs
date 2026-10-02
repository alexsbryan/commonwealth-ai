// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rotate's pre-split guard (pb-mesh-exit-transport; director phase-b-81
//! (2); FE-15, FE-17): the daemon's `rotate_pre_split_guard` tests, on the
//! rotate cw-rails serves since the flip.
//!
//! A node founded alone, with one Online peer added that carries no key, so
//! the confirmation round dials nobody and the peer stays unconfirmed unless
//! a test records its generation the way a merge would.

use std::sync::Arc;

use axum::extract::{Query, State};
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{MemberRecord, NodeStatus};
use commonwealth_rails::config::{Config, MediaSection, RelaySection};
use commonwealth_rails::membership::{rotate, RotateQuery};
use commonwealth_rails::{gossip, RailsDaemon, RailsNode};

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

/// A founder with one Online peer named `peer_name`.
async fn with_online_peer(peer_name: &str) -> (Arc<RailsDaemon>, NodeId, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let node = RailsNode::bind(dir.path().to_path_buf(), hermetic("founder"))
        .await
        .expect("binds");
    let (mut mesh, _key) =
        commonwealth_discovery::membership::init_mesh("rotate-guard", "founder", Vec::new());
    let founder = mesh.members.values().next().unwrap().clone();
    mesh.members.clear();
    let mut me = founder.clone();
    me.node_id = node.self_id;
    mesh.members.insert(node.self_id, me);
    let peer = NodeId::from_u128(0x5150_6060_7070_8080);
    let mut row: MemberRecord = founder;
    row.node_id = peer;
    row.name = peer_name.into();
    row.status = NodeStatus::Online;
    row.node_pubkey = None;
    mesh.members.insert(peer, row);
    let daemon = Arc::new(RailsDaemon::start(node, mesh).await.expect("starts"));
    (daemon, peer, dir)
}

fn observe(daemon: &RailsDaemon, peer: NodeId, post_split: bool) {
    daemon
        .split_generation
        .lock()
        .unwrap()
        .insert(peer, post_split);
}

async fn rotate_now(daemon: &Arc<RailsDaemon>, force: bool) -> (u16, serde_json::Value) {
    let resp = rotate(State(daemon.clone()), Query(RotateQuery { force })).await;
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// An Online peer not merged since start blocks rotation, named as
/// unconfirmed and never as an old build.
///
/// covers: FE-17
#[tokio::test(flavor = "multi_thread")]
async fn rotate_refuses_while_an_unconfirmed_peer_is_online() {
    let (daemon, _peer, _dir) = with_online_peer("stale-peer").await;
    let (status, body) = rotate_now(&daemon, false).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["unconfirmed"], serde_json::json!(["stale-peer"]));
    assert_eq!(
        body["pre_split"],
        serde_json::json!([]),
        "a peer never reached is not an old BUILD"
    );
}

/// The refusal tells the two populations apart, each with its own remedy.
///
/// covers: FE-15
#[tokio::test(flavor = "multi_thread")]
async fn the_refusal_tells_an_unconfirmed_peer_apart_from_an_old_build() {
    let (daemon, peer, _dir) = with_online_peer("old-build").await;
    observe(&daemon, peer, false);
    let (_, body) = rotate_now(&daemon, false).await;
    let said = body["error"].as_str().unwrap();
    assert!(
        said.contains("pre-split build") && said.contains("upgrade them first"),
        "an observed OLD BUILD is told to upgrade: {said}"
    );

    let (daemon, _peer, _dir) = with_online_peer("never-reached").await;
    let (_, body) = rotate_now(&daemon, false).await;
    let said = body["error"].as_str().unwrap();
    assert!(
        said.contains("not been confirmed since this daemon started"),
        "{said}"
    );
    assert!(!said.contains("pre-split build"), "{said}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rotate_refuses_a_peer_confirmed_pre_split() {
    let (daemon, peer, _dir) = with_online_peer("old-build").await;
    observe(&daemon, peer, false);
    let (status, body) = rotate_now(&daemon, false).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["pre_split"], serde_json::json!(["old-build"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn rotate_proceeds_once_every_online_peer_is_confirmed_post_split() {
    let (daemon, peer, dir) = with_online_peer("upgraded-peer").await;
    observe(&daemon, peer, true);
    let before = daemon.mesh.read().await.invite_key_hash;
    let (status, body) = rotate_now(&daemon, false).await;
    assert_eq!(status, 200, "{body}");
    assert!(!body["join_key"].as_str().unwrap().is_empty());
    // The live mesh moved, and disk agrees with it, so the next gossip round
    // has nothing to revert (the daemon's mesh_http_tests
    // `rotate_changes_the_live_in_memory_hash_not_only_the_disk_one`,
    // `rotate_leaves_disk_and_memory_agreeing_so_a_gossip_round_cannot_revert_it`).
    let live = daemon.mesh.read().await.invite_key_hash;
    assert_ne!(live, before, "the live hash rotated");
    let on_disk = commonwealth_rails::identity::load_mesh(dir.path())
        .unwrap()
        .expect("persisted")
        .invite_key_hash;
    assert_eq!(on_disk, live, "disk and memory agree");
}

#[tokio::test(flavor = "multi_thread")]
async fn force_rotates_even_with_an_unconfirmed_peer_online() {
    let (daemon, _peer, _dir) = with_online_peer("stale-peer").await;
    let (status, body) = rotate_now(&daemon, true).await;
    assert_eq!(status, 200, "--force overrides the refusal: {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_upgrades_stops_blocking_rotation() {
    let (daemon, peer, _dir) = with_online_peer("upgrading-peer").await;
    observe(&daemon, peer, false);
    assert_eq!(rotate_now(&daemon, false).await.0, 409, "blocked while old");
    observe(&daemon, peer, true);
    assert_eq!(
        rotate_now(&daemon, false).await.0,
        200,
        "the peer upgraded; the refusal lifts"
    );
    assert_eq!(
        gossip::split_generation_of(&daemon.split_generation, peer),
        Some(true)
    );
}

async fn status_now(daemon: &Arc<RailsDaemon>) -> serde_json::Value {
    use axum::response::IntoResponse;
    let resp = commonwealth_rails::api::status(State(daemon.clone()))
        .await
        .into_response();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A solo node has no mesh to rotate (404) and serves no invite. Successors
/// of the daemon's mesh_http_tests `rotate_without_mesh_returns_404` and
/// `status_omits_invite_when_solo`.
#[tokio::test(flavor = "multi_thread")]
async fn a_solo_node_rotates_nothing_and_serves_no_invite() {
    let dir = tempfile::tempdir().unwrap();
    let node = RailsNode::bind(dir.path().to_path_buf(), hermetic("solo"))
        .await
        .expect("binds");
    let daemon = Arc::new(RailsDaemon::start_from_disk(node).await.expect("starts"));
    assert!(daemon.is_solo(), "no mesh.json starts solo");
    assert_eq!(rotate_now(&daemon, false).await.0, 404);
    assert!(status_now(&daemon).await["join_link"].is_null());
}

/// A rotation's key is the one status serves at once, in place. Successor of
/// the daemon's mesh_http_tests `rotate_refreshes_status_invite_in_place`.
#[tokio::test(flavor = "multi_thread")]
async fn status_serves_the_rotated_invite_in_place() {
    let (daemon, peer, _dir) = with_online_peer("upgraded-peer").await;
    observe(&daemon, peer, true);
    let (status, body) = rotate_now(&daemon, false).await;
    assert_eq!(status, 200, "{body}");
    let key = body["join_key"].as_str().unwrap().to_string();
    let link = status_now(&daemon).await["join_link"]
        .as_str()
        .map(str::to_string);
    assert!(
        link.as_deref().is_some_and(|l| l.contains(&key)),
        "status serves {link:?}, not the rotated key {key}"
    );
}
