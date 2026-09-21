// SPDX-License-Identifier: AGPL-3.0-or-later
//! `tg-2-strangers-are-refused` — the internal port, driven as a stranger.
//!
//! One clause of `quality/campaigns/threat-gaps.toml`'s
//! `tg-stranger-refused-9742` per test, through the REAL `internal_router`
//! rather than a hand-rolled one: a test that mounts its own routes cannot
//! fail on a gate that was never applied in `server.rs`, which is the only
//! failure that matters here (ARCH principle 5 — assert on something the
//! subject cannot author).
//!
//! Clause (b) — a non-member key over the internal ALPN — is the mesh crate's:
//! the two-endpoint iroh harness lives there, and so does the rest of "who may
//! dial what", one test per ALPN
//! (`sovereign-mesh/tests/main/iroh_dialer_admission_e2e.rs`,
//! `a_non_member_key_over_the_internal_alpn_is_refused` and its member control).

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_transport::mesh_proof::mesh_proof_stamp;
use sovereign_daemon::internal_gate::EXEMPT_ROUTES;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;
use std::collections::HashMap;
use tower::ServiceExt;

/// A LAN address — the stranger's, and the plain-IP member's. Both arrive the
/// same way; what tells them apart is the header one of them carries.
const LAN: [u8; 4] = [10, 0, 0, 7];

const SECRET: [u8; 32] = [9u8; 32];

fn mesh_with_secret() -> Mesh {
    Mesh {
        mesh_secret: SECRET,
        invite_expires_at: None,
        id: MeshId::from_u128(4242),
        name: "Gate Test".into(),
        invite_key_hash: [0u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    }
}

fn state() -> AppState {
    AppState::new(NodeId::from_u128(1), mesh_with_secret())
}

fn now_secs() -> u64 {
    sovereign_time::unix_now_u64()
}

/// The header pair a member of this mesh would carry on a plain-IP hop. Minted
/// through the ONE minter, against the same secret the daemon holds.
fn member_stamp() -> (&'static str, String) {
    let mesh = mesh_with_secret();
    let stamp = mesh_proof_stamp(&mesh, NodeId::from_u128(77), now_secs()).expect("secret is set");
    let (name, value) = stamp.pair();
    (name, value.to_string())
}

fn from_lan(mut req: Request<Body>) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((LAN, 51000))));
    req
}

/// ── Clause (a). An uncredentialed non-loopback POST to quiesce is 401, AND
/// the flag it was trying to flip did not move.
///
/// The second half is the one that makes this a refusal rather than a status
/// code: a gate that answered 401 after running the handler would look
/// identical on the wire.
#[tokio::test]
async fn a_stranger_cannot_quiesce_this_node_and_the_flag_does_not_move() {
    let state = state();
    assert!(!state.mesh_quiesced(), "the fixture starts un-quiesced");

    let resp = internal_router(state.clone())
        .oneshot(from_lan(
            Request::post("/internal/mesh/quiesce")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"quiesced":true}"#))
                .unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !state.mesh_quiesced(),
        "the refusal ran BEFORE the handler — a 401 with the flag flipped is \
         not a refusal, it is a side effect with a status code"
    );
}

