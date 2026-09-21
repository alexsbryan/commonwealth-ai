// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring rail, end to end through the real router.
//!
//! Convergence is settled offline in `commonwealth-knowledge`, and the
//! reference app's arithmetic in `templates/expenses.test.mjs`. What is
//! settled HERE is the join: that a grant reaches exactly one namespace's
//! journal, that an act comes back attributed to a **person** and not a node
//! key, and that every refusal the rail promises is a refusal an HTTP caller
//! actually gets.
//!
//! Note what these tests no longer assert: a balance. The rail carries an
//! opaque payload now, so there is no total for this layer to check — an
//! assertion here on `6000 - 3000` would be a second expense implementation
//! living in a test file (ARCH §10.6).
//!
//! Driven with `tower::oneshot` against `client_router`, with `ConnectInfo`
//! injected — the same harness shape as `client_auth.rs`, for the same reason:
//! the loopback-vs-remote split must not depend on the CI box having a
//! routable NIC.
//!
//! The replication drills go through `internal_router`, which is where the
//! receiver's `DefaultBodyLimit` lives — see §"the convergence ceiling", the
//! only place in the tree that says what happens when one exchange outgrows
//! it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    Digest, Ed25519Verifier, Op, Payload, Person, RailAct, RingJournal, RingRail, RingSigner,
    Roster, SignedOp,
};
use ed25519_dalek::SigningKey;
use sovereign_daemon::routes_internal::{
    RingSyncRequest, RingSyncResponse, RING_SYNC_OPS_BUDGET_BYTES,
};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::AppState;
use sovereign_grants::Scope;
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55001";
const LAN_PEER: &str = "192.168.1.50:44444";
const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";
const GUEST_TOKEN: &str = "9c1f7b2ea4d68053aa11ff7c3e5b90d4c7a2f16b8e04d93b5c7a1e2f3b4d5c6a";
const NS: &str = "house-expenses";

fn bare_state() -> AppState {
    bare_state_with_seed(
        sovereign_daemon::state::FabricSeed::default(),
        sovereign_grants::GuestSessionBinding::Door,
        Default::default(),
    )
}

/// [`bare_state`] with Fabric's construction seed — the rail is a construction
/// argument now, not a post-construction install (DC §4.2 "Construction is
/// staged, and parts are total").
fn bare_state_with_seed(
    seed: sovereign_daemon::state::FabricSeed,
    sessions: sovereign_grants::GuestSessionBinding,
    pages: sovereign_daemon::guest_door::GuestPages,
) -> AppState {
    let node = NodeId::from_u128(1);
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "Test".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members: HashMap::new(),
        peers: vec![],
    };
    AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving_and_node(
        node,
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().unwrap()),
        Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new()),
        None,
        None,
        seed,
        sovereign_daemon::state::ServingSeed::default(),
        sovereign_daemon::state::NodeSeed {
            client_token: Some(Arc::<str>::from(TOKEN)),
            guest_sessions: sessions,
            guest_pages: pages,
            internal_auth: Default::default(),
            ..Default::default()
        },
    )
}

/// A daemon with ring storage under `root`, signing as `key`, and a roster
/// that says that key is Alex. Its door binds guest sessions the default way:
/// a name claimed on one of this wall's links is the same person on the next.
fn state_with_rail(root: &std::path::Path, key: &SigningKey) -> AppState {
    state_with_rail_sessions(
        root,
        key,
        sovereign_grants::GuestSessionBinding::Door,
        Default::default(),
    )
}

/// [`state_with_rail`] on a wall whose owner DECLARED `pages` — what a wall
/// grant is scoped by. A test that mints `Scope::Wall` against a state with no
/// registry is testing a door with nothing on it.
fn state_with_wall(
    root: &std::path::Path,
    key: &SigningKey,
    pages: &[(&str, sovereign_core::guest_pages::GuestPage)],
) -> AppState {
    state_with_rail_sessions(
        root,
        key,
        sovereign_grants::GuestSessionBinding::Door,
        sovereign_daemon::guest_door::GuestPages::new(
            None,
            pages
                .iter()
                .map(|(ns, p)| ((*ns).to_string(), p.clone()))
                .collect(),
        ),
    )
}

