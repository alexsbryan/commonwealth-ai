// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `mesh_http` route tests, beside the file they exercise — ARCH §3.1's
//! split: `mesh_http.rs` was past its ceiling and this trailing module is the
//! self-contained half. `super::*` still resolves to `mesh_http`.

use super::*;
use crate::EmbeddedDaemon;
use sovereign_core::setup_config::{
    DaemonSection, DiscoverySection, IrohSection, ModelsSection, SetupConfig,
};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tempfile::TempDir;

/// Hermetic daemon config for tests: ephemeral ports (`0`) so parallel
/// `create`/`leave` tests never fight over the real `:9741`/`:9742` — a
/// bind conflict is now a hard error (`MeshError::Network`) rather than
/// silently swallowed — and mDNS + iroh off so no unit test touches a
/// multicast socket or binds an iroh endpoint. Everything else defaulted.
fn hermetic_cfg() -> SetupConfig {
    SetupConfig {
        engine: Default::default(),
        compute: Default::default(),
        search: Default::default(),
        models: Some(ModelsSection {
            primary: PathBuf::from("/models/primary.gguf"),
            fast: None,
            embed: PathBuf::from("/models/embed.gguf"),
            code: None,
            context_size: None,
            fast_context_size: None,
            max_extras_memory_gb: None,
            extra: BTreeMap::new(),
            primary_pool: None,
            edit: None,
        }),
        node: Default::default(),
        daemon: DaemonSection {
            client_port: 0,
            internal_port: 0,
            ..Default::default()
        },
        data: Default::default(),
        watched_folders: Default::default(),
        memory: Default::default(),
        iroh: IrohSection {
            enabled: Some(false),
            ..Default::default()
        },
        shared_model: Default::default(),
        discovery: DiscoverySection {
            mdns: false,
            ..Default::default()
        },
        mcp_servers: Vec::new(),
    }
}

/// Stand up the mesh HTTP router over a no-mesh daemon bound to
/// an ephemeral localhost port. Returns `(daemon_arc, base_url,
/// _tmp)` — hold the tempdir so it isn't cleaned up mid-test.
async fn spawn_test_router() -> (Arc<EmbeddedDaemon>, String, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        hermetic_cfg(),
        crate::daemon_services::DaemonServices::mesh_admin(),
    );
    let app = mesh_router(Arc::clone(&daemon));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (daemon, format!("http://{addr}"), tmp)
}

#[tokio::test]
async fn status_returns_empty_when_no_mesh() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["running"], false);
    assert_eq!(body["member_count"].as_u64().unwrap_or(0), 0);
}

#[tokio::test]
async fn create_and_status_round_trip() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["mesh_name"], "test mesh");
    assert!(body["join_key"].as_str().unwrap().starts_with("cwth-"));
    assert!(body["join_link"].as_str().unwrap().contains("sovereign://"));

    // Status should now report running + one member.
    let resp = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["running"], true);
    assert_eq!(body["mesh_name"], "test mesh");
    assert_eq!(body["members_total"], 1);
}

/// The user-facing `POST /v1/mesh/leave` must return the node to a live
/// SOLO mesh in the SAME process — `/v1/mesh/status` keeps answering, no
/// restart. Regression guard for the bug where leaving a mesh killed
/// `:9741` with no way back (the daemon tore down its listeners and
/// relied on a service manager that wasn't there to relaunch it).
#[tokio::test]
async fn http_leave_returns_to_solo_mesh() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();

    // Create a mesh so leave() has something to leave.
    let resp = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = client
        .post(format!("{base}/v1/mesh/leave"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // The re-solo runs in a detached task after a short grace, so poll
    // status until the fresh solo mesh is back up (same process, same
    // test listener). We wait specifically for a mesh that is NOT the
    // old "test mesh" — the pre-teardown window still reports the old
    // one as running. A missing re-solo would never satisfy this and
    // fail the assertion after the loop.
    let mut running_solo = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let body: serde_json::Value = client
            .get(format!("{base}/v1/mesh/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["running"] == true
            && body["members_total"] == 1
            && body["mesh_name"].as_str() != Some("test mesh")
        {
            running_solo = true;
            break;
        }
    }
    assert!(
        running_solo,
        "POST /v1/mesh/leave should re-create a live solo mesh in-process"
    );
}

/// A DIRECT `leave()` — the path `join_mesh`'s auto-leave and the
/// deprecated `stop()` take when switching meshes — must NOT re-create a
/// solo mesh. It leaves the daemon Stopped so the caller can join the
/// next mesh; only the user-facing `leave_to_solo` bounces back to solo.
#[tokio::test]
async fn direct_leave_leaves_daemon_stopped() {
    let (daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "test mesh", "node_name": "alice" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(daemon.is_running().await);

    // Leave via the library method, exactly as join_mesh's auto-leave does.
    daemon.leave().await.unwrap();

    assert!(
        !daemon.is_running().await,
        "direct leave() must leave the daemon Stopped (no auto re-solo — that \
         would restart the daemon mid mesh-switch)"
    );
}

/// Leaving and re-soloing repeatedly must rebind the SAME address cleanly
/// every time. `stop_inner` awaits the old serve task before `create_mesh`
/// rebinds, so the in-process rebind never races the just-dropped socket
/// into `EADDRINUSE`. Uses a fixed (non-ephemeral) port so each iteration
/// genuinely re-binds the same `host:port` — the exact race being guarded.
#[tokio::test]
async fn leave_to_solo_rebinds_same_port_repeatedly() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = hermetic_cfg();
    cfg.daemon.client_port = 39411;
    cfg.daemon.internal_port = 39412;
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        cfg,
        crate::daemon_services::DaemonServices::mesh_admin(),
    );

    daemon.create_mesh("test mesh", "alice").await.unwrap();
    for i in 0..5 {
        daemon
            .leave_to_solo()
            .await
            .unwrap_or_else(|e| panic!("re-solo #{i} failed (bind race?): {e}"));
        assert!(
            daemon.is_running().await,
            "daemon should be running after re-solo #{i}"
        );
    }
}

#[tokio::test]
async fn create_fails_when_mesh_already_exists() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let _ = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "first" }))
        .send()
        .await
        .unwrap();
    let resp = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "second" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "second create must conflict");
}