/// ── Clause (c), the route half. A plain-IP member — the same LAN address,
/// carrying a mesh proof — reaches the same route and flips the flag.
///
/// Read together with the test above this is the whole bar: the gate
/// distinguishes two callers who differ in exactly one header.
#[tokio::test]
async fn a_plain_ip_member_can_quiesce_this_node() {
    let state = state();
    let (name, value) = member_stamp();

    let resp = internal_router(state.clone())
        .oneshot(from_lan(
            Request::post("/internal/mesh/quiesce")
                .header("content-type", "application/json")
                .header(name, &value)
                .body(Body::from(r#"{"quiesced":true}"#))
                .unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.mesh_quiesced(), "a member's request is served");
}

/// ── Clause (d). A fresh node still reaches the door it joins through, from
/// the same LAN address, carrying nothing.
///
/// `/internal/join` answers on its own credential — this asserts only that the
/// gate did not answer first. A 401 here would mean a joiner can never become
/// a member, which is the way this gate could break the mesh outright.
#[tokio::test]
async fn a_fresh_node_still_reaches_the_join_door_carrying_nothing() {
    let resp = internal_router(state())
        .oneshot(from_lan(
            Request::post("/internal/join")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        ))
        .await
        .unwrap();

    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a joiner holds no mesh credential by definition; gating the join door \
         means nobody can ever join over IP"
    );
}

/// Every `/internal/...` path `server.rs` mounts on `internal_router`, read out
/// of the source rather than typed here.
///
/// The two const-named routes are named by their CONSTS, the same two
/// `server.rs` mounts, so a change to either wire path travels here on its own.
fn mounted_internal_paths() -> Vec<String> {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = loop {
        let candidate = dir.join("sovereign/crates/sovereign-daemon/src/server.rs");
        if candidate.exists() {
            break candidate;
        }
        assert!(
            dir.pop(),
            "no server.rs above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    };
    let text = std::fs::read_to_string(&source).expect("read server.rs");
    let start = text
        .find("pub fn internal_router")
        .expect("internal_router is defined in server.rs");
    // The function ends at the next item at column 0. `with_state(state)` is
    // its last expression, so anything after that belongs to `serve`.
    let body = &text[start..];
    let end = body
        .find("\n/// Start both API servers")
        .unwrap_or(body.len());
    let body = &body[..end];

    let mut paths: Vec<String> = Vec::new();
    for raw in body.split('"').skip(1).step_by(2) {
        if raw.starts_with("/internal/") && !paths.iter().any(|p| p == raw) {
            paths.push(raw.to_string());
        }
    }
    for c in [
        commonwealth_core::model::MODELS_LIST_PATH,
        commonwealth_core::model::MODEL_FILE_ROUTE,
    ] {
        paths.push(c.to_string());
    }
    // Path params: any concrete segment reaches the same mount, and the gate
    // runs ahead of routing anyway.
    paths
        .into_iter()
        .map(|p| {
            let mut out = String::new();
            let mut skipping = false;
            for ch in p.chars() {
                match ch {
                    '{' => {
                        skipping = true;
                        out.push('x');
                    }
                    '}' => skipping = false,
                    c if !skipping => out.push(c),
                    _ => {}
                }
            }
            out
        })
        .collect()
}

/// ── Clause (e). THE EXEMPT SET IS EXACTLY TWO, and this test does not own
/// the list it checks.
///
/// Every path is read out of `server.rs`; every one is driven as a stranger.
/// A path this gate does not guard answers anything but 401 — so adding a
/// third exempt route turns this red and names it, which is the whole point:
/// the exemption is where this gate can be silently widened.
#[tokio::test]
async fn the_exempt_set_is_exactly_the_two_doors() {
    let paths = mounted_internal_paths();
    assert!(
        paths.len() > 50,
        "the walk found only {} mounted paths — it is not reading the router \
         it claims to read, which would make this gate pass vacuously",
        paths.len()
    );

    // NAMED HERE, not read from `EXEMPT_ROUTES`. The exempt set is the
    // SUBJECT of this test — asserting it against itself is the tautology the
    // row warned about, and it stays green through a third entry. The PATHS
    // still come from `server.rs`, which is the half the test must not author.
    const THE_TWO_DOORS: &[&str] = &["/internal/join", "/internal/gossip"];
    assert_eq!(
        EXEMPT_ROUTES, THE_TWO_DOORS,
        "a third exempt route is an operator decision (order §Assumptions), \
         not a fix — and every route added to that set stops being gated"
    );

    let mut wrongly_open = Vec::new();
    let mut wrongly_shut = Vec::new();
    for path in &paths {
        let resp = internal_router(state())
            .oneshot(from_lan(
                Request::post(path.as_str())
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            ))
            .await
            .unwrap();
        let refused = resp.status() == StatusCode::UNAUTHORIZED;
        let exempt = THE_TWO_DOORS.contains(&path.as_str());
        if refused && exempt {
            wrongly_shut.push(path.clone());
        }
        if !refused && !exempt {
            wrongly_open.push(format!("{path} answered {}", resp.status()));
        }
    }

    assert!(
        wrongly_open.is_empty(),
        "these mounted routes answered a stranger on the LAN:\n{}",
        wrongly_open.join("\n")
    );
    assert!(
        wrongly_shut.is_empty(),
        "these doors must stay open to a caller with no mesh identity — it is \
         how one becomes a member:\n{}",
        wrongly_shut.join("\n")
    );
}

/// ── The gate-level test the stamping work owes: every route the five
/// disclosed builders hit admits a MARKED non-loopback caller.
///
/// This is the goodhart the row named. Clause (c) covers gossip and ring sync
/// only, so a gate that refused the corpus pull, the shard serve, the model
/// file or the rpc-warm would leave `tg-stranger-refused-9742` reading PASSED
/// with auto-ingest, collaborative merge and distributed load silently broken
/// on every mesh created without `--encrypt`. One route per stamped builder.
#[tokio::test]
async fn every_route_the_stamped_builders_hit_admits_a_marked_member() {
    // Each entry is (route, which builder reaches it). Named so a failure says
    // what broke rather than which string did not match.
    let reached: &[(&str, &str)] = &[
        ("/internal/corpus/next_unit", "auto_ingest::pull_loop"),
        ("/internal/corpus/heartbeat", "auto_ingest::spawn_heartbeat"),
        ("/internal/corpus/complete_unit", "auto_ingest::pull_loop"),
        (
            "/internal/corpus/canonical/wiki-mini",
            "canonical_pull::pull_canonical_from_peer",
        ),
        ("/internal/index/serve", "ShardManager::fetch_remote_shard"),
        (
            "/internal/corpus/partition_evict",
            "ShardManager::merge_participants (ephemeral)",
        ),
        ("/internal/rpc-warm", "rpc_warm_http::orchestrate_warm"),
        (
            commonwealth_core::model::MODELS_LIST_PATH,
            "model_fetch::list_peer_files",
        ),
        (
            "/internal/v1/models/file/m.gguf",
            "model_fetch::fetch_model_to_dir + warm_cache_from_ranges",
        ),
    ];

    let (name, value) = member_stamp();
    let mut refused = Vec::new();
    for (route, builder) in reached {
        for method in ["POST", "GET"] {
            let req = Request::builder()
                .method(method)
                .uri(*route)
                .header("content-type", "application/json")
                .header(name, &value)
                .body(Body::from("{}"))
                .unwrap();
            let resp = internal_router(state())
                .oneshot(from_lan(req))
                .await
                .unwrap();
            if resp.status() == StatusCode::UNAUTHORIZED {
                refused.push(format!("{method} {route} — reached by {builder}"));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "the gate refused a marked member on routes this workspace's own \
         builders call. Every one of these is a stamped builder whose stamp \
         is not being read:\n{}",
        refused.join("\n")
    );
}

/// A member's proof from ANOTHER mesh is not a credential here — the marker is
/// the group, so the group has to be this one.
///
/// The negative control for `a_plain_ip_member_can_quiesce_this_node`: without
/// it, that test could be passing because the gate admits any caller that sets
/// the header at all.
#[tokio::test]
async fn a_proof_from_another_mesh_is_refused() {
    let state = state();
    let mut theirs = mesh_with_secret();
    theirs.mesh_secret = [1u8; 32];
    let stamp = mesh_proof_stamp(&theirs, NodeId::from_u128(77), now_secs()).unwrap();
    let (name, value) = stamp.pair();

    let resp = internal_router(state.clone())
        .oneshot(from_lan(
            Request::post("/internal/mesh/quiesce")
                .header("content-type", "application/json")
                .header(name, value)
                .body(Body::from(r#"{"quiesced":true}"#))
                .unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(!state.mesh_quiesced());
}

/// ── The one END-TO-END leg, and why there is only one.
///
/// A non-loopback hop cannot be produced on a single host: every socket a test
/// can open lands on `127.0.0.1`, and the gate would admit it for that reason
/// alone. So the listener below wraps the REAL `internal_router` in one outer
/// layer that replaces the connect info with a LAN address before the gate
/// reads it, and records the status of every answer. Everything under that line
/// is real — reqwest, the header the builder itself applied, the gate, the
/// handler. The two-machine version of this is `HUMAN-tg-the-stranger`.
///
/// `ShardManager::fetch_remote_shard` is the builder driven here because it is
/// the cheapest of the five to stand up. It is what links the stamping half of
/// this row to the refusing half: drop the stamp from that builder and this
/// test goes red naming `/internal/index/serve`, not just its own unit test.
async fn serve_as_lan(state: AppState) -> (String, std::sync::Arc<std::sync::Mutex<Vec<u16>>>) {
    let seen: std::sync::Arc<std::sync::Mutex<Vec<u16>>> = Default::default();
    let sink = seen.clone();
    let app = internal_router(state).layer(axum::middleware::from_fn(
        move |mut req: axum::extract::Request, next: axum::middleware::Next| {
            let sink = sink.clone();
            async move {
                req.extensions_mut()
                    .insert(ConnectInfo(SocketAddr::from((LAN, 51000))));
                let resp = next.run(req).await;
                sink.lock().unwrap().push(resp.status().as_u16());
                resp
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), seen)
}

/// Drive one real shard pull against the real router and give back the statuses
/// the router answered with.
async fn shard_pull_statuses(stamped: bool) -> Vec<u16> {
    use commonwealth_state::MeshStore;
    use corpus_engine::{CorpusEngine, EmbedFn};
    use sovereign_grants::shard_manager::MergePlan;
    use sovereign_grants::ShardManager;
    use std::sync::Arc;

    let tmp = tempfile::tempdir().unwrap();
    let (url, seen) = serve_as_lan(state()).await;

    let embed: EmbedFn = Arc::new(|_: &str| Box::pin(async { Ok(vec![0.0f32; 8]) }));
    let index_dir = tmp.path().join("indexes");
    let engine = Arc::new(CorpusEngine::new(
        tmp.path().join("recipes"),
        index_dir.clone(),
        embed,
    ));
    let manager = ShardManager::new(
        Arc::clone(&engine),
        index_dir,
        Arc::new(MeshStore::in_memory().unwrap()),
    );

    let (name, value) = member_stamp();
    let local = NodeId::from_u128(1);
    let peer = NodeId::from_u128(2);
    let peer_urls = vec![(peer, url)];
    let participants = [local, peer];

    let _ = manager
        .merge_participants(MergePlan {
            handoff_id: commonwealth_core::ids::HandoffId::from_u128(7),
            corpus_id: "wire",
            local_node_id: local,
            participants: &participants,
            peer_shard_base_urls: &peer_urls,
            mesh_proof: stamped.then_some((name, value.as_str())),
            ephemeral: false,
            expected_partitions: None,
        })
        .await;

    let out = seen.lock().unwrap().clone();
    assert!(
        !out.is_empty(),
        "the shard pull never reached the router — this test would then pass \
         for the wrong reason"
    );
    out
}

#[tokio::test]
async fn a_members_shard_pull_is_admitted_end_to_end() {
    let statuses = shard_pull_statuses(true).await;
    assert!(
        !statuses.contains(&401),
        "the gate refused a shard pull this workspace's own merge builds: {statuses:?}"
    );
}

/// THE failing input for the test above, standing permanently rather than only
/// under a plant: the SAME builder with no pair to apply is refused, so the
/// green above is the stamp being read and not the gate being open.
#[tokio::test]
async fn an_unstamped_shard_pull_is_refused_end_to_end() {
    let statuses = shard_pull_statuses(false).await;
    assert!(
        statuses.contains(&401),
        "an unstamped pull from a non-loopback caller must be refused: {statuses:?}"
    );
}
