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

// ── the convergence ceiling, and the budget that ended it ────
//
// `ops_missing_from` returned everything a peer lacks in ONE body, and the
// receiver caps a request body at `MAX_REQUEST_BODY_BYTES`. Past ~9,599 ops
// of the fixture below those two met, and the meeting was silent: the 413 is
// answered at the extractor, so the handler never ran, the gauge it carries
// could not fire, and the sender filed a reachable peer as unreachable at
// DEBUG. The peer that had been refused the journal reported zero ops, zero
// gaps and a COMPLETE ring.
//
// `RING_SYNC_OPS_BUDGET_BYTES` stops the selection at a byte budget and the
// exchange repeats, so one body is no longer the unit of convergence.
// **Nothing on the wire changed shape** — the exchange was always idempotent
// — which is why every per-body figure below is unchanged and only what they
// MEAN has moved: they now size a chunk, not a ceiling.
//
// THE FIGURES ARE IN BYTES, NOT OPS, so every one names its fixture.
// Measured on this host with the production types (railread, release, and
// reproduced by `one_budgeted_chunk_carries_less_than_a_whole_journal`):
//
//   fixture body   B/op on the wire   ops in 8 MiB
//   594 B (order 2 §2's ledger)   873   9,608
//   ~332 B (an expense row)       609  13,774
//   ~274 B (a work-atlas obs.)    552  15,196
//
// The 594-byte fixture is the one carried here because it is the one order 2
// priced, and it puts the figure nearest the round number the order named.

/// A `Payload` whose serialised `RailAct::Record` body is at least `target`
/// bytes. Constructed rather than guessed: the envelope overhead is measured
/// by `body_json` on each pass instead of being hard-coded, so a change to
/// the act's wire shape moves the fixture with it.
fn body_of_size(target: usize) -> Payload {
    let mut filler = target.saturating_sub(40);
    loop {
        let payload = Payload::new(serde_json::json!({ "b": "x".repeat(filler) })).unwrap();
        let got = commonwealth_rail::body_json(&RailAct::Record {
            payload: payload.clone(),
        })
        .len();
        if got >= target {
            return payload;
        }
        filler += target - got;
    }
}

/// The fixture order 2 §2's ceiling table is quoted against.
const CEILING_FIXTURE_BODY_BYTES: usize = 594;

/// `n` real signed ops on disk, one `append_all`. Every op is signed for its
/// own `seq` and `ts`, so each carries a distinct content-derived `OpId` and
/// a signature that actually verifies — a fixture of clones would make the
/// convergence assertion below meaningless.
fn seed(journal: &RingJournal, key: &SigningKey, n: usize) -> usize {
    let payload = body_of_size(CEILING_FIXTURE_BODY_BYTES);
    let ops: Vec<_> = (0..n as u64)
        .map(|seq| {
            signed(
                key,
                journal.namespace(),
                seq,
                RailAct::Record {
                    payload: payload.clone(),
                },
            )
        })
        .collect();
    journal.ingest_all(&ops).unwrap()
}

/// One op, signed for its own `(namespace, ts, seq)` exactly as the door
/// signs it — so its `OpId` is distinct and its signature actually verifies.
fn signed(key: &SigningKey, namespace: &str, seq: u64, act: RailAct) -> Op<SignedOp> {
    let ts = 1_700_000_000i64 + seq as i64;
    let sig = commonwealth_rail::sign_ring_op(
        key,
        namespace,
        ts,
        seq,
        &commonwealth_rail::body_json(&act),
    );
    Op::new(SignedOp { seq, sig, act }, ts, key.actor())
}

fn solo_roster(key: &SigningKey) -> Roster {
    let mut m = std::collections::BTreeMap::new();
    m.insert(Person::from("alex"), vec![key.actor()]);
    Roster::new(m)
}

/// A daemon holding a ring under `dir`, signing as `key`. `n = 0` leaves it a
/// node that has never seen this ring at all — no `rings/<ns>` directory,
/// which is the state that matters below.
fn node(
    dir: &std::path::Path,
    key: &SigningKey,
    n: usize,
) -> (AppState, std::sync::Arc<RingJournal>) {
    let state = bare_state();
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let journal = rail.journal(NS).unwrap();
    if n > 0 {
        journal.set_roster(&solo_roster(key)).unwrap();
        assert_eq!(seed(&journal, key, n), n, "the fixture must land in full");
    }
    state.install_ring_rail(rail);
    (state, journal)
}