#[tokio::test]
async fn rotate_after_create_changes_invite_key_hash() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();

    let create: serde_json::Value = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "m" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let original_key = create["join_key"].as_str().unwrap().to_string();

    let rotate: serde_json::Value = client
        .post(format!("{base}/v1/mesh/rotate"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rotated_key = rotate["join_key"].as_str().unwrap();
    assert!(rotated_key.starts_with("cwth-"));
    assert_ne!(original_key, rotated_key);
}

#[tokio::test]
async fn rotate_without_mesh_returns_404() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/mesh/rotate"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn status_includes_invite_after_create() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let create: serde_json::Value = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "Lab Squad" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let created_key = create["join_key"].as_str().unwrap().to_string();

    let status: serde_json::Value = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status["join_key"].as_str().unwrap(),
        created_key,
        "status must echo back the same plaintext key"
    );
    let link = status["join_link"].as_str().unwrap();
    assert!(link.starts_with("sovereign://join/"));
    assert!(link.contains(&created_key));
}

#[tokio::test]
async fn status_omits_invite_when_solo() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let status: serde_json::Value = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(status.get("join_key").is_none_or(|v| v.is_null()));
    assert!(status.get("join_link").is_none_or(|v| v.is_null()));
}

#[tokio::test]
async fn rotate_refreshes_status_invite_in_place() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let create: serde_json::Value = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "m" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pre_key = create["join_key"].as_str().unwrap().to_string();

    let rotate: serde_json::Value = client
        .post(format!("{base}/v1/mesh/rotate"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_key = rotate["join_key"].as_str().unwrap().to_string();
    assert_ne!(pre_key, new_key);

    let status: serde_json::Value = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["join_key"].as_str().unwrap(), new_key);
    assert!(status["join_link"].as_str().unwrap().contains(&new_key));
    // The rotate answer carries the same link status does — the CLI prints
    // it, and before this it printed a dial-less bare key instead.
    assert_eq!(rotate["join_link"], status["join_link"]);
}

/// Read the hash the RUNNING daemon actually gates on, not the plaintext
/// it handed back.
async fn live_invite_key_hash(daemon: &Arc<EmbeddedDaemon>) -> [u8; 32] {
    let app = daemon
        .app_state()
        .await
        .expect("daemon is running after create");
    let mesh = app.inner.fabric.mesh.read().await;
    mesh.invite_key_hash
}

