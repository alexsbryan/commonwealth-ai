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
use commonwealth_api::routes_internal::{
    RingSyncRequest, RingSyncResponse, RING_SYNC_OPS_BUDGET_BYTES,
};
use commonwealth_api::server::client_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_knowledge::Scope;
use commonwealth_rail::{
    Digest, Ed25519Verifier, Op, Payload, Person, RailAct, RingJournal, RingRail, RingSigner,
    Roster, SignedOp,
};
use ed25519_dalek::SigningKey;
use tower::ServiceExt;

const LOOPBACK: &str = "127.0.0.1:55001";
const LAN_PEER: &str = "192.168.1.50:44444";
const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";
const GUEST_TOKEN: &str = "9c1f7b2ea4d68053aa11ff7c3e5b90d4c7a2f16b8e04d93b5c7a1e2f3b4d5c6a";
const NS: &str = "house-expenses";

fn bare_state() -> AppState {
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
    let state = AppState::new(node, mesh);
    state.install_client_token(Some(Arc::<str>::from(TOKEN)));
    state
}

/// A daemon with ring storage under `root`, signing as `key`, and a roster
/// that says that key is Alex.
fn state_with_rail(root: &std::path::Path, key: &SigningKey) -> AppState {
    let state = bare_state();
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
    state.install_ring_rail(rail);
    state
}

fn with_guest(state: AppState, scopes: Vec<Scope>) -> AppState {
    let now = commonwealth_core::clock::unix_now_millis();
    state
        .inner
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

    state.inner.guest_grants.revoke(GUEST_TOKEN);
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

// ── replication, through the real route ──────────────────────

/// One `/internal/ring/sync` exchange, in the wire types the mesh loop
/// itself uses — `RingSyncRequest` in, `RingSyncResponse` out.
///
/// It said "mirrors exactly what `sovereign-mesh`'s loop does" while driving
/// the route through `serde_json::Value`, which made this a THIRD spelling of
/// a shape that already has exactly one (ARCH §10.6): the handler declares
/// it, `sovereign_mesh::ring_sync` imports that declaration, and a field
/// renamed on the struct would have left this harness green while the mesh
/// loop stopped converging. Typed, the compiler is the mirror.
///
/// Returns the raw status alongside the body because the ceiling drill below
/// needs the exchange that is REFUSED, and a helper that asserts 200 can only
/// ever see the half that works.
async fn sync_raw(
    responder: AppState,
    req: &RingSyncRequest,
) -> (StatusCode, Option<RingSyncResponse>) {
    let http = Request::builder()
        .method("POST")
        .uri("/internal/ring/sync")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(req).unwrap()))
        .unwrap();
    let resp = commonwealth_api::server::internal_router(responder)
        .oneshot(http)
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).ok())
}