/// **The measurement, not arithmetic.** Pins the per-op wire cost of the
/// fixture the other tests use, and derives from it what one budgeted CHUNK
/// carries — so a change to the op envelope or to the budget moves the number
/// here, where it is one assertion, rather than silently un-chunking the
/// convergence tests below.
///
/// All three bounds are alarms rather than descriptions:
///
/// - **860-890 B/op** is the envelope. It is what every figure in the table
///   above is quoted against.
/// - **Under 10,000 ops in one body** is 2a's ceiling. It has NOT moved and
///   is not supposed to — the fix was to stop making one body the unit of
///   convergence, not to make the body bigger.
/// - **Under 10,000 ops in one chunk** is what keeps the tests below honest.
///   If a chunk ever carried a whole 10,000-op journal, every convergence
///   assertion here would pass without a second chunk ever being sent.
#[tokio::test]
async fn one_budgeted_chunk_carries_less_than_a_whole_journal() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    const N: usize = 500;
    let (_state, journal) = node(dir.path(), &key, N);

    let body = RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops: journal.ops_missing_from(&Digest::new()).unwrap(),
    };
    assert_eq!(body.ops.len(), N, "an empty digest asks for everything");
    let per_op = serde_json::to_vec(&body).unwrap().len() / N;
    let one_body = commonwealth_api::server::MAX_REQUEST_BODY_BYTES / per_op;
    let per_chunk = RING_SYNC_OPS_BUDGET_BYTES / per_op;

    assert!(
        (860..=890).contains(&per_op),
        "the {CEILING_FIXTURE_BODY_BYTES}-byte fixture costs {per_op} B/op on \
         the wire, not the ~873 the ceiling table assumes"
    );
    assert!(
        one_body < 10_000,
        "one body now holds {one_body} ops — the envelope or the limit moved, \
         and every figure in the table above is stale"
    );
    assert!(
        (1..10_000).contains(&per_chunk),
        "one chunk carries {per_chunk} ops — the convergence tests below stop \
         exercising a second chunk"
    );
}

/// **The sender's loop, driven against the real route.**
///
/// The production spelling is `sovereign_mesh::ring_sync::exchange`, which
/// this harness cannot call: that one needs a `reqwest` client against a live
/// socket and this dispatches through `tower::oneshot`. (It is tested against
/// a real listener, in that crate, beside the loop it drives.) What is NOT
/// duplicated here is the DECISION — the chunk comes from the production
/// `ops_missing_from_within` at the production `RING_SYNC_OPS_BUDGET_BYTES`,
/// so a budget that stopped binding would fail these tests too.
///
/// Returns `(chunks, ops the peer reported as new)`.
async fn push_until_converged(peer: AppState, journal: &RingJournal) -> (usize, usize) {
    let mut chunks = 0usize;
    let mut pushed = 0usize;
    loop {
        // Call 1 — learn what they hold. A digest is ~600 bytes whatever the
        // journal weighs, which is why this half was never the problem.
        let theirs = sync_once(peer.clone(), NS, journal.digest().unwrap(), Vec::new()).await;
        let (ops, more) = journal
            .ops_missing_from_within(&theirs.digest, RING_SYNC_OPS_BUDGET_BYTES)
            .unwrap();
        if ops.is_empty() {
            assert!(!more, "nothing to send cannot also mean more remains");
            return (chunks, pushed);
        }
        let offered = ops.len();
        let push = RingSyncRequest {
            namespace: NS.to_string(),
            digest: journal.digest().unwrap(),
            ops,
        };
        let bytes = serde_json::to_vec(&push).unwrap().len();
        let (status, body) = sync_raw(peer.clone(), &push).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "chunk {chunks} was {bytes} B and was refused — the budget exists \
             to keep every chunk under the {} B limit",
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
        );
        let ingested = body.expect("a 200 carries a response").ingested;
        assert!(
            ingested > 0,
            "chunk {chunks} offered {offered} ops and moved none — a chunk \
             whose first op the peer already holds is the spin the contiguous \
             mark is supposed to make impossible"
        );
        pushed += ingested;
        chunks += 1;
        assert!(chunks < 64, "the push did not converge in 64 chunks");
    }
}