/// [`state_with_rail`] with the session binding named — the `[daemon]
/// guest_sessions` knob, so both settings are driven by a test.
fn state_with_rail_sessions(
    root: &std::path::Path,
    key: &SigningKey,
    sessions: sovereign_grants::GuestSessionBinding,
    pages: sovereign_daemon::guest_door::GuestPages,
) -> AppState {
    let rail = Arc::new(RingRail::new(root, Arc::new(key.clone())));
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    members.insert(
        Person::from("bo"),
        vec!["bo-has-not-joined-yet".to_string()],
    );
    rail.journal(NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    bare_state_with_seed(
        sovereign_daemon::state::FabricSeed {
            ring_rail: Some(rail),
            ..Default::default()
        },
        sessions,
        pages,
    )
}

fn with_guest(state: AppState, scopes: Vec<Scope>) -> AppState {
    let now = commonwealth_core::clock::unix_now_millis();
    state
        .inner
        .node
        .guest_grants
        .issue(GUEST_TOKEN, scopes, Some("ring app".into()), 3_600, now);
    state
}

async fn call(state: AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = client_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn request(
    method: &str,
    path: &str,
    peer: &str,
    bearer: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        b = b.header(axum::http::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let mut req = match body {
        Some(v) => b
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    req.extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    req
}

/// One act, in the reference app's vocabulary. The rail does not read inside
/// `payload` — it is here in an expense shape only because that is the app
/// this rail was built against.
fn groceries() -> serde_json::Value {
    serde_json::json!({
        "op": "record",
        "payload": {
            "kind": "expense",
            "payer": "alex",
            "amount_cents": 6000,
            "description": "groceries",
            "participants": ["alex", "bo"],
        }
    })
}

fn expense_payload(payer: &str, cents: i64, what: &str) -> Payload {
    Payload::new(serde_json::json!({
        "kind": "expense",
        "payer": payer,
        "amount_cents": cents,
        "description": what,
        "participants": ["alex", "bo"],
    }))
    .unwrap()
}

// ── the outcome the whole rail exists for ────────────────────

/// **A ring app writes an act and reads it back attributed to a PERSON.**
///
/// If "who wrote this" came back as a 64-character hex key, the app is dead
/// and the rail has not delivered what it promised — that is the whole reason
/// the roster exists, and the reason this assertion is on the name.
#[tokio::test]
async fn a_ring_app_appends_an_act_and_reads_it_back_attributed_to_a_person() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );

    let (status, body) = call(
        state.clone(),
        request(
            "POST",
            "/v1/rail/append",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(groceries()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["seq"], 0);
    assert_eq!(body["namespace"], NS);
    assert_eq!(body["actor"], key.actor(), "the node key signed it");

    let (status, log) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert_eq!(log["complete"], true, "gaps: {}", log["gaps"]);
    assert_eq!(log["held"], 1);
    let ops = log["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["person"], "alex", "a person, not a node key");
    assert_eq!(ops[0]["voided"], false);
    // The payload comes back exactly as the app wrote it, untouched except
    // for its canonical key order.
    assert_eq!(ops[0]["payload"]["description"], "groceries");
    assert_eq!(ops[0]["payload"]["amount_cents"], 6000);
    // And the rail computed no total, because it cannot.
    assert!(
        log.get("balances").is_none(),
        "the rail invented a reading: {log}"
    );
}

/// The journal survives the process. A ring app that loses a month of acts
/// on a daemon restart is not a journal.
#[tokio::test]
async fn the_journal_outlives_the_state_that_wrote_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    for _ in 0..2 {
        let state = with_guest(
            state_with_rail(dir.path(), &key),
            vec![Scope::Rails(NS.into())],
        );
        let (status, body) = call(
            state,
            request(
                "POST",
                "/v1/rail/append",
                LAN_PEER,
                Some(GUEST_TOKEN),
                Some(groceries()),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (_, log) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(log["held"], 2, "both writes, across two AppStates");
    assert_eq!(log["ops"].as_array().unwrap().len(), 2);
    assert_eq!(log["complete"], true, "gaps: {}", log["gaps"]);
}

// ── the refusals ─────────────────────────────────────────────

/// A rail grant reaches its namespace and nothing else. The app cannot even
/// *name* another one: asking is a refusal, not a silent redirect.
#[tokio::test]
async fn an_app_cannot_reach_another_apps_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (status, body) = call(
        state,
        request(
            "GET",
            "/v1/rail/log?namespace=tool-lending",
            LAN_PEER,
            Some(GUEST_TOKEN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// The same token that works on the rail must be refused everywhere else.
/// These are the paths a compromised ring app would reach for.
#[tokio::test]
async fn the_rail_token_is_refused_on_every_privileged_path() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    for path in [
        "/internal/guest/grant",
        "/v1/mesh/status",
        "/v1/apps",
        "/v1/models",
        "/v1/chat/completions",
    ] {
        let state = with_guest(
            state_with_rail(dir.path(), &key),
            vec![Scope::Rails(NS.into())],
        );
        let (status, _) = call(
            state,
            request("GET", path, LAN_PEER, Some(GUEST_TOKEN), None),
        )
        .await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
            "a rail grant reached {path} with {status}"
        );
    }
}

/// Revocation is immediate — there is no window behind the reaper.
#[tokio::test]
async fn a_revoked_rail_grant_fails_closed_on_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (before, _) = call(
        state.clone(),
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(before, StatusCode::OK);

    state.inner.node.guest_grants.revoke(GUEST_TOKEN);
    let (after, _) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(after, StatusCode::UNAUTHORIZED);
}

/// **A daemon with no ring storage refuses.** It must not answer an empty
/// journal: "this daemon cannot keep a journal" and "your ring is empty" are
/// different facts, and collapsing them hands the app a plausible zero
/// (ARCH §18.3).
#[tokio::test]
async fn a_daemon_without_ring_storage_refuses_rather_than_answering_empty() {
    let state = with_guest(bare_state(), vec![Scope::Rails(NS.into())]);
    let (status, body) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body["error"].as_str().unwrap().contains("no ring storage"));
}

/// **The rail refuses what it can judge, in a sentence, and writes nothing.**
///
/// What it can judge is narrow now — an act's *meaning* is the app's — but a
/// payload with no canonical form is the rail's business, because two nodes
/// have to derive identical bytes from it. The assertion on the body text is
/// the point: this string reaches a housemate, and it returned a raw serde
/// dump at one before the gap renderer existed.
#[tokio::test]
async fn a_payload_with_no_canonical_form_is_refused_in_a_sentence() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (status, body) = call(
        state.clone(),
        request(
            "POST",
            "/v1/rail/append",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(serde_json::json!({
                "op": "record",
                "payload": { "kind": "expense", "amount": 24.5 },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let why = body["error"].as_str().unwrap_or_default();
    assert!(
        why.contains("whole number"),
        "not a sentence a person can act on: {why}"
    );
    assert!(!why.contains('{'), "rendered as a dump: {why}");

    let (_, log) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(log["ops"].as_array().unwrap().len(), 0);
}

/// The complement, and the trade this refactor made explicit: an act that is
/// nonsense to the app it belongs to is still a well-formed act to the rail,
/// and the rail writes it. Judging it would mean the rail knowing what an
/// expense is, which is exactly what it stopped knowing.
#[tokio::test]
async fn an_act_the_app_would_refuse_is_still_the_apps_problem_not_the_rails() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (status, body) = call(
        state,
        request(
            "POST",
            "/v1/rail/append",
            LAN_PEER,
            Some(GUEST_TOKEN),
            Some(serde_json::json!({
                "op": "record",
                // An expense for nothing, split between nobody. The reference
                // app's `validate` refuses this; the rail cannot see it.
                "payload": {
                    "kind": "expense",
                    "payer": "alex",
                    "amount_cents": 0,
                    "participants": [],
                },
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// An operator reached this daemon on a listener that already trusts them, so
/// they have no grant — and must therefore name the namespace. An unnamed one
/// is refused rather than defaulted to something plausible.
#[tokio::test]
async fn an_operator_names_the_namespace_and_is_refused_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key);

    let (named, body) = call(
        state.clone(),
        request(
            "GET",
            &format!("/v1/rail/log?namespace={NS}"),
            LOOPBACK,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(named, StatusCode::OK, "{body}");
    assert_eq!(body["namespace"], NS);

    let (unnamed, _) = call(state, request("GET", "/v1/rail/log", LOOPBACK, None, None)).await;
    assert_eq!(unnamed, StatusCode::BAD_REQUEST);
}

/// A namespace names a directory. `..` must be refused at the door rather
/// than sanitised somewhere downstream (ARCH §7.1).
#[tokio::test]
async fn a_namespace_that_is_a_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = state_with_rail(dir.path(), &key);
    let (status, body) = call(
        state,
        request(
            "GET",
            "/v1/rail/log?namespace=..%2f..%2fetc",
            LOOPBACK,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

// The replication + sealing half lives in a sibling file: together they put
// this one into the 800-1200 approach band (ARCH §3.1).
mod replication;

// `sync_raw` / `sync_once` moved with the replication suite; the sibling
// suites below reach them through `use super::*`, as they always did.
use replication::{sync_once, sync_raw};

mod ceiling;

// The ring-sync route's OWN refusal, driven at the handler because the gate in
// front never lets the case reach the mounted route.
mod roster_refusal;

// The guest door rides the same helpers: the rail on a LAN-reachable bind.
mod guest_door;