/// Create a mesh over the router and return its plaintext join key.
async fn create_mesh_over_http(client: &reqwest::Client, base: &str) -> String {
    let create: serde_json::Value = client
        .post(format!("{base}/v1/mesh/create"))
        .json(&serde_json::json!({ "name": "m" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    create["join_key"].as_str().unwrap().to_string()
}

/// ARCH §18.1 — assert on something the subject cannot author.
///
/// The three rotate tests above all read `join_key`: the plaintext the
/// handler itself just returned. That is a verbatim echo of the value
/// under test, so they pass cleanly on the exact failure they exist to
/// catch. The state that actually gates admission is
/// `AppState.inner.fabric.mesh.invite_key_hash`, and nothing reads it.
///
/// The defect this was written against: rotation wrote the new hash to
/// disk and refreshed the cached plaintext, but never wrote the live
/// `Mesh`, so the running daemon kept admitting the OLD key on
/// `/internal/join` and kept gossiping the OLD hash. `rotate_join_key` is
/// now the one implementation and mutates the live mesh first; this
/// assertion is what stays red if that is ever undone.
#[tokio::test]
async fn rotate_changes_the_live_in_memory_hash_not_only_the_disk_one() {
    let (daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();

    let original_key = create_mesh_over_http(&client, &base).await;
    let hash_before = live_invite_key_hash(&daemon).await;
    assert_eq!(
        hash_before,
        commonwealth_discovery::membership::hash_join_key(&original_key),
        "precondition: the live mesh gates on the key create just minted"
    );

    let rotate: serde_json::Value = client
        .post(format!("{base}/v1/mesh/rotate"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_key = rotate["join_key"].as_str().unwrap().to_string();
    assert_ne!(original_key, new_key, "precondition: rotation minted a key");

    let hash_after = live_invite_key_hash(&daemon).await;
    assert_eq!(
        hash_after,
        commonwealth_discovery::membership::hash_join_key(&new_key),
        "the running daemon still gates on the OLD hash — rotation touched \
         disk and the cached plaintext but not the live Mesh, so \
         /internal/join keeps admitting the old key and gossip keeps \
         advertising the old hash"
    );
}

/// The clobber, named by its mechanism rather than raced against a timer.
///
/// The gossip loop re-persists the live in-memory mesh over `mesh.json`
/// every round (`gossip.rs`'s `persist::save` call). So if rotation leaves
/// disk and memory disagreeing, the next round silently resolves the
/// disagreement toward memory and the rotation is reverted — while
/// `join_key.secret` keeps the NEW plaintext. The operator is then holding
/// an invite that hashes to nothing the mesh accepts.
///
/// Asserting agreement is strictly stronger than sleeping for a round:
/// if the two never disagree, no round can revert anything.
#[tokio::test]
async fn rotate_leaves_disk_and_memory_agreeing_so_a_gossip_round_cannot_revert_it() {
    let (daemon, base, tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();

    create_mesh_over_http(&client, &base).await;
    let _: serde_json::Value = client
        .post(format!("{base}/v1/mesh/rotate"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let in_memory = live_invite_key_hash(&daemon).await;
    let on_disk = sovereign_mesh::persist::load(tmp.path())
        .expect("mesh.json readable")
        .expect("a mesh is persisted after create")
        .invite_key_hash;

    assert_eq!(
        in_memory, on_disk,
        "rotation left the live mesh and mesh.json disagreeing; the next \
         gossip round re-persists memory over disk and silently reverts \
         the rotation"
    );
}

/// **The failing input is a rotation this node did not perform.**
///
/// A rotation performed elsewhere reaches this node as hash + version
/// (`Mesh::merge_invite_from`, so the credential travels) and the cached
/// plaintext — `join_key.secret` and the in-memory slot — is left behind by
/// design: the new plaintext never travels. Measured live 2026-09-23: a
/// daemon in exactly this state served a link its own founder refused
/// ("join key does not match"). A link that cannot join is worse than no
/// link: it reads as a working invitation and costs whoever tries it.
///
/// Assert on the served body, not on an internal flag: a stranger reads the
/// link.
#[tokio::test]
async fn status_serves_no_link_when_the_cached_key_no_longer_matches_the_invite() {
    let (daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let key = create_mesh_over_http(&client, &base).await;

    // Positive control: while cache and hash agree, the link is served.
    let status: serde_json::Value = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        status["join_link"].as_str().unwrap().contains(&key),
        "precondition: a fresh mesh serves its link"
    );

    // The divergence, by its mechanism: the hash moves (as a peer's
    // rotation moves it), the cached plaintext does not.
    let app = daemon.app_state().await.expect("daemon is running");
    app.inner.fabric.mesh.write().await.rotate_invite_key(
        commonwealth_discovery::membership::hash_join_key("cwth-0000-0000-0000"),
        None,
    );

    let status: serde_json::Value = client
        .get(format!("{base}/v1/mesh/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        status.get("join_key").is_none_or(|v| v.is_null()),
        "the stale key must not be echoed: {status}"
    );
    assert!(
        status.get("join_link").is_none_or(|v| v.is_null()),
        "no dead link may be served: {status}"
    );
    assert!(
        !status.to_string().contains(&key),
        "the stale plaintext must not appear anywhere in the status body"
    );
}

#[tokio::test]
async fn relay_candidates_endpoint_returns_classified_array() {
    // Doesn't require a mesh — just lists local interfaces.
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/v1/mesh/relay-candidates"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let arr = body["candidates"].as_array().expect("candidates is array");
    // Test runners on macOS / Linux always have at least one
    // non-loopback interface (even CI VMs). Each entry must be
    // shape-correct so the desktop's typed deserializer doesn't
    // silently drop fields.
    for c in arr {
        assert!(c["ip"].is_string());
        assert!(c["kind"].is_string());
        assert!(c["url_fragment"].is_string());
        assert!(c["recommended"].is_boolean());
    }
    // At most one should be marked recommended.
    let recommended_count = arr
        .iter()
        .filter(|c| c["recommended"].as_bool().unwrap_or(false))
        .count();
    assert!(
        recommended_count <= 1,
        "got {recommended_count} recommended"
    );
}

#[tokio::test]
async fn join_rejects_unparseable_input() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/v1/mesh/join"))
        .json(&serde_json::json!({ "key_or_url": "not a valid key" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// -- Measurements -------------------------------------------------------
//
// The namespace lives on the ring rail (`sovereign_mesh::measurements_rail`), so
// the journal-level properties — self-exclusion, ordering, what an
// undecodable line costs — are pinned there, against a real journal. What
// is left here is the MAPPING: how an admitted op becomes the DTO the CLI
// reads.

use commonwealth_rail::{Person, RingSigner};
use sovereign_mesh::measurements_rail::{RailMeasurement, RailMeasurements};
use sovereign_mesh::ring_roster::tests::{key, member, mesh_of, pubkey_of};
use sovereign_mesh::ring_roster::MeshRoster;

use sovereign_mesh::measurements_rail::tests::a_measurement;

fn node(b: u8) -> commonwealth_core::ids::NodeId {
    commonwealth_core::ids::NodeId::from_u128(u128::from(b))
}

/// A publisher is named from the ROSTER, never from the payload it wrote.
/// The `actor` on a journal line is the key that signed it — the one field
/// a writer cannot forge for someone else (ARCH §18.1) — so both halves of
/// the attribution, the node id and the display name, are derived from it.
#[test]
fn a_publisher_is_named_from_the_key_that_signed_the_line() {
    let k = key(71);
    let id = node(2);
    let roster = MeshRoster::derive(
        &mesh_of(vec![member(id, "BeefyMac", Some(pubkey_of(&k)))]),
        node(1),
        None,
    );
    let view = peer_view(
        RailMeasurements {
            found: vec![RailMeasurement {
                actor: RingSigner::actor(&k),
                person: Person::from("BeefyMac"),
                record: a_measurement(11.08, 200),
            }],
            unreadable: 0,
            gaps: 0,
        },
        &roster,
    );
    assert_eq!(view.records.len(), 1);
    assert_eq!(view.records[0].origin_node, hex::encode(id.as_bytes()));
    assert_eq!(view.records[0].origin_name.as_deref(), Some("BeefyMac"));
}

/// **An incomplete answer says so, in one number.** A line this build
/// cannot decode and a line the rail could not account for at all are both
/// "your answer covers less than the ring holds"; reporting them in two
/// fields would let a caller read one and believe the other was zero
/// (ARCH §18.3). Before the move this counted only the first.
#[test]
fn a_gap_the_rail_reported_reaches_the_reader_as_an_unreadable_line() {
    let view = peer_view(
        RailMeasurements {
            found: Vec::new(),
            unreadable: 1,
            gaps: 2,
        },
        &MeshRoster::default(),
    );
    assert!(view.records.is_empty());
    assert_eq!(
        view.unreadable, 3,
        "\"nobody has measured this\" and \"three lines did not survive \
         admission\" send an operator to different places"
    );
}

#[tokio::test]
async fn publishing_without_a_mesh_is_declined_with_a_reason_not_an_error() {
    // The harness's daemon is not running, which is exactly the state of a
    // machine that has never run `mesh create`. `mesh bench` on such a
    // machine is completely normal, so this path must be a 200 with a reason
    // the CLI can print — not a 5xx it would have to interpret.
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/mesh/measurements"))
        .json(&a_measurement(11.08, 200))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["published"], false);
    assert!(
        body["refused"].as_str().is_some_and(|s| !s.is_empty()),
        "a refusal without a reason is a failure the operator cannot act on: {body}"
    );
}

#[tokio::test]
async fn reading_peers_without_a_mesh_is_empty_not_an_error() {
    let (_daemon, base, _tmp) = spawn_test_router().await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/mesh/measurements"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "`mesh plan` is a useful command on a solo machine; the peer half \
         going missing must make the answer smaller, not fail it"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["records"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["unreadable"], 0);
}
