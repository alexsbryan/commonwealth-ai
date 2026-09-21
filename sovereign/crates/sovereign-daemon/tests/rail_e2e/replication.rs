// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replication through the real `/internal/ring/sync` route, and sealing
//! through the append door.
//!
//! Split out of `main.rs`, which the ring-rail work pushed into the
//! 800-1200 approach band (ARCH §3.1). The fixtures stay in `main.rs`.

use super::*;

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
pub(crate) async fn sync_raw(
    responder: AppState,
    req: &RingSyncRequest,
) -> (StatusCode, Option<RingSyncResponse>) {
    let http = Request::builder()
        .method("POST")
        .uri("/internal/ring/sync")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        // `internal_gate` reads a MISSING `ConnectInfo` as "not loopback" and
        // refuses. Both internal listeners attach one in production, so a
        // driver without it is a shape that never occurs; say the local one.
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 54321))))
        .body(Body::from(serde_json::to_vec(req).unwrap()))
        .unwrap();
    let resp = sovereign_daemon::server::internal_router(responder)
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
pub(crate) async fn sync_once(
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
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        rail.journal(NS).unwrap().set_roster(&roster).unwrap();
        let state = bare_state_with_seed(
            sovereign_daemon::state::FabricSeed {
                ring_rail: Some(rail.clone()),
                ..Default::default()
            },
            sovereign_grants::GuestSessionBinding::Door,
            Default::default(),
        );
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
            None,
        )
        .unwrap();
    led_b
        .append(
            RailAct::Record {
                payload: expense_payload("bo", 2000, "beer"),
            },
            &key_b,
            &roster,
            None,
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
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 54321))))
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "namespace": NS })).unwrap(),
        ))
        .unwrap();
    let resp = sovereign_daemon::server::internal_router(bare_state())
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
/// **An app seals through the door it already writes with, and the journal
/// gets shorter.** No second verb, no capability an app has to be granted
/// separately, no local setting — `{"op":"seal"}` is an act like any other, it
/// takes the author's next `seq`, and the prune it authorises happens in the
/// same request.
///
/// The `retired` block is the point. Before it, an app could seal and had no
/// way to learn whether anything was actually removed: the seal's own 200
/// looks identical whether the journal shrank by a thousand lines or was
/// refused outright.
#[tokio::test]
async fn an_app_seals_through_the_append_door_and_the_journal_shrinks() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let state = with_guest(
        state_with_rail(dir.path(), &key),
        vec![Scope::Rails(NS.into())],
    );
    let post = |body: serde_json::Value| {
        let state = state.clone();
        async move {
            call(
                state,
                request(
                    "POST",
                    "/v1/rail/append",
                    LAN_PEER,
                    Some(GUEST_TOKEN),
                    Some(body),
                ),
            )
            .await
        }
    };

    for _ in 0..3 {
        let (status, body) = post(groceries()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    // An ordinary append says nothing about retention, because it retires
    // nothing — the field is absent rather than a zero that reads like a prune
    // that found nothing.
    let (_, plain) = post(groceries()).await;
    assert!(plain.get("retired").is_none(), "{plain}");

    let (status, sealed) = post(serde_json::json!({ "op": "seal" })).await;
    assert_eq!(status, StatusCode::OK, "{sealed}");
    assert_eq!(sealed["seq"], 4, "a seal is the author's next act");
    assert_eq!(sealed["retired"]["removed"], 4, "{sealed}");
    assert_eq!(sealed["retired"]["kept"], 1, "the seal itself: {sealed}");

    let (status, log) = call(
        state,
        request("GET", "/v1/rail/log", LAN_PEER, Some(GUEST_TOKEN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert_eq!(log["held"], 1, "the journal is four lines shorter: {log}");
    assert_eq!(
        log["complete"], true,
        "a compacted node is not a broken one: {}",
        log["gaps"]
    );
    // The seal is delivery, not meaning: it is on the log and no reducer sees
    // a payload for it.
    let ops = log["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert!(
        ops[0].get("payload").map(|p| p.is_null()).unwrap_or(true),
        "{log}"
    );
}