/// [`sync_raw`] for the exchanges that are supposed to succeed.
async fn sync_once(
    responder: AppState,
    namespace: &str,
    digest: Digest,
    ops: Vec<Op<SignedOp>>,
) -> RingSyncResponse {
    let (status, body) = sync_raw(
        responder,
        &RingSyncRequest {
            namespace: namespace.to_string(),
            digest,
            ops,
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body.expect("a 200 must carry a RingSyncResponse")
}

/// **The partition drill, over HTTP.** Two nodes, two journals, two keys.
/// Each writes while partitioned; one exchange each way heals them; both then
/// read the same acts, in the same order, with no gaps.
#[tokio::test]
async fn two_nodes_converge_through_the_sync_route() {
    let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (key_a, key_b) = (
        SigningKey::from_bytes(&[1u8; 32]),
        SigningKey::from_bytes(&[2u8; 32]),
    );
    // Both nodes carry the same roster — the roster is a parameter of
    // admission, so two nodes disagreeing about it would admit different ops.
    let roster = {
        let mut m = std::collections::BTreeMap::new();
        m.insert(Person::from("alex"), vec![key_a.actor()]);
        m.insert(Person::from("bo"), vec![key_b.actor()]);
        Roster::new(m)
    };
    let build = |dir: &std::path::Path, key: &SigningKey| {
        let state = bare_state();
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        rail.journal(NS).unwrap().set_roster(&roster).unwrap();
        state.install_ring_rail(rail.clone());
        (state, rail)
    };
    let (state_a, rail_a) = build(dir_a.path(), &key_a);
    let (state_b, rail_b) = build(dir_b.path(), &key_b);
    let (led_a, led_b) = (rail_a.journal(NS).unwrap(), rail_b.journal(NS).unwrap());

    // Partitioned writes.
    led_a
        .append(
            RailAct::Record {
                payload: expense_payload("alex", 6000, "groceries"),
            },
            &key_a,
            &roster,
        )
        .unwrap();
    led_b
        .append(
            RailAct::Record {
                payload: expense_payload("bo", 2000, "beer"),
            },
            &key_b,
            &roster,
        )
        .unwrap();
    assert_ne!(
        led_a.admit(&roster, &Ed25519Verifier).unwrap(),
        led_b.admit(&roster, &Ed25519Verifier).unwrap(),
        "the fixture must actually be partitioned"
    );

    // A dials B. Call 1 pulls; call 2 pushes what B's digest says it lacks.
    let first = sync_once(state_b.clone(), NS, led_a.digest().unwrap(), Vec::new()).await;
    assert_eq!(led_a.ingest_all(&first.ops).unwrap(), 1);

    let for_b = led_a.ops_missing_from(&first.digest).unwrap();
    assert_eq!(for_b.len(), 1);
    let second = sync_once(state_b.clone(), NS, led_a.digest().unwrap(), for_b).await;
    assert_eq!(second.ingested, 1);

    // One answer, on both nodes, with nothing missing. The answer is the ACT
    // ORDER — which is everything the rail promises, and everything an app's
    // reducer needs in order to agree with its housemates.
    let (fa, fb) = (
        led_a.admit(&roster, &Ed25519Verifier).unwrap(),
        led_b.admit(&roster, &Ed25519Verifier).unwrap(),
    );
    assert_eq!(fa, fb, "two nodes, one answer");
    assert!(fa.is_complete(), "{:?}", fa.gaps);
    assert_eq!(fa.ops.len(), 2);
    // Sorted, because the order these two land in is CONTENT-derived and not
    // write-derived: both were written in the same second, so the tie breaks
    // on the signing key and neither node's local history wins. That is the
    // property — asserting a literal sequence here would be asserting
    // something about two fixture keypairs.
    let mut who: Vec<&str> = fa.ops.iter().map(|o| o.person.as_str()).collect();
    who.sort();
    assert_eq!(who, vec!["alex", "bo"], "both acts, both attributed");

    // Steady state: another exchange moves nothing and changes nothing.
    let again = sync_once(state_b, NS, led_a.digest().unwrap(), Vec::new()).await;
    assert!(again.ops.is_empty());
    assert_eq!(again.ingested, 0);
    assert_eq!(led_a.admit(&roster, &Ed25519Verifier).unwrap(), fa);

    // And the ring app on A now sees B's expense through the rail it can
    // reach — which is the whole point of replicating at all.
    let app = with_guest(state_a, vec![Scope::Rails(NS.into())]);
    let (status, log) = call(
        app,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert_eq!(log["ops"].as_array().unwrap().len(), 2);
    assert_eq!(log["complete"], true, "gaps: {}", log["gaps"]);
}

/// A node with no ring storage refuses the exchange rather than answering an
/// empty digest — which would tell the peer it holds nothing and stop the
/// peer from ever offering it ops.
#[tokio::test]
async fn a_node_without_ring_storage_refuses_the_exchange() {
    let req = Request::builder()
        .method("POST")
        .uri("/internal/ring/sync")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "namespace": NS })).unwrap(),
        ))
        .unwrap();
    let resp = commonwealth_api::server::internal_router(bare_state())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// The convergence ceiling and the byte budget that ended it live in their
// own module: this suite crossed ARCH §3.2's 1200-line ceiling when rung 2f
// landed the chunking tests. Split rather than re-baselined, and kept as ONE
// test binary (`tests/rail_e2e/main.rs`) rather than a second top-level
// `tests/*.rs` — binary count is its own budget (order build-1).
mod ceiling;