/// **The limit is still real; the budget is what keeps us under it.**
///
/// Nothing about the receiver got more permissive in 2f, and this is where
/// that is nailed down — otherwise "it converges now" could as easily mean
/// somebody raised `MAX_REQUEST_BODY_BYTES` and moved the same failure a
/// factor of two out.
///
/// - The whole selection in ONE body, the shape the loop sent until 2f, is
///   still refused at 10,000 ops.
/// - The 9,000-op arm is the negative control: a body under the limit is
///   served, so the 413 is evidence about SIZE and not about a route that
///   refuses everything.
/// - The same 10,000-op journal offered as a budgeted CHUNK — what the loop
///   sends now — is served.
#[tokio::test]
async fn the_budgeted_chunk_is_served_where_the_whole_journal_is_refused() {
    let key = SigningKey::from_bytes(&[1u8; 32]);

    for (n, want) in [
        (9_000usize, StatusCode::OK),
        (10_000, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let sender_dir = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let (_sender, journal) = node(sender_dir.path(), &key, n);
        // A node that has never seen this ring: it holds nothing, so
        // `ops_missing_from(&empty)` is the sender's whole journal.
        let (peer, _) = node(receiver.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);

        let push = RingSyncRequest {
            namespace: NS.to_string(),
            digest: journal.digest().unwrap(),
            ops: journal.ops_missing_from(&Digest::new()).unwrap(),
        };
        let bytes = serde_json::to_vec(&push).unwrap().len();
        let (status, _) = sync_raw(peer, &push).await;
        assert_eq!(
            status,
            want,
            "{n} unbudgeted ops = {bytes} B against a {} B limit",
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
        );
    }

    // And the same journal, budgeted the way the loop sends it.
    let sender_dir = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    let (_sender, journal) = node(sender_dir.path(), &key, 10_000);
    let (peer, _) = node(receiver.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    let (ops, more) = journal
        .ops_missing_from_within(&Digest::new(), RING_SYNC_OPS_BUDGET_BYTES)
        .unwrap();
    assert!(
        more,
        "the control: 10,000 ops must not fit one chunk, or the arm below \
         proves only that a small body is served"
    );
    let push = RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops,
    };
    let bytes = serde_json::to_vec(&push).unwrap().len();
    let (status, _) = sync_raw(peer, &push).await;
    assert_eq!(status, StatusCode::OK, "the budgeted chunk was {bytes} B");
}

/// **The defect 2a named, flipped into the alarm.**
///
/// This test used to assert the defect: a peer refused the journal reported
/// zero ops, zero gaps and `is_complete()` TRUE. "I hold the whole journal
/// and there is nothing in it" and "the push that would have given me the
/// journal was refused" are different facts, and one 413 at the extractor
/// collapsed them into the plausible one (ARCH §18.3).
///
/// The same 10,000-op journal, over the same route, now converges — because
/// the exchange is budgeted and repeated rather than made bigger.
///
/// **The bootstrap case is the one that had to work.** A node that has never
/// seen this ring has no `rings/<ns>/` directory, so `run_one_round`
/// enumerates nothing and returns before dialling anybody
/// (`sovereign-mesh/src/ring_sync.rs:117`, `:124`): it can only ever be TOLD,
/// over the direction that has a body limit. This peer is that node, and the
/// assertion below is that being told now works at any journal size.
///
/// Watched RED against the unbudgeted shape (`RING_SYNC_OPS_BUDGET_BYTES`
/// raised past the body limit): the first chunk comes back 413 and the fold
/// reports the empty, complete ring the paragraph above describes.
#[tokio::test]
async fn a_journal_past_the_old_ceiling_converges_onto_a_node_that_has_never_seen_the_ring() {
    const N: usize = 10_000;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let sender_dir = tempfile::tempdir().unwrap();
    let peer_dir = tempfile::tempdir().unwrap();
    let (_sender, journal) = node(sender_dir.path(), &key, N);
    let (peer_state, peer_journal) = node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    peer_journal.set_roster(&solo_roster(&key)).unwrap();

    let (chunks, pushed) = push_until_converged(peer_state, &journal).await;
    assert!(
        chunks > 1,
        "the control: {N} ops must take more than one chunk, or this passes \
         without ever exercising the repeat"
    );
    assert_eq!(pushed, N, "every op landed, in {chunks} chunks");

    let after = peer_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert_eq!(after.ops.len(), N, "the whole journal arrived");
    assert!(
        after.is_complete(),
        "a chunked bootstrap is not a holed one: {:?}",
        after.gaps
    );
    assert_eq!(
        peer_journal.digest().unwrap(),
        journal.digest().unwrap(),
        "two nodes, one claim"
    );
}

/// **The response is budgeted too, and it had to be.**
///
/// `DefaultBodyLimit` bounds the REQUEST. The same journal used to come back
/// in a RESPONSE with no cap at all, which sounded like a free rescue and was
/// two problems: the direction that could not bootstrap a fresh peer was also
/// the one direction nothing bounded, on a route every peer that can route to
/// this host may call. So the budget applies to both, and the pull converges
/// by repeating exactly as the push does.
///
/// The bound is asserted every round rather than once, because a response
/// that fits on round one and not on round three is the failure that would
/// otherwise ship.
#[tokio::test]
async fn the_pull_direction_is_budgeted_and_converges_by_repeating() {
    const N: usize = 10_000;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let holder_dir = tempfile::tempdir().unwrap();
    let (holder, journal) = node(holder_dir.path(), &key, N);

    let fresh_dir = tempfile::tempdir().unwrap();
    let (_fresh, fresh_journal) = node(fresh_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    fresh_journal.set_roster(&solo_roster(&key)).unwrap();

    let mut rounds = 0usize;
    loop {
        // A fresh peer's own digest, no ops offered — the pull direction.
        let body = sync_once(
            holder.clone(),
            NS,
            fresh_journal.digest().unwrap(),
            Vec::new(),
        )
        .await;
        let bytes = serde_json::to_vec(&body).unwrap().len();
        assert!(
            bytes <= commonwealth_api::server::MAX_REQUEST_BODY_BYTES,
            "round {rounds}: the response was {bytes} B against a {} B limit — \
             the pull direction is unbounded again",
            commonwealth_api::server::MAX_REQUEST_BODY_BYTES
        );
        if body.ops.is_empty() {
            break;
        }
        assert!(
            fresh_journal.ingest_all(&body.ops).unwrap() > 0,
            "round {rounds} carried ops the puller already held — the spin"
        );
        rounds += 1;
        assert!(rounds < 64, "the pull did not converge in 64 rounds");
    }
    assert!(
        rounds > 1,
        "the control: {N} ops must take more than one pull"
    );

    let admitted = fresh_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert_eq!(admitted.ops.len(), N);
    assert!(admitted.is_complete(), "gaps: {:?}", admitted.gaps);
    assert_eq!(
        fresh_journal.digest().unwrap(),
        journal.digest().unwrap(),
        "two nodes, one answer"
    );
}

/// **Does the sealed floor (`dc9010181`) reduce what goes on the wire? Only
/// the DELETE does, and nothing in the tree performs it.**
///
/// Two arms over the same journal, and the pair is the finding:
///
/// - **A seal alone buys nothing on the wire.** `ops_missing_from` is
///   author-blind over what this node HOLDS, and a peer with an empty digest
///   is missing all of it, so appending a `Seal` while the retired lines are
///   still on disk sends exactly the same ops — now over several chunks
///   instead of one refused body, which is cheaper in nothing but failure.
/// - **Deleting the retired lines is the whole mitigation.** The same push
///   after compaction carries the suffix only, in ONE chunk, and lands a
///   fresh peer on a ring `admit` calls COMPLETE — the floor travels as the
///   seal's own op, so what was retired is absent by agreement rather than a
///   hole.
///
/// The delete is done here, by this test, because there is no production
/// routine that does it: `RailAct::Seal` is reachable through `append`, and
/// `grep -rn "compact\|truncate"` over the rail crates, the API and
/// `ring_cmd` finds no writer that removes a retired prefix and no verb that
/// authors a seal. The chunked exchange makes an uncompacted journal
/// *converge*; the seal plus a delete is what makes it *small*, and those are
/// different wins.
#[tokio::test]
async fn a_seal_shortens_the_exchange_only_once_the_retired_lines_are_deleted() {
    const RETIRED: u64 = 10_000;
    const KEPT: u64 = 500;
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let (_holder, journal) = node(dir.path(), &key, RETIRED as usize);

    // The seal takes the actor's next ordinary seq, then work continues.
    let payload = body_of_size(CEILING_FIXTURE_BODY_BYTES);
    let mut tail = vec![signed(&key, NS, RETIRED, RailAct::Seal)];
    tail.extend((RETIRED + 1..=RETIRED + KEPT).map(|seq| {
        signed(
            &key,
            NS,
            seq,
            RailAct::Record {
                payload: payload.clone(),
            },
        )
    }));
    assert_eq!(journal.ingest_all(&tail).unwrap(), tail.len());

    let push = |journal: &RingJournal| RingSyncRequest {
        namespace: NS.to_string(),
        digest: journal.digest().unwrap(),
        ops: journal.ops_missing_from(&Digest::new()).unwrap(),
    };

    // ARM 1 — sealed, not compacted. The selection is unchanged, so the
    // exchange still costs several chunks.
    let before = push(&journal);
    assert_eq!(before.ops.len(), (RETIRED + KEPT + 1) as usize);
    let peer_dir = tempfile::tempdir().unwrap();
    let (peer, peer_journal) = node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    peer_journal.set_roster(&solo_roster(&key)).unwrap();
    let (sealed_chunks, sealed_pushed) = push_until_converged(peer, &journal).await;
    assert_eq!(sealed_pushed, (RETIRED + KEPT + 1) as usize);
    assert!(
        sealed_chunks > 1,
        "a seal is an op, not a delete: {} ops still go out",
        before.ops.len()
    );

    // ARM 2 — the delete the rail has no routine for. Same filter shape as
    // `commonwealth-rail`'s own journal drill: keep the seal and everything
    // above it, keep anything unparseable rather than losing it.
    let path = journal.dir().join("ring_oplog.jsonl");
    let kept: String = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| {
            serde_json::from_str::<Op<SignedOp>>(l)
                .map(|o| o.kind.seq >= RETIRED)
                .unwrap_or(true)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&path, kept).unwrap();
    assert_eq!(
        journal.read().unwrap().0.len(),
        (KEPT + 1) as usize,
        "the prefix is really gone"
    );

    let after = push(&journal);
    let peer_dir = tempfile::tempdir().unwrap();
    let (peer, peer_journal) = node(peer_dir.path(), &SigningKey::from_bytes(&[2u8; 32]), 0);
    peer_journal.set_roster(&solo_roster(&key)).unwrap();
    assert_eq!(after.ops.len(), (KEPT + 1) as usize);
    let (compacted_chunks, compacted_pushed) = push_until_converged(peer, &journal).await;
    assert_eq!(
        compacted_pushed,
        (KEPT + 1) as usize,
        "the suffix fits and lands"
    );
    assert_eq!(
        compacted_chunks, 1,
        "and it lands in ONE chunk, against {sealed_chunks} before the delete"
    );

    // And the peer that has never seen this ring reads a COMPLETE journal:
    // the seal came with it, so `admit` counts holes from the floor.
    let admitted = peer_journal
        .admit(&solo_roster(&key), &Ed25519Verifier)
        .unwrap();
    assert!(
        admitted.is_complete(),
        "a compacted bootstrap is not a broken one: {:?}",
        admitted.gaps
    );
    assert_eq!(
        admitted.applied().count(),
        KEPT as usize,
        "the seal itself carries no payload"
    );
    assert_eq!(
        peer_journal.digest().unwrap(),
        journal.digest().unwrap(),
        "two nodes, one claim"
    );
}

// ── the rail listener, where the grant is the only way in ────

/// Drive the RAIL bind rather than the operator one.
async fn call_rail(state: AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = commonwealth_api::server::client_router_for(
        state,
        commonwealth_api::server::ClientSurface::Rail,
    )
    .oneshot(req)
    .await
    .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// **Watched failing first, and it is the reason the rail is a separate bind.**
///
/// A ring app is a process on this machine, so it arrives on loopback — and on
/// the operator bind that alone admits it, before any bearer is read. Pointed
/// there, an app would arrive as an OPERATOR and its grant would be ignored:
/// the namespace scoping would be decorative, and a guard nobody can watch
/// fail is not a guard (§18.1). On the rail bind the token is the only way in.
#[tokio::test]
async fn on_the_rail_bind_a_loopback_caller_without_a_grant_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );

    let (no_token, _) = call_rail(
        state.clone(),
        request("GET", "/v1/rail/log", LOOPBACK, None, None),
    )
    .await;
    assert_eq!(
        no_token,
        StatusCode::UNAUTHORIZED,
        "loopback alone must not admit on the rail bind"
    );

    // The same request WITH the grant is served, and served the right
    // namespace — which the caller never named.
    let (with_token, body) = call_rail(
        state,
        request("GET", "/v1/rail/log", LOOPBACK, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(with_token, StatusCode::OK, "{body}");
    assert_eq!(body["namespace"], NS);
}

/// The rail bind serves the rail and nothing else, proven from BOTH sides.
///
/// Two independent mechanisms refuse here and a test that only saw one could
/// be satisfied while the other silently broke:
///
/// - **A rail grant** gets 403 on everything outside `/v1/rail/*`, because
///   `Scope::paths()` is the allowlist and the auth layer checks it before
///   routing. That is what stops grants minting grants.
/// - **The daemon token** bypasses scope entirely — so a 404 for the same
///   paths is evidence about the ROUTER: those routes are not mounted on this
///   surface at all (§7.1). If they were, this arm would answer 200.
#[tokio::test]
async fn the_rail_bind_serves_nothing_but_the_rail() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let elsewhere = [
        "/internal/guest/grant",
        "/v1/models",
        "/v1/chat/completions",
        "/v1/mesh/status",
    ];
    for path in elsewhere {
        let state = with_guest(
            state_with_rail(dir.path(), &key),
            vec![Scope::Rails(NS.into())],
        );
        let (scoped, _) = call_rail(
            state.clone(),
            request("GET", path, LOOPBACK, Some(GUEST_TOKEN), None),
        )
        .await;
        assert_eq!(scoped, StatusCode::FORBIDDEN, "a rail grant reached {path}");

        // The same path under a credential that has no scope limit at all.
        let (unscoped, _) =
            call_rail(state, request("GET", path, LOOPBACK, Some(TOKEN), None)).await;
        assert_eq!(
            unscoped,
            StatusCode::NOT_FOUND,
            "{path} is MOUNTED on the rail surface — the route set, not the \
             grant, is what must exclude it"
        );
    }

    // `/status` is the documented odd one out and gets its own case.
    // `AUTH_EXEMPT_PATHS` lets it past the auth layer WITHOUT a scope check,
    // so it reaches the router — where the rail surface does not mount it. A
    // probe against a rail listener therefore sees 404, not 401, which reads
    // as "wrong node" rather than "wrong credential". That is a known cost of
    // the split, written down here so the next person to debug it does not
    // spend the afternoon on a credential that was never the problem.
    for bearer in [GUEST_TOKEN, TOKEN] {
        let state = with_guest(
            state_with_rail(dir.path(), &key),
            vec![Scope::Rails(NS.into())],
        );
        let (status, _) = call_rail(
            state,
            request("GET", "/status", LOOPBACK, Some(bearer), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "/status under {bearer}");
    }

    // And the rail itself is served on this bind, so the assertions above are
    // not passing because the whole router is empty.
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let (ok, _) = call_rail(
        state,
        request("GET", "/v1/rail/log", LOOPBACK, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}
